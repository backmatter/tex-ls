//! Parse one source and render its CST.
use super::inputs::{Inputs, PARSE_MISSING_INPUT, inputs_or_exit};
use rowan::NodeOrToken;
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::ExitCode,
};
use tex_ls_analysis::source::file_kind_or_tex;
use tex_ls_parser::{
    declarations::ResolvedDeclarations,
    parser::{LexConfig, parse_with_declarations},
    syntax::SyntaxNode,
};

/// Parse a single file (or stdin) and print its CST to stdout. Parse errors are
/// printed after the tree; the command exits non-zero if any are reported.
pub(super) fn run_parse(paths: &[PathBuf], declared: &ResolvedDeclarations) -> ExitCode {
    // Clap caps `parse` at one positional, so the slice holds at most one path.
    let path = match inputs_or_exit(paths, "parse", PARSE_MISSING_INPUT) {
        Inputs::Stdin => None,
        Inputs::Paths(paths) => Some(paths[0].as_path()),
    };
    let input = match path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) => {
                eprintln!("tex-ls: cannot read {}: {err}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => {
            let mut input = String::new();
            if let Err(err) = std::io::stdin().read_to_string(&mut input) {
                eprintln!("tex-ls: cannot read stdin: {err}");
                return ExitCode::FAILURE;
            }
            input
        }
    };

    let config = path.map_or(LexConfig::default(), |p| file_kind_or_tex(p).lex_config());
    let parsed = parse_with_declarations(&input, config, declared);
    let mut out = String::new();
    render_cst(&parsed.syntax(), 0, &mut out);
    if let Err(err) = std::io::stdout().write_all(out.as_bytes()) {
        eprintln!("tex-ls: cannot write stdout: {err}");
        return ExitCode::FAILURE;
    }

    if parsed.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        for err in &parsed.errors {
            eprintln!("error @{}..{}: {}", err.start, err.end, err.message);
        }
        ExitCode::FAILURE
    }
}

/// Render a CST as an indented `KIND@range` tree, with token text. Kept in sync
/// with the test renderer in `tests/parser.rs`.
fn render_cst(node: &SyntaxNode, depth: usize, out: &mut String) {
    out.push_str(&format!(
        "{:indent$}{:?}@{:?}\n",
        "",
        node.kind(),
        node.text_range(),
        indent = depth * 2
    ));
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Node(n) => render_cst(&n, depth + 1, out),
            NodeOrToken::Token(t) => out.push_str(&format!(
                "{:indent$}{:?}@{:?} {:?}\n",
                "",
                t.kind(),
                t.text_range(),
                t.text(),
                indent = (depth + 1) * 2
            )),
        }
    }
}
