use tex_ls_parser::parser::parse;

#[test]
fn deep_inputs_recover_losslessly_with_bounded_trees() {
    for closed in [false, true] {
        let tail = if closed {
            "}".repeat(10_000)
        } else {
            String::new()
        };
        for text in [
            format!("{}x{tail}", "{".repeat(10_000)),
            format!("${}x{tail}$", "{".repeat(10_000)),
            format!(
                "\\ExplSyntaxOn {}x{}",
                "\\use:n {".repeat(1000),
                "}".repeat(1000)
            ),
            format!("{}x{}", "\\begin{a}".repeat(1000), "\\end{a}".repeat(1000)),
        ] {
            let parsed = parse(&text);
            assert_eq!(parsed.syntax().to_string(), text);
            assert!(
                parsed
                    .errors
                    .iter()
                    .any(|error| error.message.contains("nesting depth"))
            );
            assert!(
                parsed
                    .syntax()
                    .descendants()
                    .all(|node| node.ancestors().count() < 1024)
            );
        }
        let text = format!("@book{{key,title={{{}x{tail}}}}}", "{".repeat(10_000));
        let parsed = tex_ls_parser::bib::parse(&text);
        assert_eq!(parsed.syntax().to_string(), text);
        assert!(
            parsed
                .errors
                .iter()
                .any(|error| error.message.contains("nesting depth"))
        );
    }
}

#[test]
fn incremental_edits_near_the_nesting_budget_match_full_parses() {
    use tex_ls_parser::declarations::ResolvedDeclarations;
    use tex_ls_parser::parser::{
        Edit, LexConfig, ReparseBase, parse_with_declarations_resolved, reparse,
    };
    let declared = ResolvedDeclarations::default();
    for outer in [0, 40, 100, 140] {
        let text = format!("{}$x${}", "{".repeat(outer), "}".repeat(outer));
        let config = LexConfig::default();
        let (parsed, ctx) = parse_with_declarations_resolved(&text, config, &declared);
        let base = ReparseBase::from_parts(
            &text,
            &parsed.green,
            &parsed.errors,
            &ctx,
            config,
            &declared,
        );
        for inner in [1, 40, 100, 200] {
            let edit = Edit {
                range: outer + 1..outer + 2,
                insert: format!("{}y{}", "{".repeat(inner), "}".repeat(inner)),
            };
            let next = edit.apply(&text);
            let full = parse(&next);
            if let Some(incremental) = reparse(&base, &edit, &next) {
                assert_eq!(incremental.green, full.green);
                assert_eq!(incremental.errors, full.errors);
            }
        }
    }
}
