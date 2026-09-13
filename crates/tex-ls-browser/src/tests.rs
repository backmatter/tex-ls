use crate::session::Session;
use serde_json::json;

#[test]
fn workspace_diagnostics_reject_malformed_parameters() {
    let mut session = running_session();
    for params in [
        json!(null),
        json!({}),
        json!({"previousResultIds":"wrong"}),
        json!({"previousResultIds":[{"uri":fixture_uri!("/main.tex")}]}),
        json!({"previousResultIds":[],"partialResultToken":{}}),
        json!({"previousResultIds":[],"workDoneToken":[]}),
        json!({"previousResultIds":[],"identifier":42}),
    ] {
        assert!(
            session
                .dispatch("workspace/diagnostic", params.clone())
                .is_err(),
            "{params}"
        );
    }
    for token in [json!(7), json!("batch")] {
        assert_eq!(
            session
                .dispatch(
                    "workspace/diagnostic",
                    json!({"previousResultIds":[],"partialResultToken":token})
                )
                .unwrap(),
            json!({"items":[]})
        );
    }
}

fn open(session: &mut Session, source: &str) {
    session
        .dispatch(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri":fixture_uri!("/project/main.tex"), "languageId":"latex", "version":0, "text":source
            }}),
        )
        .unwrap();
}

#[test]
fn invalid_settings_preserve_the_previous_complete_value() {
    let mut session = running_session();
    open(
        &mut session,
        "A paragraph with enough words to wrap at a short configured width.\n",
    );
    let query = json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}});
    assert_eq!(
        session
            .dispatch(
                "tex-ls/updateSettings",
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
            .dispatch("tex-ls/updateSettings", json!({"settings":settings}))
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
        .dispatch("tex-ls/updateSettings", json!({"settings":null}))
        .unwrap();
    assert_ne!(
        session.dispatch("textDocument/formatting", query).unwrap(),
        valid
    );
}

#[test]
fn source_settings_survive_reopening_and_can_return_to_project_defaults() {
    let mut session = running_session();
    let uri = fixture_uri!("/project/main.tex");
    let source = "\\begin{code}\n\\section{Hidden}\n\\end{code}\n";
    open(&mut session, source);
    let query = json!({"textDocument":{"uri":uri}});
    let original = session
        .dispatch("textDocument/documentSymbol", query.clone())
        .unwrap();
    assert!(!original.as_array().unwrap().is_empty());
    let result = session
        .dispatch(
            "tex-ls/updateSettings",
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
        .dispatch("tex-ls/updateSettings", json!({"uri":uri,"settings":null}))
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
    let mut left = running_session();
    let mut right = running_session();
    let source = "\\input{chapter}\n";
    open(&mut left, source);
    open(&mut right, source);
    left.dispatch(
        "tex-ls/registerFile",
        json!({"uri":fixture_uri!("/project/chapter.tex")}),
    )
    .unwrap();
    let query = json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}});
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
        json!({"textDocument":{"uri":fixture_uri!("/project/chapter.tex")}}),
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
    assert_eq!(
        capabilities["executeCommandProvider"]["commands"],
        json!(["tex-ls.inspectProject"])
    );
    assert!(capabilities.get("experimental").is_none());
    assert_eq!(capabilities["textDocumentSync"]["change"], 2);
    assert_eq!(capabilities["textDocumentSync"]["openClose"], true);
}

