//! Compare bibliography entry recovery by traversal and by its known source range.
use meaning_parser::bib::{ast, parse, syntax::SyntaxKind};
use std::{hint::black_box, time::Instant};
fn median(mut f: impl FnMut()) -> f64 {
    let mut samples = Vec::new();
    for _ in 0..31 {
        let start = Instant::now();
        f();
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    samples.sort_by(f64::total_cmp);
    samples[15]
}
fn main() {
    if cfg!(debug_assertions) {
        eprintln!("run in release mode");
        std::process::exit(2);
    }
    let text = "@article{key,title={Research},author={Researcher},year={2025}}\n".repeat(20000);
    let root = parse(&text).syntax();
    let entry = root
        .children()
        .filter(|node| node.kind() == SyntaxKind::ENTRY)
        .last()
        .unwrap();
    let range = entry.text_range();
    let scan = || {
        root.descendants()
            .find(|node| node.kind() == SyntaxKind::ENTRY && node.text_range() == range)
            .unwrap()
    };
    assert_eq!(scan(), ast::entry_at_range(&root, range).unwrap());
    let scan_ms = median(|| {
        black_box(scan());
    });
    let range_ms = median(|| {
        black_box(ast::entry_at_range(&root, range).unwrap());
    });
    assert!(
        scan_ms / range_ms > 10.,
        "range lookup performance regression"
    );
    println!(
        "{}",
        serde_json::json!({"entries":20000,"scanMedianMs":scan_ms,"rangeMedianMs":range_ms,"resultsEqual":true})
    );
}
