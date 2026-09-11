use crate::session::Session;
use serde_json::json;

fn open(session: &mut Session, source: &str) {
    session
        .dispatch(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri":"file:///project/main.tex", "languageId":"latex", "version":0, "text":source
            }}),
        )
        .unwrap();
}

#[test]
fn invalid_settings_preserve_the_previous_complete_value() {
    let mut session = Session::default();
    open(
        &mut session,
        "A paragraph with enough words to wrap at a short configured width.\n",
    );
    let query = json!({"textDocument":{"uri":"file:///project/main.tex"}});
    assert_eq!(
        session
            .dispatch(
                "meaning/updateSettings",
                json!({"settings": {"format": {"line-width": 15}}})
            )
            .unwrap(),
        json!({"applied": true})
    );
    let valid = session
        .dispatch("textDocument/formatting", query.clone())
        .unwrap();
    assert!(!valid.as_array().unwrap().is_empty());
    for settings in [
        json!({"format": {"line-width": 0}}),
        json!({"lint": {"select": ["unknown-rule"]}}),
    ] {
        let rejected = session
            .dispatch("meaning/updateSettings", json!({"settings":settings}))
            .unwrap();
        assert_eq!(rejected["applied"], false);
        assert!(rejected["error"]["field"].is_string());
        assert_eq!(
            session
                .dispatch("textDocument/formatting", query.clone())
                .unwrap(),
            valid
        );
    }
    session
        .dispatch("meaning/updateSettings", json!({"settings":null}))
        .unwrap();
    assert_ne!(
        session.dispatch("textDocument/formatting", query).unwrap(),
        valid
    );
}

#[test]
fn source_settings_survive_reopening_and_can_return_to_project_defaults() {
    let mut session = Session::default();
    let uri = "file:///project/main.tex";
    let source = "\\begin{code}\n\\section{Hidden}\n\\end{code}\n";
    open(&mut session, source);
    let query = json!({"textDocument":{"uri":uri}});
    let original = session
        .dispatch("textDocument/documentSymbol", query.clone())
        .unwrap();
    assert!(!original.as_array().unwrap().is_empty());
    let result = session
        .dispatch(
            "meaning/updateSettings",
            json!({"uri":uri, "settings":{
                "declarations":{"environments":{"code":{"like":"lstlisting"}}}
            }}),
        )
        .unwrap();
    assert_eq!(result["applied"], true);
    assert!(
        session
            .dispatch("textDocument/documentSymbol", query.clone())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    session
        .dispatch("textDocument/didClose", query.clone())
        .unwrap();
    open(&mut session, source);
    assert!(
        session
            .dispatch("textDocument/documentSymbol", query.clone())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    session
        .dispatch("meaning/updateSettings", json!({"uri":uri,"settings":null}))
        .unwrap();
    assert_eq!(
        session
            .dispatch("textDocument/documentSymbol", query)
            .unwrap(),
        original
    );
}

#[test]
fn project_manifests_are_isolated() {
    let mut left = Session::default();
    let mut right = Session::default();
    let source = "\\input{chapter}\n";
    open(&mut left, source);
    open(&mut right, source);
    left.dispatch(
        "meaning/registerFile",
        json!({"uri":"file:///project/chapter.tex"}),
    )
    .unwrap();
    let query = json!({"textDocument":{"uri":"file:///project/main.tex"}});
    assert_eq!(
        left.dispatch("textDocument/documentLink", query.clone())
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        right
            .dispatch("textDocument/documentLink", query.clone())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    left.dispatch(
        "textDocument/didClose",
        json!({"textDocument":{"uri":"file:///project/chapter.tex"}}),
    )
    .unwrap();
    // Closing an editor overlay does not unregister an explicitly supplied file.
    assert_eq!(
        left.dispatch("textDocument/documentLink", query)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn browser_capabilities_do_not_advertise_native_integrations() {
    let mut session = Session::default();
    let initialized = session.dispatch("initialize", json!({})).unwrap();
    let capabilities = &initialized["capabilities"];
    assert!(capabilities.get("executeCommandProvider").is_none());
    assert!(capabilities.get("experimental").is_none());
    assert_eq!(capabilities["textDocumentSync"]["change"], 2);
    assert_eq!(capabilities["textDocumentSync"]["openClose"], true);
}

#[test]
fn edits_are_ordered_and_replayed_versions_cannot_mutate_source() {
    let mut session = Session::default();
    open(&mut session, "\\label{old}\n\\ref{old}\n");
    let update = json!({"textDocument":{"uri":"file:///project/main.tex","version":1},"contentChanges":[
        {"range":{"start":{"line":0,"character":7},"end":{"line":0,"character":10}},"text":"new"},
        {"range":{"start":{"line":1,"character":5},"end":{"line":1,"character":8}},"text":"new"}
    ]});
    session
        .dispatch("textDocument/didChange", update.clone())
        .unwrap();
    assert!(session.dispatch("textDocument/didChange", update).is_err());
    let definition=session.dispatch("textDocument/definition",json!({"textDocument":{"uri":"file:///project/main.tex"},"position":{"line":1,"character":6}})).unwrap();
    assert_eq!(definition.as_array().unwrap().len(), 1);
    assert_eq!(definition[0]["range"]["start"]["line"], 0);
}
