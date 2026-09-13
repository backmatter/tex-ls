//! Measure touching edit composition against the previous per-edit replay.
use std::{hint::black_box, time::Instant};
use tex_ls_parser::{
    bib,
    declarations::ResolvedDeclarations,
    parser::{
        Edit, LexConfig, ReparseBase, ReparseTier, apply_edits, edit::coalesce_touching_edits,
        parse_with_declarations_resolved, reparse_edits,
    },
};

fn sample(mut f: impl FnMut()) -> f64 {
    let mut times = Vec::new();
    for _ in 0..31 {
        let t = Instant::now();
        f();
        times.push(t.elapsed().as_secs_f64() * 1000.);
    }
    times.sort_by(f64::total_cmp);
    times[15]
}
fn typing(at: usize, old_len: usize) -> Vec<Edit> {
    "research"
        .chars()
        .enumerate()
        .map(|(i, c)| Edit {
            range: at + i..at + if i == 0 { old_len } else { i },
            insert: c.to_string(),
        })
        .collect()
}
fn main() {
    if cfg!(debug_assertions) {
        eprintln!("run in release mode");
        std::process::exit(2);
    }
    let declared = ResolvedDeclarations::default();
    let config = LexConfig::default();
    let text = "Ordinary research prose.\n".repeat(40000) + "\\sec\n";
    let (parsed, ctx) = parse_with_declarations_resolved(&text, config, &declared);
    let base = ReparseBase::from_parts(
        &text,
        &parsed.green,
        &parsed.errors,
        &ctx,
        config,
        &declared,
    );
    let edits = typing(text.rfind("sec").unwrap(), 3);
    let next = apply_edits(&text, &edits);
    let result = reparse_edits(&base, &edits, &next).unwrap();
    assert_eq!(result.tier, ReparseTier::Token);
    let (full, _) = parse_with_declarations_resolved(&next, config, &declared);
    assert_eq!(result.green, full.green);
    assert_eq!(result.errors, full.errors);
    let fast = sample(|| {
        black_box(reparse_edits(&base, &edits, &next).unwrap());
    });
    // Reproduce the previous replay algorithm, using the same compiler and tree.
    let replay = sample(|| {
        let mut current = text.clone();
        let mut green = parsed.green.clone();
        let mut errors = parsed.errors.clone();
        for edit in &edits {
            let next = edit.apply(&current);
            let step = ReparseBase::from_parts(&current, &green, &errors, &ctx, config, &declared);
            let result = reparse_edits(&step, std::slice::from_ref(edit), &next).unwrap();
            current = next;
            green = result.green;
            errors = result.errors;
        }
        black_box((current, green, errors));
    });
    assert!(
        replay / fast > 2.,
        "local edit batch speed floor: {replay}/{fast}"
    );
    let text = "@article{key,title={Research},year=2025}\n".repeat(20000) + "@article{probe,ti}\n";
    let parsed = bib::parse(&text);
    let edits = typing(text.rfind("ti}").unwrap(), 2);
    let next = apply_edits(&text, &edits);
    let merged = coalesce_touching_edits(&text, &edits).unwrap();
    let result = bib::reparse::reparse(&text, &parsed, &merged, &next).unwrap();
    let full = bib::parse(&next);
    assert_eq!(result.green, full.green);
    assert_eq!(result.errors, full.errors);
    let bib_fast = sample(|| {
        let merged = coalesce_touching_edits(&text, &edits).unwrap();
        black_box(bib::reparse::reparse(&text, &parsed, &merged, &next).unwrap());
    });
    let bib_full = sample(|| {
        black_box(bib::parse(&next));
    });
    assert!(
        bib_full / bib_fast > 5.,
        "bib batch speed floor: {bib_full}/{bib_fast}"
    );
    println!(
        "{}",
        serde_json::json!({
            "edits": edits.len(),
            "latex": {"tier":"Token", "replayMs":replay, "coalescedMs":fast, "speedup":replay/fast},
            "bib": {"tier":"Leaf", "fullMs":bib_full, "coalescedMs":bib_fast, "speedup":bib_full/bib_fast}
        })
    );
}
