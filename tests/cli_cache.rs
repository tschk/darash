#![cfg(feature = "client")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct Server {
    url: String,
    cache: PathBuf,
    requests: Arc<AtomicUsize>,
    no_store: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn new(no_store: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let no_store = Arc::new(AtomicBool::new(no_store));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let requests = requests.clone();
            let no_store = no_store.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            socket
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let mut data = [0; 4096];
                            let n = socket.read(&mut data).unwrap();
                            let request = String::from_utf8_lossy(&data[..n]);
                            let identity = request
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("authorization")
                                        .then_some(value.trim())
                                })
                                .unwrap_or("anonymous");
                            let count = requests.fetch_add(1, Ordering::Relaxed) + 1;
                            let body = format!("{identity} request-{count}");
                            let headers = if no_store.load(Ordering::Relaxed) {
                                "Cache-Control: max-age=60\r\nCache-Control: private, No-Store\r\n"
                            } else {
                                ""
                            };
                            write!(socket, "HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("server failed: {error}"),
                    }
                }
            })
        };
        Self {
            url: format!("http://{address}/page"),
            cache: std::env::temp_dir().join(format!(
                "darash-cache-test-{}-{}",
                std::process::id(),
                address.port()
            )),
            requests,
            no_store,
            stop,
            thread: Some(thread),
        }
    }

    fn fetch(&self, options: &[&str]) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_darash"))
            .env("XDG_CACHE_HOME", &self.cache)
            .args(["fetch", &self.url, "--body"])
            .args(options)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
        let _ = std::fs::remove_dir_all(&self.cache);
    }
}

#[test]
fn basic_auth_never_reads_or_writes_the_shared_cache() {
    let server = Server::new(false);
    assert_eq!(server.fetch(&[]).trim(), "anonymous request-1");
    assert_eq!(
        server.fetch(&["-u", "alice:one"]).trim(),
        "Basic YWxpY2U6b25l request-2"
    );
    assert_eq!(
        server.fetch(&["-u", "bob:two"]).trim(),
        "Basic Ym9iOnR3bw== request-3"
    );
    assert_eq!(server.fetch(&[]).trim(), "anonymous request-1");
    assert_eq!(server.requests.load(Ordering::Relaxed), 3);

    let empty_cache = Server::new(false);
    assert_eq!(
        empty_cache.fetch(&["-u", "alice:one"]).trim(),
        "Basic YWxpY2U6b25l request-1"
    );
    assert_eq!(empty_cache.fetch(&[]).trim(), "anonymous request-2");
}

#[test]
fn response_no_store_prevents_caching() {
    let server = Server::new(true);
    assert_eq!(server.fetch(&[]).trim(), "anonymous request-1");
    assert_eq!(server.fetch(&[]).trim(), "anonymous request-2");
    assert_eq!(server.requests.load(Ordering::Relaxed), 2);

    let server = Server::new(false);
    assert_eq!(server.fetch(&[]).trim(), "anonymous request-1");
    server.no_store.store(true, Ordering::Relaxed);
    assert_eq!(server.fetch(&["--fresh"]).trim(), "anonymous request-2");
    assert_eq!(server.fetch(&[]).trim(), "anonymous request-3");
}

#[test]
fn legacy_cache_entries_that_may_contain_private_data_are_not_reused() {
    let server = Server::new(false);
    let directory = server.cache.join("darash/fetch");
    std::fs::create_dir_all(&directory).unwrap();
    let report = darash::fetch::FetchReport::from_source(&server.url, "private body");
    let fetched_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let legacy = serde_json::json!({"fetched_at": fetched_at, "report": report});
    std::fs::write(
        directory.join(format!(
            "{}.json",
            darash::disk_cache::cache_key(&server.url)
        )),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    assert_eq!(server.fetch(&[]).trim(), "anonymous request-1");
}
