//! Separate membership reconstruction from the next surviving-source query.
use meaning_analysis::incremental::IncrementalDatabase;
use std::{hint::black_box, path::PathBuf, time::Instant};

fn main() {
    let mut samples = Vec::new();
    for count in [2, 8, 32, 128] {
        for repeat in 0..3 {
            let mut db = IncrementalDatabase::default();
            let mut members = Vec::new();
            for index in 0..count {
                let path = PathBuf::from(format!("/project/{index}.tex"));
                let source = format!(
                    "\\section{{Chapter {index}}}\n\\label{{chapter:{index}}}\n{}",
                    "Prose with $x^2$ and \\emph{emphasis}.\n".repeat(100)
                );
                let id = db.upsert_file(&path, source);
                members.push((path, id));
            }
            let query = |db: &IncrementalDatabase| {
                for (_, id) in &members[count / 2..] {
                    black_box(db.semantic_model(*id));
                    black_box(db.latex_lint_findings(*id));
                }
                black_box(db.resolve_project());
            };
            let start = Instant::now();
            query(&db);
            let cold = start.elapsed().as_nanos();
            let start = Instant::now();
            query(&db);
            let warm = start.elapsed().as_nanos();
            let removed: Vec<_> = members[..count / 2]
                .iter()
                .map(|(path, _)| path.clone())
                .collect();
            let start = Instant::now();
            db.remove_files(&removed);
            let rebuild = start.elapsed().as_nanos();
            let start = Instant::now();
            query(&db);
            let after_rebuild = start.elapsed().as_nanos();
            let start = Instant::now();
            query(&db);
            let after_warm = start.elapsed().as_nanos();
            samples.push(serde_json::json!({
                "sources": count, "removed": count / 2, "repeat": repeat,
                "cold_ns": cold, "warm_ns": warm, "rebuild_ns": rebuild,
                "survivor_first_query_ns": after_rebuild, "survivor_warm_query_ns": after_warm,
            }));
        }
    }
    println!("{}", serde_json::to_string_pretty(&samples).unwrap());
}
