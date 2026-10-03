//! Opt-in rendering with an explicitly supplied installed Chromium executable.
//!
//! Requests are intercepted and fulfilled by a bounded, DNS-pinned HTTP client.
//! A local sink proxy blocks Chromium's other network paths. This intentionally
//! supports unauthenticated GET/HEAD pages, not logins, downloads, WebSockets,
//! worker-backed applications, or a general-purpose browsing session.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};
use url::Url;

use crate::fetch::{FetchReport, FETCH_MAX_BODY_BYTES};

/// Bounds cover startup, DNS, resources, readiness and DOM serialization together.
#[derive(Clone, Debug)]
pub struct BrowserOptions {
    pub executable: PathBuf,
    /// Overall timeout, including browser startup (default 30 seconds).
    pub timeout: Duration,
    /// Maximum response body per resource and maximum serialized DOM bytes.
    pub max_bytes: usize,
    /// Maximum aggregate resource response bytes (default 8 MiB).
    pub max_network_bytes: usize,
    /// Maximum HTTP requests, including redirects (default 128).
    pub max_requests: usize,
    /// Maximum main-frame document requests, including JS navigation (default 10).
    pub max_navigations: usize,
    /// Optional CSS selector that must exist before capture. No unbounded idle wait.
    pub wait_for: Option<String>,
    /// DOM must remain unchanged for this interval after readiness (default 250 ms).
    pub settle_time: Duration,
}

impl BrowserOptions {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            timeout: Duration::from_secs(30),
            max_bytes: FETCH_MAX_BODY_BYTES,
            max_network_bytes: 8 * 1024 * 1024,
            max_requests: 128,
            max_navigations: 10,
            wait_for: None,
            settle_time: Duration::from_millis(250),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("browser options invalid: {0}")]
    Options(String),
    #[error("browser executable unavailable or unsupported: {0}; supply an installed Chromium/Chrome executable")]
    Executable(String),
    #[error("browser fetch timed out (startup, requests and readiness share one deadline)")]
    Timeout,
    #[error("browser network policy rejected request: {0}")]
    Policy(String),
    #[error("browser fetch exceeded {0}")]
    Limit(&'static str),
    #[error("browser transport failed: {0}")]
    Transport(String),
    #[error("browser protocol failed: {0}")]
    Protocol(String),
}

fn transport(error: impl std::fmt::Display) -> BrowserError {
    BrowserError::Transport(error.to_string())
}

/// Render an unauthenticated public HTTP(S) page and return the existing report
/// contract. `body` is the serialized rendered DOM; `bytes` is its UTF-8 length.
/// HTTP error responses still produce a report with `ok == false`. Dropping this
/// future cancels HTTP work and kills/reaps the owned browser and private profile.
/// No browser installation, user profile, custom headers or credentials are used.
pub async fn fetch_rendered(
    url: impl AsRef<str>,
    options: &BrowserOptions,
) -> Result<FetchReport, BrowserError> {
    fetch_with_policy(url.as_ref(), options, NetworkPolicy::default()).await
}

async fn fetch_with_policy(
    url: &str,
    options: &BrowserOptions,
    policy: NetworkPolicy,
) -> Result<FetchReport, BrowserError> {
    if !cfg!(unix) {
        return Err(BrowserError::Options(
            "browser rendering currently requires macOS/Linux process-group cleanup".into(),
        ));
    }
    let original = policy.parse(url)?;
    if options.timeout.is_zero()
        || options.max_bytes == 0
        || options.max_network_bytes == 0
        || options.max_requests == 0
        || options.max_navigations == 0
        || options.settle_time > Duration::from_secs(5)
    {
        return Err(BrowserError::Options(
            "positive timeout/limits and settle_time <= 5 seconds required".into(),
        ));
    }
    if let Some(selector) = &options.wait_for {
        scraper::Selector::parse(selector)
            .map_err(|error| BrowserError::Options(format!("invalid wait selector: {error}")))?;
    }
    let mut process = None;
    let result = tokio::time::timeout(options.timeout, async {
        let started = Instant::now();
        policy.addresses(&original).await?;
        process = Some(BrowserProcess::launch(&options.executable).await?);
        let browser = process
            .as_mut()
            .ok_or_else(|| BrowserError::Protocol("browser ownership lost".into()))?;
        render(browser, &original, options, policy, started).await
    })
    .await;
    // Timeout returns only after cleanup. Dropping this entire future uses Drop.
    if let Some(browser) = &mut process {
        browser.close().await;
    }
    result.map_err(|_| BrowserError::Timeout)?
}

