//! Replay repository transcripts through native LSP and the embedded session.
use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::{Value, json};
use std::time::Duration;
use tex_ls_browser::session::Session;

fn request(client: &Connection, id: i32, method: &str, params: Value) -> Value {
    // Native background acquisition/watching can invalidate a captured read.
    // Retry that explicit temporary response as an editor would; compare only
    // successful language results, never suppress a different error or result.
    for _ in 0..8 {
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(id),
                method: method.to_owned(),
                params: params.clone(),
            }))
            .unwrap();
        loop {
            match client
                .receiver
                .recv_timeout(Duration::from_secs(30))
                .unwrap()
            {
                Message::Response(response) if response.id == RequestId::from(id) => {
                    match response.response_result {
                        Ok(result) => return result,
                        Err(error)
                            if error.code == -32801
                                || (matches!(
                                    method,
                                    "textDocument/diagnostic" | "workspace/diagnostic"
                                ) && error.code == -32802
                                    && error
                                        .data
                                        .as_ref()
                                        .and_then(|data| data.get("retriggerRequest"))
                                        .and_then(serde_json::Value::as_bool)
                                        != Some(false)) =>
                        {
                            break;
                        }
                        Err(error) => panic!("replay {method}: {error:?}"),
                    }
                }
                Message::Notification(_) => {}
                other => panic!("unexpected replay message: {other:?}"),
            }
        }
    }
    panic!("replay {method} repeatedly invalidated");
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
fn review_filename_edits_agree_between_hosts() {
    replay(include_str!("transcripts/review-filenames-utf-8.json"));
    replay(include_str!("transcripts/review-filenames-utf-16.json"));
}

#[test]
fn review_multicite_and_accent_edits_agree_between_hosts() {
    replay(include_str!("transcripts/review-semantic-utf-8.json"));
    replay(include_str!("transcripts/review-semantic-utf-16.json"));
}

#[test]
fn review_regressions_agree_between_hosts() {
    replay(include_str!("transcripts/review-regressions.json"));
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
    use tex_ls_analysis::text::IntoSourceText;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.tex");
    let uri = tex_ls_protocol::path_to_uri(&path).unwrap();
    let (server, client) = Connection::memory();
    let worker = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());
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

// Rewrite parsed strings rather than serialized JSON: Windows paths contain
// backslashes that must be escaped by serde, including URI keys in edit maps.
fn map_fixture_strings(value: &mut Value, map: &impl Fn(&str) -> String) {
    match value {
        Value::String(text) => *text = map(text),
        Value::Array(items) => {
            for item in items {
                map_fixture_strings(item, map);
            }
        }
        Value::Object(items) => {
            *items = std::mem::take(items)
                .into_iter()
                .map(|(key, mut value)| {
                    map_fixture_strings(&mut value, map);
                    (map(&key), value)
                })
                .collect();
        }
        _ => {}
    }
}

