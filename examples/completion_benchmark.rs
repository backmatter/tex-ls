//! Measure ranking latency and wire payloads before selecting the production cap.
use lsp_types::{CompletionItem, CompletionItemKind};
use std::{collections::HashMap, time::Instant};
use tex_ls_protocol::completion_rank::rank;
fn main() {
    end_to_end();
    for size in [10_000, 50_000] {
        let items: Vec<_> = (0..size)
            .map(|i| CompletionItem {
                label: format!("key{i:06}"),
                filter_text: Some(format!(
                    "key{i:06} {} Research on deterministic systems by José García",
                    "Long bibliography title ".repeat(12)
                )),
                kind: Some(CompletionItemKind::Reference),
                ..Default::default()
            })
            .collect();
        for cap in [50, 100, 200] {
            for query in ["", "garcía", "det sys", "key000042"] {
                let mut times = Vec::new();
                let mut bytes = 0;
                for _ in 0..10 {
                    let input = items.clone();
                    let started = Instant::now();
                    let list = rank(input, query, &HashMap::new(), cap, false);
                    times.push(started.elapsed().as_micros());
                    bytes = serde_json::to_vec(&list).unwrap().len();
                    if query == "key000042" {
                        assert_eq!(list.items[0].label, query);
                    }
                }
                times.sort_unstable();
                println!(
                    "entries={size} cap={cap} query={query:?} p50_us={} p95_us={} bytes={bytes}",
                    times[5], times[9]
                );
            }
        }
    }
}

fn end_to_end() {
    use std::path::Path;
    use tex_ls_analysis::{incremental::IncrementalDatabase, text::PositionEncoding};
    for size in [10_000, 50_000] {
        let mut db = IncrementalDatabase::default();
        let path = Path::new("/completion-benchmark/main.tex");
        let source = "\\documentclass{article}\\bibliography{refs}\\cite{garcía}";
        let bibliography: String = (0..size)
            .map(|i| {
                format!(
                    "@book{{key{i:06},title={{{}}},author={{García, José}}}}\n",
                    "Long bibliography title ".repeat(12)
                )
            })
            .collect();
        db.apply_change(path, source, None);
        db.apply_change(
            Path::new("/completion-benchmark/refs.bib"),
            bibliography.as_str(),
            None,
        );
        let mut times = Vec::new();
        let mut bytes = 0;
        let mut default_bytes = 0;
        for _ in 0..6 {
            let start = Instant::now();
            let list = tex_ls_protocol::compute_completion(
                &db,
                &tex_ls_protocol::path_to_uri(path).unwrap(),
                path,
                PositionEncoding::Utf8,
                lsp_types::Position::new(0, (source.len() - 1) as u32),
            );
            times.push(start.elapsed().as_micros());
            assert_eq!(
                list.items.len(),
                tex_ls_protocol::completion_rank::RESULT_LIMIT
            );
            assert!(list.is_incomplete);
            bytes = serde_json::to_vec(&list).unwrap().len();
            let mut negotiated = serde_json::to_value(&list).unwrap();
            tex_ls_protocol::ResponsePolicy::new(&serde_json::json!({"capabilities":{"textDocument":{"completion":{"completionItem":{"snippetSupport":true},"completionList":{"itemDefaults":["editRange","insertTextFormat","insertTextMode"]}}}}})).response("textDocument/completion",None,&mut negotiated);
            default_bytes = serde_json::to_vec(&negotiated).unwrap().len();
        }
        let cold = times.remove(0);
        times.sort_unstable();
        println!(
            "pipeline_entries={size} cold_us={cold} warm_p50_us={} warm_max_us={} bytes={bytes} negotiated_default_bytes={default_bytes}",
            times[2], times[4]
        );
    }
}