#[test]
fn edits_are_ordered_and_replayed_versions_cannot_mutate_source() {
    let mut session = running_session();
    open(&mut session, "\\label{old}\n\\ref{old}\n");
    let update = json!({"textDocument":{"uri":fixture_uri!("/project/main.tex"),"version":1},"contentChanges":[
        {"range":{"start":{"line":0,"character":7},"end":{"line":0,"character":10}},"text":"new"},
        {"range":{"start":{"line":1,"character":5},"end":{"line":1,"character":8}},"text":"new"}
    ]});
    session
        .dispatch("textDocument/didChange", update.clone())
        .unwrap();
    assert!(session.dispatch("textDocument/didChange", update).is_err());
    let definition=session.dispatch("textDocument/definition",json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")},"position":{"line":1,"character":6}})).unwrap();
    assert_eq!(definition.as_array().unwrap().len(), 1);
    assert_eq!(definition[0]["range"]["start"]["line"], 0);
}

fn publish(session: &mut Session, kind: &str, inputs: serde_json::Value) {
    let token = session
        .dispatch(
            "tex-ls/beginExternalRefresh",
            json!({"uri":fixture_uri!("/project/main.tex"), "kind":kind}),
        )
        .unwrap()["token"]
        .clone();
    session
        .dispatch(
            "tex-ls/applyExternalInputs",
            json!({"token":token,"inputs":inputs}),
        )
        .unwrap();
}

#[test]
fn external_observations_control_links_and_discovery_without_io() {
    let mut session = running_session();
    open(&mut session, "\\includegraphics{plot}\n");
    let query = json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}});
    assert_eq!(
        session
            .dispatch("textDocument/documentLink", query.clone())
            .unwrap(),
        json!([])
    );
    let needs = session
        .dispatch(
            "tex-ls/discoveryNeeds",
            json!({"uri":fixture_uri!("/project/main.tex")}),
        )
        .unwrap();
    assert!(!needs.as_array().unwrap().is_empty());
    publish(
        &mut session,
        "files",
        json!({"kind":"files", "locations":[[fixture_path!("/project"), {
            "kind":{"state":"present","value":"directory"},
            "directory":{"state":"present","value":{"entries":{"plot.png":"file"},"complete":true}}
        }]]}),
    );
    let links = session
        .dispatch("textDocument/documentLink", query.clone())
        .unwrap();
    assert_eq!(links[0]["target"], fixture_uri!("/project/plot.png"));
    publish(
        &mut session,
        "files",
        json!({"kind":"files", "locations":[[fixture_path!("/project"), {
            "kind":{"state":"present","value":"directory"},
            "directory":{"state":"present","value":{"entries":{},"complete":true}}
        }]]}),
    );
    assert_eq!(
        session
            .dispatch("textDocument/documentLink", query)
            .unwrap(),
        json!([])
    );
}

#[test]
fn superseded_and_repeated_browser_acquisitions_are_rejected() {
    let mut session = running_session();
    open(&mut session, "\\input{child}");
    let request = json!({"uri":fixture_uri!("/project/main.tex"),"kind":"files"});
    let old = session
        .dispatch("tex-ls/beginExternalRefresh", request.clone())
        .unwrap()["token"]
        .clone();
    let new = session
        .dispatch("tex-ls/beginExternalRefresh", request)
        .unwrap()["token"]
        .clone();
    let inputs = json!({"kind":"files","backing":[[fixture_path!("/project/child.tex"),"Child"]]});
    assert!(
        session
            .dispatch(
                "tex-ls/applyExternalInputs",
                json!({"token":old,"inputs":inputs})
            )
            .is_err()
    );
    session
        .dispatch(
            "tex-ls/applyExternalInputs",
            json!({"token":new,"inputs":inputs}),
        )
        .unwrap();
    assert!(
        session
            .dispatch(
                "tex-ls/applyExternalInputs",
                json!({"token":new,"inputs":inputs})
            )
            .is_err()
    );
}

#[test]
fn browser_compiler_facts_are_last_build_data_and_do_not_define_symbols() {
    let mut session = running_session();
    open(
        &mut session,
        "\\section{Intro}\\label{sec:intro}\\ref{sec:intro}\n\\ref{ghost}",
    );
    publish(
        &mut session,
        "compiler",
        json!({"kind":"compiler","artifacts":[[fixture_path!("/project/main.aux"), {"state":"present","value":{"identity":"build-1","text":"\\newlabel{sec:intro}{{7}{1}}\n\\newlabel{ghost}{{9}{1}}\n\\@input{main.aux}"}}]]}),
    );
    let query = json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}});
    let symbols = session
        .dispatch("textDocument/documentSymbol", query)
        .unwrap();
    assert!(symbols.to_string().contains('7'));
    let definitions = session.dispatch("textDocument/definition", json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")},"position":{"line":1,"character":7}})).unwrap();
    assert_eq!(definitions, json!([]));
}