#[derive(Default)]
struct NetworkPolicy {
    // Only unit-test synthetic fixtures can override DNS; no production escape hatch.
    #[cfg(test)]
    fixtures: Vec<(String, SocketAddr)>,
}

impl NetworkPolicy {
    fn parse(&self, input: &str) -> Result<Url, BrowserError> {
        let url = Url::parse(input).map_err(|error| BrowserError::Policy(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
        {
            return Err(BrowserError::Policy(
                "only HTTP(S) URLs without credentials are supported".into(),
            ));
        }
        Ok(url)
    }

    async fn addresses(&self, url: &Url) -> Result<Vec<SocketAddr>, BrowserError> {
        let host = url
            .host_str()
            .ok_or_else(|| BrowserError::Policy("missing host".into()))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| BrowserError::Policy("missing HTTP(S) port".into()))?;
        #[cfg(test)]
        for (fixture_host, address) in &self.fixtures {
            if host == fixture_host && port == address.port() {
                return Ok(vec![*address]);
            }
        }
        let addresses: Vec<_> = match url.host() {
            Some(url::Host::Ipv4(ip)) => vec![SocketAddr::new(ip.into(), port)],
            Some(url::Host::Ipv6(ip)) => vec![SocketAddr::new(ip.into(), port)],
            _ => tokio::net::lookup_host((host, port))
                .await
                .map_err(transport)?
                .collect(),
        };
        if addresses.is_empty() || addresses.iter().any(|address| !is_public(address.ip())) {
            return Err(BrowserError::Policy(
                "non-public or mixed public/private address resolution".into(),
            ));
        }
        Ok(addresses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn public_address_policy_rejects_special_use_and_mapped_addresses() {
        for ip in [
            "127.0.0.1",
            "0.0.0.0",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "::ffff:8.8.8.8",
            "2001:db8::1",
            "2001::1",
            "2002:7f00:1::",
            "3fff::1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "accepted {ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "rejected {ip}");
        }
    }

    #[tokio::test]
    async fn policy_rejects_credentials_schemes_and_numeric_loopback_before_launch() {
        let options = BrowserOptions::new("/missing-browser");
        for url in [
            "file:///etc/passwd",
            "ftp://example.test",
            "data:text/html,hello",
            "http://user:password@example.test",
            "http://127.0.0.1",
            "http://2130706433",
            "http://[::1]",
        ] {
            assert!(
                matches!(
                    fetch_rendered(url, &options).await,
                    Err(BrowserError::Policy(_))
                ),
                "accepted {url}"
            );
        }
        let mut options = options;
        options.timeout = Duration::ZERO;
        assert!(matches!(
            fetch_rendered("https://8.8.8.8", &options).await,
            Err(BrowserError::Options(_))
        ));
    }

    struct Fixture {
        address: SocketAddr,
        requests: Arc<Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Fixture {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = requests.clone();
            let task = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let captured = captured.clone();
                    tokio::spawn(async move {
                        let mut buffer = vec![0; 16384];
                        let length = socket.read(&mut buffer).await.unwrap_or(0);
                        let request = String::from_utf8_lossy(&buffer[..length]).into_owned();
                        let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                        captured.lock().unwrap().push(request);
                        let (status, headers, body) = match path.as_str() {
                            "/" => (200, "Content-Type: text/html\r\n".into(), include_str!("../tests/fixtures/rendered.html").to_owned()),
                            "/app.js" => (200, "Content-Type: text/javascript\r\n".into(), include_str!("../tests/fixtures/app.js").to_owned()),
                            "/404" => (404, "Content-Type: text/html\r\n".into(), "<main>Missing page</main>".into()),
                            "/redirect" => (302, "Location: /\r\n".into(), String::new()),
                            "/loop" => (302, "Location: /loop\r\n".into(), String::new()),
                            "/private" => (302, format!("Location: http://127.0.0.1:{}/secret\r\n", address.port()), String::new()),
                            "/subresource" => (200, "Content-Type: text/html\r\n".into(), format!("<main>Hello</main><script src='http://127.0.0.1:{}/secret'></script>", address.port())),
                            "/big" => (200, "Content-Type: text/html\r\n".into(), "x".repeat(4096)),
                            "/grow" => (200, "Content-Type: text/html\r\n".into(), "<script>document.write('<main>'+'x'.repeat(4096)+'</main>')</script>".into()),
                            "/slow" => { tokio::time::sleep(Duration::from_secs(3)).await; (200, String::new(), "Slow".into()) },
                            "/download" => (200, "Content-Disposition: attachment; filename=download.html\r\n".into(), "<main>Download</main>".into()),
                            "/auth" => (200, "Content-Type: text/html\r\nSet-Cookie: secret=value\r\n".into(), "<script>fetch('/auth-hop',{headers:{Authorization:'secret'}}).then(()=>document.body.innerHTML='<main id=ready>Done</main>')</script>".into()),
                            "/auth-hop" => (302, format!("Location: http://other.fixture.test:{}/capture\r\n", address.port()), String::new()),
                            "/unsupported" => (200, "Content-Type: text/html\r\n".into(), format!("<script>new WebSocket('ws://127.0.0.1:{0}/secret'); new Worker('/worker.js'); navigator.serviceWorker.register('/sw.js').catch(()=>{{}}); window.open('http://127.0.0.1:{0}/secret'); setTimeout(()=>document.body.innerHTML='<main id=ready>Done</main>',300)</script>", address.port())),
                            "/worker.js" | "/sw.js" => (200, "Content-Type: text/javascript\r\n".into(), format!("fetch('http://127.0.0.1:{}/secret')", address.port())),
                            "/capture" => (200, "Access-Control-Allow-Origin: *\r\nContent-Type: text/plain\r\n".into(), "Captured".into()),
                            _ => (200, "Content-Type: text/html\r\n".into(), "<main>Fixture</main>".into()),
                        };
                        let response = format!("HTTP/1.1 {status} Fixture\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                        let _ = socket.write_all(response.as_bytes()).await;
                    });
                }
            });
            Self {
                address,
                requests,
                task,
            }
        }

        fn url(&self, path: &str) -> String {
            format!("http://fixture.test:{}{path}", self.address.port())
        }

        async fn fetch(
            &self,
            path: &str,
            options: &BrowserOptions,
        ) -> Result<FetchReport, BrowserError> {
            let policy = NetworkPolicy {
                fixtures: vec![
                    ("fixture.test".into(), self.address),
                    ("other.fixture.test".into(), self.address),
                ],
            };
            fetch_with_policy(&self.url(path), options, policy).await
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn browser_options() -> BrowserOptions {
        BrowserOptions::new(
            std::env::var_os("DARASH_TEST_BROWSER").expect(
                "set DARASH_TEST_BROWSER to an installed Chromium executable; no downloads",
            ),
        )
    }

    #[tokio::test]
    #[ignore = "requires explicit installed Chromium; run with DARASH_TEST_BROWSER and --ignored"]
    async fn chromium_js_static_path_status_redirects_and_extraction() {
        let fixture = Fixture::start().await;
        // Legacy static fetching still reports the initial shell without executing JS.
        let static_report = crate::fetch::fetch(format!("http://{}/", fixture.address), &[])
            .await
            .unwrap();
        assert!(static_report.body.contains("Loading shell"));
        assert!(!static_report.body.contains("Rendered guide"));
        let mut options = browser_options();
        options.wait_for = Some("#ready".into());
        let report = fixture.fetch("/redirect", &options).await.unwrap();
        assert_eq!(report.status, Some(200));
        assert!(report.ok && report.redirected);
        assert_eq!(report.url, fixture.url("/"));
        assert_eq!(report.bytes, report.body.len());
        let main = crate::main_content::extract_main_content(
            &report.body,
            crate::main_content::MainContentFallback::Error,
        )
        .unwrap();
        let markdown = crate::fetch::to_markdown(&main.html);
        for content in [
            "# Rendered guide",
            "[a reference](/reference)",
            "`cargo test`",
            "| A | 1 |",
        ] {
            assert!(markdown.contains(content), "missing {content}");
        }
        assert!(
            !markdown.contains("Navigation clutter") && !markdown.contains("Hidden CSS clutter")
        );
        assert_eq!(
            crate::fetch::select_texts(&report.body, "h1").unwrap(),
            ["Rendered guide"]
        );
        assert_eq!(crate::fetch::tables(&report.body).len(), 1);
        options.wait_for = None;
        let report = fixture.fetch("/404", &options).await.unwrap();
        assert_eq!(report.status, Some(404));
        assert!(!report.ok && report.body.contains("Missing page"));
    }

    #[tokio::test]
    #[ignore = "requires explicit installed Chromium; run with DARASH_TEST_BROWSER and --ignored"]
    async fn chromium_limits_timeouts_and_network_policy() {
        let fixture = Fixture::start().await;
        let mut options = browser_options();
        for path in ["/private", "/subresource", "/download"] {
            assert!(
                matches!(
                    fixture.fetch(path, &options).await,
                    Err(BrowserError::Policy(_))
                ),
                "accepted {path}"
            );
        }
        assert!(!fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.contains("/secret")));
        options.max_navigations = 2;
        assert!(matches!(
            fixture.fetch("/loop", &options).await,
            Err(BrowserError::Limit(_))
        ));
        options.max_navigations = 10;
        options.max_bytes = 1024;
        for path in ["/big", "/grow"] {
            assert!(
                matches!(
                    fixture.fetch(path, &options).await,
                    Err(BrowserError::Limit(_))
                ),
                "accepted {path}"
            );
        }
        options.max_bytes = FETCH_MAX_BODY_BYTES;
        options.max_network_bytes = 10;
        assert!(matches!(
            fixture.fetch("/", &options).await,
            Err(BrowserError::Limit(_))
        ));
        options.max_network_bytes = 8 * 1024 * 1024;
        options.max_requests = 1;
        assert!(matches!(
            fixture.fetch("/", &options).await,
            Err(BrowserError::Limit(_))
        ));
        options.max_requests = 128;
        options.timeout = Duration::from_secs(2);
        options.wait_for = Some("#never".into());
        assert!(matches!(
            fixture.fetch("/404", &options).await,
            Err(BrowserError::Timeout)
        ));
        options.wait_for = None;
        assert!(matches!(
            fixture.fetch("/slow", &options).await,
            Err(BrowserError::Timeout)
        ));
    }

    #[tokio::test]
    #[ignore = "requires explicit installed Chromium; run with DARASH_TEST_BROWSER and --ignored"]
    async fn chromium_cleanup_on_cancellation_and_missing_executable() {
        let options = browser_options();
        let mut process = BrowserProcess::launch(&options.executable).await.unwrap();
        process.endpoint().await.unwrap();
        let pid = process.child.as_ref().unwrap().id().unwrap();
        let profile = process.profile.as_ref().unwrap().path().to_owned();
        drop(process); // Same RAII path as cancellation or the overall timeout.
        for _ in 0..100 {
            if !profile.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!profile.exists(), "temporary profile survived cancellation");
        #[cfg(unix)]
        // SAFETY: signal 0 only checks existence of the owned process group.
        assert_eq!(
            unsafe { libc::kill(-(pid as i32), 0) },
            -1,
            "browser process group survived cancellation"
        );
        let fixture = Fixture::start().await;
        assert!(matches!(
            fixture
                .fetch("/", &BrowserOptions::new("/missing/chromium"))
                .await,
            Err(BrowserError::Executable(_))
        ));
    }

    #[tokio::test]
    #[ignore = "requires explicit installed Chromium; run with DARASH_TEST_BROWSER and --ignored"]
    async fn chromium_credentials_and_unintercepted_paths_do_not_reach_destinations() {
        let fixture = Fixture::start().await;
        let mut options = browser_options();
        options.wait_for = Some("#ready".into());
        fixture.fetch("/auth", &options).await.unwrap();
        let requests = fixture.requests.lock().unwrap().clone();
        assert!(requests.iter().any(|request| request.contains("/capture")));
        for request in requests {
            let lower = request.to_ascii_lowercase();
            assert!(
                !lower.contains("authorization:") && !lower.contains("cookie:"),
                "credentials forwarded: {request}"
            );
        }
        let result = fixture.fetch("/unsupported", &options).await;
        // Unsupported paths may cause an explicit policy failure. In either case,
        // neither a popup, worker, service worker nor socket may hit the trap URL.
        assert!(result.is_ok() || matches!(result, Err(BrowserError::Policy(_))));
        assert!(!fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.contains("/secret")));
    }
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && (c == 0 || c == 2))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            // Only global unicast; reject mapped addresses and special-use space.
            let segments = ip.segments();
            (segments[0] & 0xe000) == 0x2000
                && !(segments[0] == 0x2001 && segments[1] < 0x200)
                && !(segments[0] == 0x2001 && segments[1] == 0xdb8)
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] < 0x1000)
        }
    }
}