fn replay(transcript: &str) {
    let mut fixture: Value = serde_json::from_str(transcript).unwrap();
    let directory = tempfile::tempdir().unwrap();
    if let Some(config) = fixture.get("nativeConfig").and_then(Value::as_str) {
        std::fs::write(directory.path().join("tex-ls.toml"), config).unwrap();
        let uri = tex_ls_protocol::path_to_uri(directory.path()).unwrap();
        map_fixture_strings(&mut fixture, &|text| {
            if let Some(suffix) = text.strip_prefix("/architecture-settings") {
                return directory
                    .path()
                    .join(suffix.trim_start_matches('/'))
                    .components()
                    .collect::<std::path::PathBuf>()
                    .to_string_lossy()
                    .into_owned();
            }
            text.replace("file:///architecture-settings", uri.as_str())
                .replace(
                    "/architecture-settings",
                    &directory.path().to_string_lossy(),
                )
        });
    }
    #[cfg(windows)]
    map_fixture_strings(&mut fixture, &|text| {
        if let Some(path) = text.strip_prefix("file:///")
            && path.as_bytes().get(1) != Some(&b':')
        {
            return format!("file:///C:/{path}");
        }
        text.to_owned()
    });
    let events = fixture
        .as_array()
        .unwrap_or_else(|| fixture["events"].as_array().unwrap());
    let (server, client) = Connection::memory();
    let worker = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());
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
                .dispatch("tex-ls/updateSettings", json!({"settings": settings}))
                .unwrap(),
            json!({"applied": true})
        );
    }
    for (index, event) in events.iter().enumerate() {
        if let Some(files) = event.get("files").and_then(Value::as_object) {
            let mut changes = Vec::new();
            for (name, text) in files {
                let path = directory.path().join(name);
                let existed = path.exists();
                let kind = if let Some(text) = text.as_str() {
                    std::fs::write(&path, text).unwrap();
                    if existed { 2 } else { 1 }
                } else {
                    if existed {
                        std::fs::remove_file(&path).unwrap();
                    }
                    3
                };
                changes.push(
                    json!({"uri": tex_ls_protocol::path_to_uri(&path).unwrap(), "type": kind}),
                );
            }
            // Browser batches publish explicit observations; the native equivalent
            // is a disk mutation followed by a watcher event before the next read.
            notify(
                &client,
                "workspace/didChangeWatchedFiles",
                json!({"changes": changes}),
            );
        }
        let method = event["method"].as_str().unwrap();
        let params = event["params"].clone();
        let embedded = browser.dispatch(method, params.clone()).unwrap();
        if let Some(expected) = event.get("expected") {
            assert_eq!(&embedded, expected, "expected event {index}: {method}");
        }
        if event["embeddedOnly"].as_bool().unwrap_or(false) {
            continue;
        }
        if event["request"].as_bool().unwrap_or(false) {
            assert_eq!(
                language_result(embedded),
                language_result(request(&client, index as i32 + 1, method, params)),
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

#[test]
fn explicit_external_inputs_agree_with_native_acquisition() {
    replay(include_str!("transcripts/external-inputs.json"));
}

// Applicability tokens identify host-local lifetimes, not language semantics.
fn language_result(mut value: Value) -> Value {
    match &mut value {
        Value::Object(object) => {
            object.remove("sourceRevision");
            object.remove("storageEpoch");
            for value in object.values_mut() {
                *value = language_result(value.take());
            }
        }
        Value::Array(values) => {
            for value in values {
                *value = language_result(value.take());
            }
        }
        _ => {}
    }
    value
}

#[test]
fn standard_file_operations_agree_between_hosts() {
    replay(include_str!("transcripts/file-renames.json"));
}

#[test]
fn bibliography_editing_agrees_in_both_encodings() {
    replay(include_str!("transcripts/bib-editing-utf-8.json"));
    replay(include_str!("transcripts/bib-editing-utf-16.json"));
}

#[test]
fn completion_and_root_views_agree_between_hosts() {
    replay(include_str!("transcripts/root-views.json"));
    replay(include_str!("transcripts/completion-edits-utf-8.json"));
    replay(include_str!("transcripts/completion-edits-utf-16.json"));
    replay(include_str!("transcripts/completion-ranking.json"));
}

#[test]
fn compiler_diagnostics_agree_between_hosts() {
    replay(include_str!("transcripts/compiler-diagnostics.json"));
}

#[test]
fn fix_all_agrees_between_hosts() {
    replay(include_str!("transcripts/fix-all.json"));
}

#[test]
fn outline_and_hints_agree_between_hosts() {
    replay(include_str!("transcripts/outline-hints.json"));
}

#[test]
fn minimal_formatting_agrees_between_hosts() {
    replay(include_str!("transcripts/minimal-format-utf-8.json"));
    replay(include_str!("transcripts/minimal-format-utf-16.json"));
}
