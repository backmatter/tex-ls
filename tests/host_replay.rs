//! Replay repository transcripts through native LSP and the embedded session.
use lsp_server::{Connection, Message, Notification, Request, RequestId};
use meaning_browser::session::Session;
use serde_json::{Value, json};
use std::time::Duration;

fn request(client: &Connection, id: i32, method: &str, params: Value) -> Value {
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(id),
            method: method.to_owned(),
            params,
        }))
        .unwrap();
    loop {
        match client
            .receiver
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
        {
            Message::Response(response) if response.id == RequestId::from(id) => {
                return response
                    .response_result
                    .expect("successful replay response");
            }
            Message::Notification(_) => {}
            other => panic!("unexpected replay message: {other:?}"),
        }
    }
}

fn notify(client: &Connection, method: &str, params: Value) {
    client
        .sender
        .send(Message::Notification(Notification {
            method: method.to_owned(),
            params,
        }))
        .unwrap();
}

#[test]
fn source_lifecycle_agrees_between_hosts() {
    replay(include_str!("transcripts/source-lifecycle.json"));
}

#[test]
fn collaborative_project_agrees_between_hosts() {
    replay(include_str!("transcripts/collaborative-project.json"));
}

#[test]
fn equivalent_settings_agree_between_hosts() {
    replay(include_str!("transcripts/configuration.json"));
}

#[test]
fn nested_workspace_projects_agree_between_hosts() {
    replay(include_str!("transcripts/projects.json"));
}

#[test]
fn snapshot_features_agree_between_hosts() {
    replay(include_str!("transcripts/features.json"));
}

#[test]
fn backing_updates_stay_hidden_until_overlay_close_in_both_hosts() {
    use meaning_analysis::text::IntoSourceText;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.tex");
    let uri = meaning_protocol::path_to_uri(&path).unwrap();
    let (server, client) = Connection::memory();
    let worker = std::thread::spawn(move || meaning::lsp::serve(server).unwrap());
    request(&client, 0, "initialize", json!({"capabilities": {}}));
    notify(&client, "initialized", json!({}));
    let mut browser = Session::default();
    browser.dispatch("initialize", json!({})).unwrap();
    std::fs::write(&path, "\\section{Disk}\n").unwrap();
    browser
        .set_backing(&path, Some("\\section{Disk}\n".into_source_text()))
        .unwrap();
    let opened = json!({"textDocument": {"uri": uri, "version":0, "languageId":"latex", "text":"\\section{Editor}\n"}});
    notify(&client, "textDocument/didOpen", opened.clone());
    browser
        .dispatch("textDocument/didOpen", opened.clone())
        .unwrap();
    std::fs::write(&path, "\\section{Changed backing}\n").unwrap();
    notify(
        &client,
        "workspace/didChangeWatchedFiles",
        json!({"changes":[{"uri":uri,"type":2}]}),
    );
    browser
        .set_backing(
            &path,
            Some("\\section{Changed backing}\n".into_source_text()),
        )
        .unwrap();
    let document = json!({"textDocument":{"uri":uri}});
    let symbols = request(&client, 1, "textDocument/documentSymbol", document.clone());
    assert_eq!(
        symbols,
        browser
            .dispatch("textDocument/documentSymbol", document.clone())
            .unwrap()
    );
    assert_eq!(symbols[0]["name"], "Editor");
    notify(&client, "textDocument/didClose", document.clone());
    browser
        .dispatch("textDocument/didClose", document.clone())
        .unwrap();
    let symbols = request(
        &client,
        2,
        "workspace/symbol",
        json!({"query":"Changed backing"}),
    );
    assert_eq!(
        symbols,
        browser
            .dispatch("workspace/symbol", json!({"query":"Changed backing"}))
            .unwrap()
    );
    assert_eq!(symbols.as_array().unwrap().len(), 1);

    notify(&client, "textDocument/didOpen", opened.clone());
    browser.dispatch("textDocument/didOpen", opened).unwrap();
    std::fs::remove_file(&path).unwrap();
    notify(
        &client,
        "workspace/didChangeWatchedFiles",
        json!({"changes":[{"uri":uri,"type":3}]}),
    );
    browser.set_backing(&path, None).unwrap();
    let symbols = request(&client, 3, "textDocument/documentSymbol", document.clone());
    assert_eq!(
        symbols,
        browser
            .dispatch("textDocument/documentSymbol", document.clone())
            .unwrap()
    );
    assert_eq!(symbols[0]["name"], "Editor");
    notify(&client, "textDocument/didClose", document.clone());
    browser.dispatch("textDocument/didClose", document).unwrap();
    let symbols = request(&client, 4, "workspace/symbol", json!({"query":""}));
    assert_eq!(
        symbols,
        browser
            .dispatch("workspace/symbol", json!({"query":""}))
            .unwrap()
    );
    assert!(symbols.as_array().unwrap().is_empty());
    request(&client, 1000, "shutdown", Value::Null);
    notify(&client, "exit", Value::Null);
    worker.join().unwrap();
}

fn replay(transcript: &str) {
    let mut fixture: Value = serde_json::from_str(transcript).unwrap();
    let directory = tempfile::tempdir().unwrap();
    if let Some(config) = fixture.get("nativeConfig").and_then(Value::as_str) {
        std::fs::write(directory.path().join("meaning.toml"), config).unwrap();
        let uri = meaning_protocol::path_to_uri(directory.path()).unwrap();
        fixture = serde_json::from_str(
            &fixture
                .to_string()
                .replace("file:///architecture-settings", uri.as_str()),
        )
        .unwrap();
    }
    let events = fixture
        .as_array()
        .unwrap_or_else(|| fixture["events"].as_array().unwrap());
    let (server, client) = Connection::memory();
    let worker = std::thread::spawn(move || meaning::lsp::serve(server).unwrap());
    let mut browser = Session::default();
    request(
        &client,
        0,
        "initialize",
        fixture
            .get("initialize")
            .cloned()
            .unwrap_or_else(|| json!({"capabilities": {}})),
    );
    notify(&client, "initialized", json!({}));
    browser
        .dispatch(
            "initialize",
            fixture
                .get("initialize")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
        .unwrap();
    if let Some(settings) = fixture.get("settings") {
        assert_eq!(
            browser
                .dispatch("meaning/updateSettings", json!({"settings": settings}))
                .unwrap(),
            json!({"applied": true})
        );
    }
    for (index, event) in events.iter().enumerate() {
        let method = event["method"].as_str().unwrap();
        let params = event["params"].clone();
        let embedded = browser.dispatch(method, params.clone()).unwrap();
        if let Some(expected) = event.get("expected") {
            assert_eq!(&embedded, expected, "expected event {index}: {method}");
        }
        if event["request"].as_bool().unwrap_or(false) {
            assert_eq!(
                embedded,
                request(&client, index as i32 + 1, method, params),
                "event {index}: {method}"
            );
        } else {
            notify(&client, method, params);
        }
    }
    request(&client, 1000, "shutdown", Value::Null);
    notify(&client, "exit", Value::Null);
    worker.join().unwrap();
}