struct BrowserProcess {
    child: Option<Child>,
    profile: Option<tempfile::TempDir>,
    // Own the sink port so it can never reach an unrelated local proxy/service.
    _sink: TcpListener,
}

impl BrowserProcess {
    async fn launch(executable: &Path) -> Result<Self, BrowserError> {
        let executable = std::fs::canonicalize(executable)
            .map_err(|error| BrowserError::Executable(error.to_string()))?;
        let version = Command::new(&executable)
            .arg("--version")
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|error| BrowserError::Executable(error.to_string()))?;
        let version_text = String::from_utf8_lossy(&version.stdout);
        if !version.status.success()
            || !(version_text.contains("Chromium") || version_text.contains("Google Chrome"))
        {
            return Err(BrowserError::Executable(
                "executable did not identify as Chromium/Google Chrome".into(),
            ));
        }
        let profile = tempfile::Builder::new()
            .prefix("darash-browser-")
            .tempdir()
            .map_err(transport)?;
        let sink = TcpListener::bind("127.0.0.1:0").await.map_err(transport)?;
        let mut command = Command::new(executable);
        #[cfg(unix)]
        command.process_group(0);
        let child = command
            .args([
                "--headless=new",
                "--remote-debugging-port=0",
                "--remote-debugging-address=127.0.0.1",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-sync",
                "--disable-extensions",
                "--disable-quic",
                "--disable-features=MediaRouter,OptimizationHints",
                "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
                "--proxy-bypass-list=<-loopback>",
                "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1",
            ])
            .arg(format!(
                "--proxy-server=http://{}",
                sink.local_addr().map_err(transport)?
            ))
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| BrowserError::Executable(error.to_string()))?;
        Ok(Self {
            child: Some(child),
            profile: Some(profile),
            _sink: sink,
        })
    }

    async fn endpoint(&mut self) -> Result<String, BrowserError> {
        let path = self
            .profile
            .as_ref()
            .ok_or_else(|| BrowserError::Protocol("browser profile closed".into()))?
            .path()
            .join("DevToolsActivePort");
        loop {
            if self
                .child
                .as_mut()
                .ok_or_else(|| BrowserError::Protocol("browser process closed".into()))?
                .try_wait()
                .map_err(transport)?
                .is_some()
            {
                return Err(BrowserError::Executable(
                    "Chromium exited during startup".into(),
                ));
            }
            if let Ok(port_file) = tokio::fs::read_to_string(&path).await {
                let mut lines = port_file.lines();
                if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                    if let Ok(port) = port.parse::<u16>() {
                        if path.starts_with("/devtools/browser/") {
                            return Ok(format!("ws://127.0.0.1:{port}{path}"));
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn close(&mut self) {
        if let Some(child) = self.child.as_mut() {
            kill_process_group(child);
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        self.child.take();
        self.profile.take();
    }
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            kill_process_group(&child);
            let _ = child.start_kill();
            let profile = self.profile.take();
            // Keep the profile until Chromium has exited, including cancellation.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = child.wait().await;
                    drop(profile);
                });
            }
        }
    }
}

