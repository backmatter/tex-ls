//! Compare repeated native lint traversal with the shared derived query.
use std::{hint::black_box, path::PathBuf, time::Instant};
use tex_ls_analysis::{incremental::IncrementalDatabase, linter::lint_document};
fn median(mut f: impl FnMut()) -> f64 {
    let mut values = Vec::new();
    for _ in 0..31 {
        let t = Instant::now();
        f();
        values.push(t.elapsed().as_secs_f64() * 1000.);
    }
    values.sort_by(f64::total_cmp);
    values[15]
}
fn main() {
    if cfg!(debug_assertions) {
        eprintln!("run in release mode");
        std::process::exit(2);
    }
    let mut db = IncrementalDatabase::default();
    let path = PathBuf::from("/project/main.tex");
    let text = "Ordinary research prose.\n".repeat(40000) + "\\bf text\n";
    let file = db.apply_change(&path, text, None);
    let start = Instant::now();
    let cached = db.latex_lint_findings(file);
    let cold_ms = start.elapsed().as_secs_f64() * 1000.;
    let snapshot = db.snapshot();
    let root = snapshot.parsed_tree(file);
    let model = snapshot.semantic_model(file);
    let packages = snapshot.resolve_package_options();
    let eager = || lint_document(&path, &root, model, None, None, Some(packages));
    assert_eq!(cached, &eager());
    assert!(cached.iter().any(|d| d.fix.is_some()));
    let eager_ms = median(|| {
        black_box(eager());
    });
    let query_ms = median(|| {
        black_box(db.latex_lint_findings(file));
    });
    assert!(eager_ms / query_ms > 10., "query reuse regression");
    println!(
        "{}",
        serde_json::json!({"coldMs":cold_ms,"eagerMedianMs":eager_ms,"queryMedianMs":query_ms,"speedup":eager_ms/query_ms,"findings":cached.len(),"resultsEqual":true})
    );
}
