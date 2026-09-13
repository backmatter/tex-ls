//! Measure diagnostic identities, unchanged reports, and isolated source edits.
//! Run with `cargo run --release --example diagnostics_benchmark --locked`.
use serde_json::{Value, json};
use std::{path::Path, time::Instant};
use tex_ls_analysis::{
    incremental::{Analysis, IncrementalDatabase},
    linter::RuleSelection,
    source::file_kind_or_tex,
    text::PositionEncoding,
};
use tex_ls_protocol::diagnostic_store::{document_diagnostics, workspace_diagnostics};
fn main() {
    for size in [100, 1000] {
        let mut db = IncrementalDatabase::default();
        let source = format!(
            "\\documentclass{{article}}\n{}",
            "% Unchanged prose padding for source content identity measurements.\n".repeat(64)
        );
        for index in 0..size {
            db.apply_change(
                &Path::new("/diagnostics-benchmark").join(format!("doc{index:04}.tex")),
                source.as_str(),
                None,
            );
        }
        let snapshot = db.snapshot();
        let started = Instant::now();
        let initial = pull(&snapshot, &json!([]));
        let cold = started.elapsed().as_micros();
        let previous = json!(
            initial["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| json!({"uri":item["uri"],"value":item["resultId"]}))
                .collect::<Vec<_>>()
        );
        let mut workspace_times = Vec::new();
        let mut individual_times = Vec::new();
        let mut bytes = 0;
        let rules = RuleSelection::resolve(None, &[]).0;
        for _ in 0..3 {
            let started = Instant::now();
            let unchanged = pull(&snapshot, &previous);
            workspace_times.push(started.elapsed().as_micros());
            assert!(
                unchanged["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|item| item["kind"] == "unchanged")
            );
            bytes = serde_json::to_vec(&unchanged).unwrap().len();
            let started = Instant::now();
            for ((path, _), expected) in snapshot
                .tracked_files()
                .into_iter()
                .zip(initial["items"].as_array().unwrap())
            {
                let report = document_diagnostics(
                    &snapshot,
                    &path,
                    file_kind_or_tex(&path),
                    &rules,
                    PositionEncoding::Utf16,
                    expected["resultId"].as_str(),
                );
                assert_eq!(report["kind"], "unchanged");
                assert_eq!(report["resultId"], expected["resultId"]);
            }
            individual_times.push(started.elapsed().as_micros());
        }
        drop(snapshot);
        let path = Path::new("/diagnostics-benchmark/doc0000.tex");
        db.apply_change(path, format!("{source}\\ref{{missing}}"), None);
        let started = Instant::now();
        let changed = pull(&db.snapshot(), &previous);
        let changed_us = started.elapsed().as_micros();
        assert_eq!(
            changed["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["kind"] == "full")
                .count(),
            1
        );
        workspace_times.sort_unstable();
        individual_times.sort_unstable();
        println!(
            "files={size} source_bytes={} cold_workspace_us={cold} unchanged_workspace_p50_us={} individual_unchanged_p50_us={} one_file_change_us={changed_us} response_bytes={bytes}",
            size * source.len(),
            workspace_times[1],
            individual_times[1]
        );
    }
}
fn pull(snapshot: &Analysis, previous: &Value) -> Value {
    workspace_diagnostics(
        std::slice::from_ref(snapshot),
        previous,
        PositionEncoding::Utf16,
        |_| RuleSelection::resolve(None, &[]).0,
        |_| None,
    )
}
