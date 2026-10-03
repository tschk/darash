use darash::fetch::{self, FetchOptions, FetchReport, FETCH_MAX_BODY_BYTES};
use serde_json::json;

#[test]
fn existing_fetch_options_and_report_schema_remain_compatible() {
    // Exhaustive construction intentionally protects the existing public struct.
    let options = FetchOptions {
        method: None,
        headers: vec![],
        body: None,
        basic_auth: None,
        insecure: false,
        timeout: None,
        max_bytes: None,
    };
    assert_eq!(options.max_bytes, FetchOptions::default().max_bytes);
    assert_eq!(FETCH_MAX_BODY_BYTES, 2 * 1024 * 1024);
    let report = FetchReport::from_source("fixture.html", "<h1>Hi</h1>");
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        json!({
            "status":null, "ok":true, "url":"fixture.html", "redirected":false,
            "ms":0, "contentType":null, "bytes":11, "body":"<h1>Hi</h1>"
        })
    );
}

#[test]
fn full_document_rendering_retains_clutter_unless_extraction_is_requested() {
    let html = include_str!("fixtures/main-content.html");
    assert!(fetch::to_markdown(html).contains("Navigation"));
    assert!(fetch::to_text(html).contains("Footer"));
    let main = darash::main_content::extract_main_content(html, Default::default()).unwrap();
    assert!(!fetch::to_markdown(&main.html).contains("Navigation"));
    assert_eq!(fetch::tables(&main.html)[0].rows, [["A", "1"]]);
}

#[cfg(feature = "client")]
#[tokio::test]
async fn static_fetch_keeps_redirect_error_body_and_byte_cap_contracts() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..3 {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let length = socket.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let response = if request.starts_with("GET /redirect ") {
                "HTTP/1.1 302 Found\r\nLocation: /error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                "HTTP/1.1 404 Missing\r\nContent-Type: text/html\r\nContent-Length: 14\r\nConnection: close\r\n\r\n<h1>Oops</h1>\n"
            };
            socket.write_all(response.as_bytes()).unwrap();
        }
    });
    let report = fetch::fetch(format!("http://{address}/redirect"), &[])
        .await
        .unwrap();
    assert_eq!(report.status, Some(404));
    assert!(!report.ok && report.redirected);
    assert_eq!(report.url, format!("http://{address}/error"));
    assert_eq!(report.body, "<h1>Oops</h1>\n");
    assert_eq!(report.content_type.as_deref(), Some("text/html"));
    assert_eq!(report.bytes, report.body.len());
    let error = fetch::fetch_with(
        format!("http://{address}/error"),
        &FetchOptions {
            max_bytes: Some(3),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        darash::Error::FetchBodyTooLarge { limit: 3 }
    ));
    server.join().unwrap();
}