fn running_session() -> Session {
    let mut session = Session::default();
    session.dispatch("initialize", json!({"capabilities":{"textDocument":{"documentSymbol":{"hierarchicalDocumentSymbolSupport":true}}}})).unwrap();
    session
}

#[test]
fn old_edit_preconditions_fail_after_close_and_reopen_at_the_same_version() {
    let mut session = running_session();
    open(&mut session, "\\label{key}\\ref{key}");
    let result = session.dispatch_with_context("textDocument/rename", json!({
        "textDocument":{"uri":fixture_uri!("/project/main.tex")}, "position":{"line":0,"character":8},"newName":"other"
    })).unwrap();
    assert_eq!(
        session
            .dispatch(
                "tex-ls/checkPreconditions",
                json!({"preconditions":result["preconditions"]})
            )
            .unwrap()["current"],
        true
    );
    session
        .dispatch(
            "textDocument/didClose",
            json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}}),
        )
        .unwrap();
    open(&mut session, "\\label{key}\\ref{key}");
    assert_eq!(
        session
            .dispatch(
                "tex-ls/checkPreconditions",
                json!({"preconditions":result["preconditions"]})
            )
            .unwrap()["current"],
        false
    );
}

#[test]
fn shutdown_exit_and_fresh_session_dispose_old_sources() {
    let mut session = Session::default();
    assert!(
        session
            .dispatch("workspace/symbol", json!({"query":""}))
            .is_err()
    );
    session.dispatch("initialize", json!({})).unwrap();
    open(&mut session, "\\section{Before restart}");
    session.dispatch("shutdown", json!(null)).unwrap();
    assert!(
        session
            .dispatch("workspace/symbol", json!({"query":""}))
            .is_err()
    );
    session.dispatch("exit", json!(null)).unwrap();
    assert!(session.dispatch("initialize", json!({})).is_err());
    let mut fresh = running_session();
    assert_eq!(
        fresh
            .dispatch("workspace/symbol", json!({"query":""}))
            .unwrap(),
        json!([])
    );
}

#[test]
fn replacing_a_project_drops_its_sources_settings_and_pending_acquisitions() {
    let mut session = running_session();
    open(&mut session, "\\section{Old project}");
    let token = session
        .dispatch(
            "tex-ls/beginExternalRefresh",
            json!({"uri":fixture_uri!("/project/main.tex"),"kind":"files"}),
        )
        .unwrap()["token"]
        .clone();
    session
        .dispatch(
            "tex-ls/replaceProject",
            json!({"uri":fixture_uri!("/project/main.tex")}),
        )
        .unwrap();
    assert!(session.dispatch("tex-ls/applyExternalInputs", json!({"token":token,"inputs":{"kind":"files","backing":[[fixture_path!("/project/old.tex"),"Old"]]}})).is_err());
    assert_eq!(
        session
            .dispatch("workspace/symbol", json!({"query":""}))
            .unwrap(),
        json!([])
    );
    open(&mut session, "\\section{New project}");
    let symbols = session
        .dispatch("workspace/symbol", json!({"query":""}))
        .unwrap();
    assert!(symbols.to_string().contains("New project"));
    assert!(!symbols.to_string().contains("Old project"));
}

#[test]
fn completion_items_from_a_replaced_attachment_do_not_resolve() {
    let mut session = running_session();
    open(&mut session, "\\newcommand{\\greet}[1]{Hello #1}\n\\gre");
    let list = session
        .dispatch(
            "textDocument/completion",
            json!({
                "textDocument":{"uri":fixture_uri!("/project/main.tex")},
                "position":{"line":1,"character":4}
            }),
        )
        .unwrap();
    let item = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["data"]["name"] == "greet")
        .expect("local command")
        .clone();
    let resolved = session
        .dispatch("completionItem/resolve", item.clone())
        .unwrap();
    assert!(resolved["documentation"].is_object());
    open(&mut session, "\\newcommand{\\greet}[2]{Changed}\n\\gre");
    assert_eq!(
        session
            .dispatch("completionItem/resolve", item.clone())
            .unwrap(),
        item
    );
}

