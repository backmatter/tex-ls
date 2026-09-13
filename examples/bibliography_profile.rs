//! Stage timings and live heap for the pinned bibliography; run in release mode.
#[path = "support/allocations.rs"]
mod allocations;
use serde_json::json;
use std::{
    hash::{Hash, Hasher},
    hint::black_box,
    path::Path,
    time::Instant,
};
use tex_ls_analysis::{
    incremental::IncrementalDatabase, linter::RuleSelection, text::PositionEncoding,
};
use tex_ls_protocol::{
    diagnostics::analyze_bib, response_policy::ResponsePolicy, symbols::compute_bib_symbols,
};
fn stage<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let before = allocations::live_bytes();
    allocations::reset_peak();
    let start = Instant::now();
    let result = f();
    let ms = start.elapsed().as_secs_f64() * 1000.;
    let live = allocations::live_bytes();
    println!(
        "{}",
        json!({"stage":name,"ms":ms,"live_bytes":live,"delta_bytes":live as i64-before as i64,"peak_bytes":allocations::peak_bytes()})
    );
    result
}
// Baseline adapter retained only in this harness, to compare allocation peaks
// and prove complete wire equivalence under the same process conditions.
fn legacy_flatten(
    items: &[serde_json::Value],
    container: Option<&serde_json::Value>,
    out: &mut Vec<serde_json::Value>,
) {
    for item in items {
        let mut symbol = json!({"name":item["name"],"kind":item["kind"],"location":{"uri":"file:///profile.bib","range":item["selectionRange"]}});
        if let Some(container) = container {
            symbol["containerName"] = container.clone();
        }
        out.push(symbol);
        if let Some(children) = item.get("children").and_then(serde_json::Value::as_array) {
            legacy_flatten(children, item.get("name"), out);
        }
    }
}
fn digest(value: &serde_json::Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(value).unwrap().hash(&mut hasher);
    hasher.finish()
}
fn main() {
    if cfg!(debug_assertions) {
        eprintln!("run in release mode");
        std::process::exit(2);
    }
    let path = Path::new("target/lsp-release-inputs/bibliography/rendering-bibtex.bib");
    let text = std::fs::read_to_string(path).unwrap();
    let mut db = IncrementalDatabase::default();
    let file = db.apply_change(path, text.clone(), None);
    let rules = RuleSelection::resolve(None, &[]).0;
    let enc = PositionEncoding::Utf16;
    let policy = ResponsePolicy::new(&json!({"capabilities":{}}));
    for generation in 0..=5 {
        println!("{}", json!({"generation":generation}));
        if generation > 0 {
            let at = text.find("title").unwrap();
            let at = at + text[at..].find('{').unwrap() + 1;
            let edit = tex_ls_parser::parser::Edit {
                range: at..at,
                insert: "x".repeat(generation),
            };
            db.apply_change(
                path,
                edit.apply(&text),
                Some(vec![tex_ls_parser::parser::Edit {
                    range: at..at,
                    insert: "x".into(),
                }]),
            );
        }
        stage("parse", || black_box(db.parsed_bib_tree(file)));
        stage("semantic_model", || {
            black_box(db.bib_semantic_model(file));
        });
        stage("lint", || {
            black_box(db.bib_lint_findings(file));
        });
        let snapshot = db.snapshot();
        let diagnostics = stage("diagnostic_conversion", || {
            analyze_bib(&snapshot, path, &rules, enc).unwrap()
        });
        let mut value = stage("diagnostic_json", || {
            serde_json::to_value(&diagnostics).unwrap()
        });
        stage("diagnostic_policy", || {
            policy.response("textDocument/diagnostic", None, &mut value)
        });
        stage("diagnostic_serialization", || {
            black_box(serde_json::to_vec(&value).unwrap())
        });
        if generation == 0 {
            let symbols = stage("outline_conversion", || {
                compute_bib_symbols(&snapshot, path, enc)
            });
            let mut value = stage("outline_json", || serde_json::to_value(&symbols).unwrap());
            ResponsePolicy::new(&json!({"capabilities":{"textDocument":{"documentSymbol":{"hierarchicalDocumentSymbolSupport":true}}}})).response("textDocument/documentSymbol",None,&mut value);
            let legacy = stage("legacy_outline_policy", || {
                let mut flat = Vec::new();
                legacy_flatten(value.as_array().unwrap(), None, &mut flat);
                serde_json::Value::Array(flat)
            });
            let expected = digest(&legacy);
            let count = legacy.as_array().unwrap().len();
            drop(legacy);
            stage("outline_policy", || {
                policy.response(
                    "textDocument/documentSymbol",
                    Some(&json!("file:///profile.bib")),
                    &mut value,
                )
            });
            assert_eq!(value.as_array().unwrap().len(), count);
            assert_eq!(digest(&value), expected);
            println!(
                "{}",
                json!({"outline_items":count,"legacy_wire_equal":true})
            );
            stage("outline_serialization", || {
                black_box(serde_json::to_vec(&value).unwrap())
            });
        }
    }
    let current = db.parsed_bib_tree(file);
    let full = tex_ls_parser::bib::parse(&current.to_string());
    assert_eq!(current.green(), &*full.green);
    assert_eq!(
        db.bib_parse_diagnostics(file)
            .iter()
            .map(|e| (e.start, e.end, e.message.as_str()))
            .collect::<Vec<_>>(),
        full.errors
            .iter()
            .map(|e| (e.start, e.end, e.message.as_str()))
            .collect::<Vec<_>>()
    );
    drop(current);
    drop(full);
    stage("settled", || ());
    drop(db);
    println!(
        "{}",
        json!({"after_database_drop_live_bytes":allocations::live_bytes(),"linux_process_status":std::fs::read_to_string("/proc/self/status").ok().map(|s| s.lines().filter(|l| l.starts_with("VmRSS:") || l.starts_with("VmHWM:")).collect::<Vec<_>>().join("; "))})
    );
}
