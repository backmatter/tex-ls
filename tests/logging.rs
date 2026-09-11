//! The native logger must respect RUST_LOG without corrupting LSP stdout.
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn rust_log_controls_stderr_and_preserves_protocol_framing() {
    let messages = [
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}),
        serde_json::json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}),
        serde_json::json!({"jsonrpc":"2.0","method":"exit","params":null}),
    ];
    let input = messages.iter().fold(String::new(), |mut input, message| {
        let body = message.to_string();
        input.push_str(&format!("Content-Length: {}\r\n\r\n{}", body.len(), body));
        input
    });
    for level in ["off", "lsp_server=debug"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_meaning"))
            .arg("lsp")
            .env("RUST_LOG", level)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut remaining = std::str::from_utf8(&output.stdout).unwrap();
        let mut ids = Vec::new();
        while !remaining.is_empty() {
            let (header, body) = remaining.split_once("\r\n\r\n").unwrap();
            let length: usize = header
                .strip_prefix("Content-Length: ")
                .unwrap()
                .parse()
                .unwrap();
            let message: serde_json::Value = serde_json::from_str(&body[..length]).unwrap();
            ids.push(message["id"].clone());
            remaining = &body[length..];
        }
        assert_eq!(ids, vec![serde_json::json!(1), serde_json::json!(2)]);
        if level == "off" {
            assert!(output.stderr.is_empty());
        } else {
            assert!(String::from_utf8_lossy(&output.stderr).contains("DEBUG lsp_server"));
        }
    }
}
