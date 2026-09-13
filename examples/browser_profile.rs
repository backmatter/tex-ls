//! Profile a JSON document fixture through native stages shared with the browser.
//! cargo run --release --example browser_profile -- /tmp/tex-ls-large-fixture.json
//!
//! Input is an array of {path, source} documents, including main.tex. Row zero
//! includes cold queries; rows 1..=100 alternate command-prefix replacements.
//! The stage database deliberately demands every listed query. completionMs
//! measures a separate Session with the actual request's demand dependencies.
//! Both databases are disposable profiling state, not application persistence.
use lsp_types::{Position, Range};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Instant};
use tex_ls_analysis::{
    incremental::IncrementalDatabase,
    text::{PositionEncoding, TextBuffer},
};
#[derive(Deserialize)]
struct Source {
    path: String,
    source: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sample {
    write_ms: f64,
    parse_ms: f64,
    classify_ms: f64,
    scope_ms: f64,
    semantic_ms: f64,
    candidates_ms: f64,
    completion_ms: f64,
    items: usize,
}

fn main() {
    let sources: Vec<Source> = serde_json::from_str(
        &std::fs::read_to_string(std::env::args().nth(1).expect("fixture JSON path")).unwrap(),
    )
    .unwrap();
    let mut db = IncrementalDatabase::default();
    let mut session = Session::default();
    session
        .dispatch("initialize", serde_json::json!({}))
        .unwrap();
    for source in &sources {
        let path = PathBuf::from("/project").join(&source.path);
        db.apply_change(&path, source.source.as_str(), None);
        session.dispatch("textDocument/didOpen", json!({"textDocument":{"uri":format!("file://{}",path.display()),"languageId":"latex","version":0,"text":source.source}})).unwrap();
    }
    let path = PathBuf::from("/project/main.tex");
    let source = &sources
        .iter()
        .find(|s| s.path == "main.tex")
        .unwrap()
        .source;
    let mut text = Arc::new(TextBuffer::new(
        format!("{source}\n\\sec\n"),
        PositionEncoding::Utf16,
    ));
    let line = source.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let file = db.apply_change(&path, text.text_arc(), None);
    session.dispatch("textDocument/didChange", json!({"textDocument":{"uri":"file:///project/main.tex","version":1},"contentChanges":[{"text":text.to_string()}]})).unwrap();
    let mut rows = Vec::new();
    for i in 0..101 {
        let prefix = if i % 2 == 0 { "pro" } else { "sec" };
        let changes = vec![
            lsp_types::TextDocumentContentChangePartial {
                range: Range::new(Position::new(line, 1), Position::new(line, 4)),
                text: prefix.into(),
                ..Default::default()
            }
            .into(),
        ];
        let start = Instant::now();
        let edits = apply_content_changes(&mut text, changes.clone()).unwrap();
        db.apply_change(&path, text.text_arc(), edits);
        let write = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let snapshot = db.snapshot();
        let root = snapshot.parsed_tree(file);
        let parse = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let ctx = tex_ls_analysis::completion::classify_context_with_declarations(
            &root,
            text.len() - 1,
            snapshot.declarations(),
        );
        let classify = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let sigs = snapshot.scope_signatures(file);
        let scope = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let model = snapshot.semantic_model(file);
        let semantic = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        std::hint::black_box(tex_ls_analysis::completion::candidates_with_declarations(
            &ctx,
            sigs,
            model,
            snapshot.declarations(),
        ));
        let candidates = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        session.dispatch("textDocument/didChange",json!({"textDocument":{"uri":"file:///project/main.tex","version":i+2},"contentChanges":changes})).unwrap();
        let answer=session.dispatch("textDocument/completion",json!({"textDocument":{"uri":"file:///project/main.tex"},"position":{"line":line,"character":4}})).unwrap();
        let completion = start.elapsed().as_secs_f64() * 1000.;
        rows.push(Sample {
            write_ms: write,
            parse_ms: parse,
            classify_ms: classify,
            scope_ms: scope,
            semantic_ms: semantic,
            candidates_ms: candidates,
            completion_ms: completion,
            items: answer["items"].as_array().unwrap().len(),
        });
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "files": sources.len(),
            "sourceBytes": sources.iter().map(|source| source.source.len()).sum::<usize>(),
            "activeBytes": source.len(),
            "coldRow": 0,
            "raw": rows,
        }))
        .unwrap()
    );
}

use tex_ls_browser::session::Session;
use tex_ls_protocol::apply_content_changes;
