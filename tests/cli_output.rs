#![cfg(feature = "client")]

use std::process::Command;

fn fetch(html: &str, options: &[&str]) -> std::process::Output {
    let path = std::env::temp_dir().join(format!(
        "darash-cli-{}-{:?}.html",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&path, html).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_darash"))
        .arg("fetch")
        .arg(&path)
        .args(options)
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    output
}

#[test]
fn text_escapes_terminal_controls_in_all_html_paths() {
    let output = fetch(
        "<p>safe\x1b[31mred\x07</p><pre>code\x1b[2J\x7f</pre><p><code>inline\u{009b}31m</code></p>",
        &["--text"],
    );
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("safe") && stdout.contains("red") && stdout.contains("code"));
    assert!(
        !stdout
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t'),
        "{stdout:?}"
    );
}

#[test]
fn json_document_budget_omits_whole_blocks() {
    let html = "<p>first paragraph</p><p>second paragraph</p><p>third paragraph</p>";
    for mode in ["--md", "--text"] {
        let output = fetch(html, &[mode, "--budget", "1", "--json"]);
        assert!(output.status.success());
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["data"].as_str().unwrap().trim(), "first paragraph");
        assert_eq!(json["meta"]["omitted"], 2);
        assert_eq!(json["meta"]["count"], 1);
    }
}

#[test]
fn json_structured_output_respects_limits_and_budget() {
    let html = "<h1>title</h1><p>first paragraph</p><p>second paragraph</p><p>third paragraph</p>";
    let output = fetch(html, &["--select", "p", "--json", "--budget", "1"]);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"], serde_json::json!(["first paragraph"]));
    assert_eq!(json["meta"]["count"], 1);
    assert_eq!(json["meta"]["omitted"], 2);

    let output = fetch(
        html,
        &[
            "--select",
            "p",
            "--json-envelope",
            "--offset",
            "1",
            "--budget",
            "1",
        ],
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"], serde_json::json!(["second paragraph"]));
    assert_eq!(json["meta"]["returned"], 1);
    assert_eq!(json["meta"]["next_offset"], 2);
    assert_eq!(json["meta"]["state"], "more");

    let output = fetch(html, &["--outline", "--json", "--limit", "1"]);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"].as_array().unwrap().len(), 1);
    assert_eq!(json["meta"]["count"], 1);
    assert_eq!(json["meta"]["omitted"], 1);
}

#[test]
fn oversized_max_time_is_a_parse_error_not_a_panic() {
    for value in ["1e300", "18446744073709551616", "NaN", "-1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_darash"))
            .args(["fetch", "unused.html", "--max-time", value])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("--max-time expects seconds"));
    }
}
