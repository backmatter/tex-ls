//! Measure bounded workspace search latency and payloads.
use std::{path::Path, time::Instant};
use tex_ls_analysis::{incremental::IncrementalDatabase, text::PositionEncoding};
fn main() {
    for size in [1000, 10000] {
        let mut db = IncrementalDatabase::default();
        let path = Path::new("/symbols-benchmark/main.tex");
        let source: String = (0..size)
            .map(|n| format!("\\section{{Topic {n:05}}}\n"))
            .collect();
        db.apply_change(path, source, None);
        for query in ["", "Topic 00999"] {
            let mut times = Vec::new();
            let mut bytes = 0;
            let mut count = 0;
            for _ in 0..6 {
                let start = Instant::now();
                let result = tex_ls_protocol::symbols::compute_projects_workspace_symbols(
                    &[db.snapshot()],
                    query,
                    PositionEncoding::Utf16,
                    &|_| Default::default(),
                    Some(path),
                );
                times.push(start.elapsed().as_micros());
                let value = serde_json::to_value(result).unwrap();
                count = value.as_array().unwrap().len();
                bytes = serde_json::to_vec(&value).unwrap().len();
                assert!(count <= tex_ls_protocol::symbols::WORKSPACE_SYMBOL_LIMIT);
                if !query.is_empty() {
                    assert_eq!(value[0]["name"], query);
                }
            }
            let cold = times.remove(0);
            times.sort_unstable();
            println!(
                "symbols={size} query={query:?} cold_us={cold} warm_p50_us={} warm_max_us={} returned={count} bytes={bytes}",
                times[2], times[4]
            );
        }
    }
}