#[test]
fn known_source_availability_still_requests_missing_contents() {
    let mut session = running_session();
    open(&mut session, "\\input{child}");
    session
        .dispatch(
            "tex-ls/registerFile",
            json!({"uri":fixture_uri!("/project/child.tex")}),
        )
        .unwrap();
    let needs = session
        .dispatch(
            "tex-ls/discoveryNeeds",
            json!({"uri":fixture_uri!("/project/main.tex")}),
        )
        .unwrap();
    assert!(
        needs
            .as_array()
            .unwrap()
            .contains(&json!({"kind":"source","path":std::path::Path::new(fixture_path!("/project/child.tex")).components().collect::<std::path::PathBuf>()}))
    );
}

#[test]
fn browser_minimal_and_rich_capabilities_choose_response_shapes() {
    for rich in [false, true] {
        let mut session = Session::default();
        let initialized = session
            .dispatch(
                "initialize",
                json!({"capabilities":{"textDocument":{
                    "documentSymbol":{"hierarchicalDocumentSymbolSupport":rich},
                    "hover":{"contentFormat":[if rich {"markdown"} else {"plaintext"}]}
                }}}),
            )
            .unwrap();
        assert_eq!(initialized["serverInfo"]["name"], "tex-ls");
        assert!(
            initialized["capabilities"]
                .get("codeActionProvider")
                .is_none()
        );
        open(&mut session, "\\section{Intro}\n");
        let symbols = session
            .dispatch(
                "textDocument/documentSymbol",
                json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}}),
            )
            .unwrap();
        assert_eq!(symbols[0].get("selectionRange").is_some(), rich);
        assert_eq!(symbols[0].get("location").is_some(), !rich);
        let hover = session.dispatch("textDocument/hover", json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")},"position":{"line":0,"character":3}})).unwrap();
        assert_eq!(
            hover["contents"]["kind"],
            if rich { "markdown" } else { "plaintext" }
        );
    }
}

