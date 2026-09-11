use meaning_analysis::{incremental::IncrementalDatabase, linter::lint_document};
use std::{path::PathBuf, time::Instant};
fn main() {
    let sources: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap())
            .unwrap();
    let mut db = IncrementalDatabase::default();
    for s in sources.as_array().unwrap() {
        let p = PathBuf::from("/project").join(s["path"].as_str().unwrap());
        db.apply_change(&p, s["source"].as_str().unwrap(), None);
    }
    let snapshot = db.snapshot();
    let p = PathBuf::from("/project/main.tex");
    let f = snapshot.lookup_file(&p).unwrap();
    let mut times = serde_json::Map::new();
    macro_rules! timed {
        ($name:literal,$expr:expr) => {{
            let t = Instant::now();
            let result = $expr;
            times.insert(
                $name.into(),
                serde_json::json!(t.elapsed().as_secs_f64() * 1000.),
            );
            result
        }};
    }
    let root = timed!("parseMs", snapshot.parsed_tree(f));
    let model = timed!("semanticMs", snapshot.semantic_model(f));
    let packages = timed!("packagesMs", snapshot.resolve_package_options());
    let (labels, citations) = timed!("resolutionMs", snapshot.resolve_project());
    let result = timed!(
        "lintMs",
        lint_document(
            &p,
            &root,
            model,
            Some(labels),
            Some(citations),
            Some(packages)
        )
    );
    times.insert("diagnostics".into(), serde_json::json!(result.len()));
    println!("{}", serde_json::to_string_pretty(&times).unwrap());
}