fn kill_process_group(child: &Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: launch creates a new process group whose ID is the owned child
        // PID. A negative PID targets that group, never the caller's process group.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = child;
}

struct Bridge<'a> {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    session: Option<String>,
    frame: String,
    options: &'a BrowserOptions,
    policy: NetworkPolicy,
    requests: usize,
    navigations: usize,
    network_bytes: usize,
    status: Option<u16>,
    content_type: Option<String>,
    response_url: Option<Url>,
}

impl Bridge<'_> {
    async fn send(&mut self, method: &str, params: Value) -> Result<u64, BrowserError> {
        self.next_id += 1;
        let mut message = json!({"id": self.next_id, "method": method, "params": params});
        if let Some(session) = &self.session {
            message["sessionId"] = json!(session);
        }
        self.socket
            .send(Message::Text(message.to_string().into()))
            .await
            .map_err(transport)?;
        Ok(self.next_id)
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, BrowserError> {
        let id = self.send(method, params).await?;
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or_else(|| BrowserError::Protocol("Chromium closed the connection".into()))?
                .map_err(transport)?;
            let Message::Text(text) = message else {
                continue;
            };
            let value: Value = serde_json::from_str(&text).map_err(transport)?;
            if let Some(error) = value.get("error") {
                return Err(BrowserError::Protocol(error.to_string()));
            }
            if value["id"].as_u64() == Some(id) {
                return Ok(value["result"].clone());
            }
            if value["method"] == "Fetch.requestPaused" {
                self.request(&value["params"]).await?;
            }
        }
    }

    async fn request(&mut self, event: &Value) -> Result<(), BrowserError> {
        self.requests += 1;
        if self.requests > self.options.max_requests {
            return Err(BrowserError::Limit("request limit"));
        }
        let main = event["resourceType"] == "Document"
            && event["frameId"].as_str() == Some(self.frame.as_str());
        if main {
            self.navigations += 1;
            if self.navigations > self.options.max_navigations {
                return Err(BrowserError::Limit("navigation/redirect limit"));
            }
        }
        let method = event["request"]["method"].as_str().unwrap_or_default();
        // Unsupported resource methods and subframes are explicitly refused.
        if !matches!(method, "GET" | "HEAD") || (event["resourceType"] == "Document" && !main) {
            self.send(
                "Fetch.failRequest",
                json!({"requestId":event["requestId"], "errorReason":"BlockedByClient"}),
            )
            .await?;
            return Ok(());
        }
        let url = self
            .policy
            .parse(event["request"]["url"].as_str().unwrap_or_default())?;
        let addresses = self.policy.addresses(&url).await?;
        let host = url
            .host_str()
            .ok_or_else(|| BrowserError::Policy("missing resource host".into()))?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addresses)
            .timeout(self.options.timeout)
            .user_agent(concat!("darash-browser/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(transport)?;
        let mut request = client.request(
            reqwest::Method::from_bytes(method.as_bytes()).map_err(transport)?,
            url.clone(),
        );
        // Never forward Cookie, Authorization, proxy auth, user headers or referrers.
        for name in ["Accept", "Origin"] {
            if let Some(value) = event["request"]["headers"][name].as_str() {
                request = request.header(name, value);
            }
        }
        let mut response = request.send().await.map_err(transport)?;
        let status = response.status().as_u16();
        if let Some(location) = response.headers().get(reqwest::header::LOCATION) {
            let location = location.to_str().map_err(transport)?;
            let redirect = url.join(location).map_err(transport)?;
            self.policy.parse(redirect.as_str())?;
            self.policy.addresses(&redirect).await?;
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if main
            && response
                .headers()
                .get(reqwest::header::CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(';')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .eq_ignore_ascii_case("attachment")
                })
        {
            return Err(BrowserError::Policy(
                "document downloads are unsupported".into(),
            ));
        }
        let headers: Vec<_> = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                // Transfer/body framing is replaced by Fetch.fulfillRequest. Cookies
                // and downloads are unsupported; neither creates a persistent session.
                if matches!(
                    name.as_str(),
                    "content-length"
                        | "transfer-encoding"
                        | "connection"
                        | "set-cookie"
                        | "content-disposition"
                ) {
                    return None;
                }
                value
                    .to_str()
                    .ok()
                    .map(|value| json!({"name":name.as_str(), "value":value}))
            })
            .collect();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            if bytes.len().saturating_add(chunk.len()) > self.options.max_bytes {
                return Err(BrowserError::Limit("resource body limit"));
            }
            self.network_bytes = self.network_bytes.saturating_add(chunk.len());
            if self.network_bytes > self.options.max_network_bytes {
                return Err(BrowserError::Limit("aggregate network body limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if main {
            self.status = Some(status);
            self.content_type = content_type;
            self.response_url = Some(url);
        }
        self.send("Fetch.fulfillRequest", json!({"requestId":event["requestId"], "responseCode":status, "responseHeaders":headers, "body":STANDARD.encode(bytes)})).await?;
        Ok(())
    }
}