#[test]
fn compiler_reports_replace_clear_filter_and_share_workspace_identity() {
    let mut session = running_session();
    open(&mut session, "Text.\nSecond line.\n");
    let params = json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}});
    publish(
        &mut session,
        "compiler",
        json!({"kind":"compiler","artifacts":[[fixture_path!("/project/main.log"),{"state":"present","value":{"identity":"build1","text":"./main.tex:2: Broken command"}}],[fixture_path!("/project/main.fls"),{"state":"present","value":{"identity":"recorder1","text":format!("PWD {}\nINPUT main.tex\nOUTPUT main.pdf", fixture_path!("/project"))}}]]}),
    );
    let first = session
        .dispatch("textDocument/diagnostic", params.clone())
        .unwrap();
    assert!(
        first["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["source"] == "compiler" && item["range"]["start"]["line"] == 1)
    );
    let workspace = session
        .dispatch("workspace/diagnostic", json!({"previousResultIds":[]}))
        .unwrap();
    assert_eq!(workspace["items"][0]["resultId"], first["resultId"]);
    let unchanged = session.dispatch("workspace/diagnostic", json!({"previousResultIds":[{"uri":fixture_uri!("/project/main.tex"),"value":first["resultId"]}]})).unwrap();
    assert_eq!(unchanged["items"][0]["kind"], "unchanged");
    session
        .dispatch(
            "tex-ls/updateSettings",
            json!({"settings":{"lint":{"external":{"sources":[]}}}}),
        )
        .unwrap();
    let filtered = session
        .dispatch("textDocument/diagnostic", params.clone())
        .unwrap();
    assert!(
        filtered["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["source"] != "compiler")
    );
    publish(
        &mut session,
        "compiler",
        json!({"kind":"compiler","artifacts":[[fixture_path!("/project/main.log"),{"state":"absent"}],[fixture_path!("/project/main.fls"),{"state":"absent"}]]}),
    );
    session
        .dispatch("tex-ls/updateSettings", json!({"settings":{}}))
        .unwrap();
    let cleared = session.dispatch("textDocument/diagnostic", params).unwrap();
    assert!(
        cleared["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["source"] != "compiler")
    );
    assert_ne!(cleared["resultId"], first["resultId"]);
    let inspected = session.dispatch("workspace/executeCommand", json!({"command":"tex-ls.inspectProject","arguments":[{"uri":fixture_uri!("/project/main.tex")}]})).unwrap();
    assert_eq!(inspected["compilerObservations"], json!([]));
}

#[test]
fn fix_all_carries_editor_version_and_declining_it_changes_nothing() {
    let mut session = Session::default();
    session.dispatch("initialize", json!({"capabilities":{"workspace":{"workspaceEdit":{"documentChanges":true}},"textDocument":{"codeAction":{"codeActionLiteralSupport":{"codeActionKind":{"valueSet":[]}}}}}})).unwrap();
    open(&mut session, "$x^{2}$ and $y_{3}$\n");
    let request = json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")},"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"context":{"diagnostics":[],"only":["source.fixAll.tex-ls"]}});
    let actions = session
        .dispatch("textDocument/codeAction", request.clone())
        .unwrap();
    assert_eq!(actions.as_array().unwrap().len(), 1);
    assert_eq!(
        actions[0]["edit"]["documentChanges"][0]["textDocument"]["version"],
        0
    );
    // A client owns literal-edit application/refusal. No command mutates the
    // server's source; declining leaves the identical action available.
    assert_eq!(
        session
            .dispatch("textDocument/codeAction", request)
            .unwrap(),
        actions
    );
}

#[test]
fn outline_display_settings_do_not_change_formatting_or_definitions() {
    let mut session = running_session();
    open(
        &mut session,
        "\\section{Title}\n\\begin{custom}Text\\label{a}\\end{custom}\n\\ref{a}\n",
    );
    let params = json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}});
    let format = session
        .dispatch("textDocument/formatting", params.clone())
        .unwrap();
    let symbols = session
        .dispatch("textDocument/documentSymbol", params.clone())
        .unwrap();
    session.dispatch("tex-ls/updateSettings", json!({"settings":{"outline":{"sections":false,"environmentNames":{"custom":"My environment"}},"inlayHints":{"definitions":false,"references":false}}})).unwrap();
    let changed = session
        .dispatch("textDocument/documentSymbol", params.clone())
        .unwrap();
    assert_ne!(changed, symbols);
    assert!(changed.to_string().contains("My environment"));
    assert_eq!(
        session.dispatch("textDocument/formatting", params).unwrap(),
        format
    );
    let rejected = session
        .dispatch(
            "tex-ls/updateSettings",
            json!({"settings":{"inlayHints":{"maxLength":0}}}),
        )
        .unwrap();
    assert_eq!(rejected["applied"], false);
}

#[test]
fn nesting_recovery_keeps_the_session_usable() {
    let mut session = running_session();
    for closed in [false, true] {
        let text = format!(
            "{}x{}",
            "{".repeat(10_000),
            if closed {
                "}".repeat(10_000)
            } else {
                String::new()
            }
        );
        open(&mut session, &text);
        assert_eq!(
            session
                .dispatch(
                    "textDocument/documentSymbol",
                    json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}})
                )
                .unwrap(),
            json!([])
        );
        session
            .dispatch(
                "textDocument/didClose",
                json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}}),
            )
            .unwrap();
    }
    open(&mut session, "\\section{Recovered}");
    let symbols = session
        .dispatch(
            "textDocument/documentSymbol",
            json!({"textDocument":{"uri":fixture_uri!("/project/main.tex")}}),
        )
        .unwrap();
    assert_eq!(symbols[0]["name"], "Recovered");
}
