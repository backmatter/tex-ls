//! Release equivalence and speed floor for the new shared edit paths.
use std::{hint::black_box, time::Instant};
use tex_ls_parser::{
    bib,
    declarations::ResolvedDeclarations,
    parser::{
        Edit, LexConfig, ReparseBase, ReparseTier, parse_with_declarations_resolved, reparse,
    },
};
fn median(mut x: Vec<f64>) -> f64 {
    x.sort_by(f64::total_cmp);
    x[x.len() / 2]
}
fn main() {
    if cfg!(debug_assertions) {
        eprintln!("Run this benchmark in release mode");
        std::process::exit(2);
    }
    let decl = ResolvedDeclarations::default();
    let text = "\\begin{document}\n".to_owned()
        + &"Ordinary research prose.\n".repeat(40000)
        + "\\end{document}\n\\sec\n";
    let (parsed, ctx) = parse_with_declarations_resolved(&text, LexConfig::default(), &decl);
    let base = ReparseBase::from_parts(
        &text,
        &parsed.green,
        &parsed.errors,
        &ctx,
        LexConfig::default(),
        &decl,
    );
    let at = text.rfind("sec").unwrap();
    let edit = Edit {
        range: at..at + 3,
        insert: "pro".into(),
    };
    let new = edit.apply(&text);
    let result = reparse(&base, &edit, &new).expect("command splice");
    assert_eq!(result.tier, ReparseTier::Token);
    let (full, _) = parse_with_declarations_resolved(&new, LexConfig::default(), &decl);
    assert_eq!(result.green, full.green);
    assert_eq!(result.errors, full.errors);
    let mut tex_fast = vec![];
    let mut tex_full = vec![];
    for _ in 0..31 {
        let t = Instant::now();
        black_box(reparse(&base, &edit, &new).unwrap());
        tex_fast.push(t.elapsed().as_secs_f64() * 1000.);
        let t = Instant::now();
        black_box(parse_with_declarations_resolved(
            &new,
            LexConfig::default(),
            &decl,
        ));
        tex_full.push(t.elapsed().as_secs_f64() * 1000.);
    }
    let text = "@article{key,title={Research},year=2025}\n".repeat(20000) + "@article{probe,ti}\n";
    let parsed = bib::parse(&text);
    let at = text.rfind("ti}").unwrap();
    let edit = Edit {
        range: at..at + 2,
        insert: "author".into(),
    };
    let new = edit.apply(&text);
    let result = bib::reparse::reparse(&text, &parsed, &edit, &new).expect("BibTeX leaf splice");
    let full = bib::parse(&new);
    assert_eq!(result.green, full.green);
    assert_eq!(result.errors, full.errors);
    let mut bib_fast = vec![];
    let mut bib_full = vec![];
    for _ in 0..31 {
        let t = Instant::now();
        black_box(bib::reparse::reparse(&text, &parsed, &edit, &new).unwrap());
        bib_fast.push(t.elapsed().as_secs_f64() * 1000.);
        let t = Instant::now();
        black_box(bib::parse(&new));
        bib_full.push(t.elapsed().as_secs_f64() * 1000.);
    }
    let (tf, ts, bf, bs) = (
        median(tex_full),
        median(tex_fast),
        median(bib_full),
        median(bib_fast),
    );
    assert!(tf / ts > 5., "command speed floor: {tf}/{ts}");
    assert!(bf / bs > 5., "bib speed floor: {bf}/{bs}");
    println!(
        "{}",
        serde_json::json!({"command":{"tier":"Token","fullMs":tf,"reparseMs":ts,"speedup":tf/ts},"bib":{"tier":"Leaf","fullMs":bf,"reparseMs":bs,"speedup":bf/bs}})
    );
}