async fn render(
    process: &mut BrowserProcess,
    original: &Url,
    options: &BrowserOptions,
    policy: NetworkPolicy,
    started: Instant,
) -> Result<FetchReport, BrowserError> {
    let endpoint = process.endpoint().await?;
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(
            options.max_bytes.saturating_mul(8).saturating_add(65536),
        ))
        .max_frame_size(Some(
            options.max_bytes.saturating_mul(8).saturating_add(65536),
        ));
    let (socket, _) = tokio_tungstenite::connect_async_with_config(endpoint, Some(config), false)
        .await
        .map_err(transport)?;
    let mut bridge = Bridge {
        socket,
        next_id: 0,
        session: None,
        frame: String::new(),
        options,
        policy,
        requests: 0,
        navigations: 0,
        network_bytes: 0,
        status: None,
        content_type: None,
        response_url: None,
    };
    bridge
        .call("Browser.setDownloadBehavior", json!({"behavior":"deny"}))
        .await?;
    let target = bridge
        .call("Target.createTarget", json!({"url":"about:blank"}))
        .await?;
    let session = bridge
        .call(
            "Target.attachToTarget",
            json!({"targetId":target["targetId"], "flatten":true}),
        )
        .await?;
    bridge.session = session["sessionId"].as_str().map(str::to_owned);
    if bridge.session.is_none() {
        return Err(BrowserError::Protocol("missing target session".into()));
    }
    bridge.call("Page.enable", json!({})).await?;
    bridge.call("Network.enable", json!({})).await?;
    bridge
        .call("Network.setCacheDisabled", json!({"cacheDisabled":true}))
        .await?;
    bridge
        .call("Network.setBypassServiceWorker", json!({"bypass":true}))
        .await?;
    bridge
        .call(
            "Network.setBlockedURLs",
            json!({"urls":["file:*", "ftp:*", "ws:*", "wss:*", "data:*", "blob:*"]}),
        )
        .await?;
    let frame = bridge.call("Page.getFrameTree", json!({})).await?;
    bridge.frame = frame["frameTree"]["frame"]["id"]
        .as_str()
        .ok_or_else(|| BrowserError::Protocol("missing main frame".into()))?
        .to_owned();
    bridge
        .call(
            "Fetch.enable",
            json!({"patterns":[{"urlPattern":"*", "requestStage":"Request"}]}),
        )
        .await?;
    let navigation = bridge
        .call("Page.navigate", json!({"url":original.as_str()}))
        .await?;
    if let Some(error) = navigation["errorText"].as_str() {
        return Err(BrowserError::Protocol(error.into()));
    }
    let selector = serde_json::to_string(&options.wait_for).map_err(transport)?;
    let expression = format!(
        r#"(() => {{
        const selector = {selector};
        if (document.readyState === 'loading' || !document.documentElement || (selector && !document.querySelector(selector))) return null;
        const root = document.documentElement.cloneNode(true);
        const live = document.documentElement.querySelectorAll('*');
        const copies = root.querySelectorAll('*');
        for (let i = 0; i < live.length; i++) {{
            const style = getComputedStyle(live[i]);
            if (style.display === 'none' || style.visibility === 'hidden') copies[i].setAttribute('hidden', '');
        }}
        const html = root.outerHTML;
        if (new TextEncoder().encode(html).length > {}) return {{tooLarge:true}};
        return {{html, url:location.href}};
    }})()"#,
        options.max_bytes
    );
    let mut previous = String::new();
    let mut stable_since = Instant::now();
    loop {
        let snapshot = bridge
            .call(
                "Runtime.evaluate",
                json!({"expression":expression, "returnByValue":true}),
            )
            .await?;
        if snapshot.get("exceptionDetails").is_some() {
            return Err(BrowserError::Protocol(
                "readiness/snapshot evaluation failed".into(),
            ));
        }
        let value = &snapshot["result"]["value"];
        if value["tooLarge"] == true {
            return Err(BrowserError::Limit("rendered DOM byte limit"));
        }
        if let (Some(html), Some(final_url)) = (value["html"].as_str(), value["url"].as_str()) {
            if html != previous {
                previous = html.to_owned();
                stable_since = Instant::now();
            }
            if stable_since.elapsed() >= options.settle_time {
                let final_url = bridge.policy.parse(final_url)?;
                bridge.policy.addresses(&final_url).await?;
                // Reject a history-rewritten URL that no longer describes the fetched origin.
                if bridge
                    .response_url
                    .as_ref()
                    .is_none_or(|url| url.origin() != final_url.origin())
                {
                    return Err(BrowserError::Policy(
                        "final document has no matching HTTP response".into(),
                    ));
                }
                let status = bridge
                    .status
                    .ok_or_else(|| BrowserError::Protocol("no HTTP document response".into()))?;
                return Ok(FetchReport {
                    status: Some(status),
                    ok: (200..300).contains(&status),
                    redirected: final_url != *original,
                    url: final_url.into(),
                    ms: started.elapsed().as_millis() as u64,
                    content_type: bridge.content_type,
                    bytes: html.len(),
                    body: html.to_owned(),
                });
            }
        } else {
            stable_since = Instant::now();
            previous.clear();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
