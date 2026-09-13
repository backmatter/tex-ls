//! Per-rule costs using the real lint driver; no counting allocator overhead.
use serde_json::json;
use std::{path::Path, time::Instant};
use tex_ls_analysis::{
    bib::{
        linter::{check::lint_document_with_rules, rules::all_rules},
        semantic::Model,
    },
    incremental::IncrementalDatabase,
};
fn main() {
    if cfg!(debug_assertions) {
        eprintln!("run in release mode");
        std::process::exit(2);
    }
    let path = Path::new("target/lsp-release-inputs/bibliography/rendering-bibtex.bib");
    let text = std::fs::read_to_string(path).unwrap();
    let root = tex_ls_parser::bib::parse(&text).syntax();
    let mut db = IncrementalDatabase::default();
    let file = db.apply_change(path, text, None);
    for round in 0..3 {
        let start = Instant::now();
        let model = Model::build(&root);
        println!(
            "{}",
            json!({"round":round,"stage":"model","ms":start.elapsed().as_secs_f64()*1000.})
        );
        let rules = all_rules();
        let mut combined = Vec::new();
        for rule in &rules {
            let start = Instant::now();
            let findings =
                lint_document_with_rules(path, &root, &model, None, std::slice::from_ref(rule));
            println!(
                "{}",
                json!({"round":round,"stage":rule.id(),"ms":start.elapsed().as_secs_f64()*1000.,"findings":findings.len()})
            );
            combined.extend(findings);
        }
        let start = Instant::now();
        let findings = lint_document_with_rules(path, &root, &model, None, &rules);
        println!(
            "{}",
            json!({"round":round,"stage":"all_rules","ms":start.elapsed().as_secs_f64()*1000.,"findings":findings.len()})
        );
        combined.sort_by_key(|d| (d.start, d.end, d.rule));
        assert_eq!(combined, findings);
        if round == 0
            && let Some(prefix) = std::env::args().nth(1)
        {
            std::fs::write(format!("{prefix}-model.txt"), format!("{model:?}")).unwrap();
            std::fs::write(
                format!("{prefix}-findings.json"),
                serde_json::to_vec(&findings).unwrap(),
            )
            .unwrap();
            std::fs::write(
                format!("{prefix}-project-findings.json"),
                serde_json::to_vec(db.bib_lint_findings(file)).unwrap(),
            )
            .unwrap();
        }
    }
}
