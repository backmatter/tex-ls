//! `textDocument/signatureHelp` computation: the signature of the command or
//! environment whose `{…}`/`[…]` argument the cursor is typing in, with the
//! active argument highlighted.
//!
//! The signature DB carries no argument names ([`ArgSpec`] is
//! `{required, kind, content}`), so the label uses TeX-idiomatic `#n`
//! placeholders — `\frac{#1}{#2}`, `\sqrt[#1]{#2}`, `\begin{tabular}[#1]{#2}` —
//! with each delimited chunk (`[#1]`, `{#2}`) as one parameter, so the client's
//! highlight shows the delimiter kind. The lookup tiers scope-first then
//! built-in then CWL, like hover.
//!
//! **Active parameter.** The parser attaches trailing groups greedily
//! (AGENTS.md decision #8), and authored optionals may be omitted, so the
//! cursor's argument node is *aligned* against the signature's slots rather
//! than counted: each source `GROUP`/`OPTIONAL` consumes the next slot of its
//! own bracket kind, skipping over omitted optional slots but never over a
//! pending required one. An argument that matches no slot (a group beyond the
//! declared arity, a `[…]` where a `{…}` is required) suppresses the help
//! entirely — never a clamped index, which would highlight a wrong parameter
//! (clients render a missing `activeParameter` as 0).
//!
//! Like hover, the read runs off the snapshot's cached parse when the tracked
//! source belongs to the captured project. Cancellation propagates to the host.

use super::*;
use crate::hover::{arg_summary, lookup_command, lookup_environment, provenance_label};
use lsp_types::{
    Documentation, ParameterInformation, ParameterInformationLabel, SignatureHelp,
    SignatureInformation,
};
use tex_ls_parser::ast::{command_name, environment_name};
use tex_ls_parser::semantic::signature::{ArgKind, ArgSpec, match_arg_slot_index};
use tex_ls_parser::syntax::{SyntaxKind, SyntaxToken};

/// Describe the argument using the snapshot's package-aware signature scope.
pub fn compute_signature_help(
    snapshot: &Analysis,
    path: &Path,
    position: Position,
    enc: PositionEncoding,
) -> Option<SignatureHelp> {
    let file = snapshot.lookup_file(path)?;
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);
    let root = snapshot.parsed_tree(file);
    let scope = snapshot.scope_signatures(file);
    let gap_help = || {
        let at = TextSize::from(offset as u32);
        let owner = root
            .descendants()
            .filter(|node| {
                matches!(node.kind(), SyntaxKind::COMMAND | SyntaxKind::BEGIN)
                    && node.text_range().contains_inclusive(at)
            })
            .min_by_key(|node| node.text_range().len())?;
        let (prefix, args, provenance) = if owner.kind() == SyntaxKind::BEGIN {
            let name = environment_name(&owner)?;
            let (sig, provenance) = lookup_environment(scope, &name)?;
            (
                format!("\\begin{{{name}}}"),
                sig.args.clone(),
                provenance_label(&provenance, "environment"),
            )
        } else {
            let name = command_name(&owner)?;
            let (sig, provenance) = lookup_command(scope, &name)?;
            (
                format!("\\{name}"),
                sig.args.clone(),
                provenance_label(&provenance, "command"),
            )
        };
        let groups: Vec<_> = owner
            .children()
            .filter(|node| matches!(node.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL))
            .collect();
        // Only a proved gap between authored arguments, never prose after a call.
        let pair = groups
            .windows(2)
            .find(|pair| pair[0].text_range().end() <= at && at <= pair[1].text_range().start())?;
        active_parameter(&args, &owner, &pair[0])?;
        active_parameter(&args, &owner, &pair[1])?;
        let mut help = render_help(&prefix, &args, provenance, 0);
        help.active_parameter = Some(lsp_types::ActiveParameter::Null);
        help.signatures[0].active_parameter = Some(lsp_types::ActiveParameter::Null);
        Some(help)
    };
    // A nested call's argument gap takes precedence over its enclosing argument.
    gap_help().or_else(|| signature_help_at(&root, scope, offset))
}

/// The shared body: find the argument node under the cursor, look up its owner's
/// signature, align the argument to a slot, and render.
fn signature_help_at(
    root: &SyntaxNode,
    scope: &SignatureDb,
    offset: usize,
) -> Option<SignatureHelp> {
    let target = argument_target_at(root, offset)?;
    let (prefix, args, provenance) = match target.kind {
        OwnerKind::Command => {
            let name = command_name(&target.owner)?;
            let (sig, provenance) = lookup_command(scope, &name)?;
            (
                format!("\\{name}"),
                sig.args.clone(),
                provenance_label(&provenance, "command"),
            )
        }
        OwnerKind::Environment => {
            let name = environment_name(&target.owner)?;
            let (sig, provenance) = lookup_environment(scope, &name)?;
            (
                format!("\\begin{{{name}}}"),
                sig.args.clone(),
                provenance_label(&provenance, "environment"),
            )
        }
    };
    let active = active_parameter(&args, &target.owner, &target.group)?;
    Some(render_help(&prefix, &args, provenance, active))
}

/// Whose argument the cursor is in: a `COMMAND`'s or a `\begin`'s.
enum OwnerKind {
    Command,
    Environment,
}

/// The argument node the cursor is typing in: the owning `COMMAND`/`BEGIN` node
/// and the `GROUP`/`OPTIONAL` child holding the cursor.
struct ArgTarget {
    owner: SyntaxNode,
    kind: OwnerKind,
    group: SyntaxNode,
}

/// The `GROUP`/`OPTIONAL` argument enclosing `offset`, if its parent is a
/// `COMMAND` or `BEGIN` and the offset sits strictly *inside* the brackets.
/// Innermost wins for nested commands (`\frac{\sqrt{x|}}{y}` finds `\sqrt`'s
/// group); math script groups (parent `SUPERSCRIPT`/`SUBSCRIPT`), the
/// `\\[2ex]` optional (parent `LINE_BREAK`), `NAME_GROUP`s, and verbatim
/// bodies (single tokens, never a `GROUP`) all fail the parent check.
fn argument_target_at(root: &SyntaxNode, offset: usize) -> Option<ArgTarget> {
    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    let (left, right) = match root.token_at_offset(at) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => (Some(t.clone()), Some(t)),
        rowan::TokenAtOffset::Between(l, r) => (Some(l), Some(r)),
    };

    for token in [left, right].into_iter().flatten() {
        let Some(group) = enclosing_argument(&token) else {
            continue;
        };
        if !cursor_inside(&group, at) {
            continue;
        }
        let Some(parent) = group.parent() else {
            continue;
        };
        let kind = match parent.kind() {
            SyntaxKind::COMMAND => OwnerKind::Command,
            SyntaxKind::BEGIN => OwnerKind::Environment,
            _ => continue,
        };
        return Some(ArgTarget {
            owner: parent,
            kind,
            group,
        });
    }
    None
}

/// The nearest argument of a command/environment containing `token`. Ordinary
/// nested groups are transparent: `\frac{{x}}{y}` still edits the first argument.
/// Command, environment and script boundaries stop the search so a token never
/// binds to an unrelated enclosing argument.
fn enclosing_argument(token: &SyntaxToken) -> Option<SyntaxNode> {
    let mut node = token.parent();
    while let Some(current) = node {
        match current.kind() {
            SyntaxKind::GROUP | SyntaxKind::OPTIONAL
                if current.parent().is_some_and(|parent| {
                    matches!(parent.kind(), SyntaxKind::COMMAND | SyntaxKind::BEGIN)
                }) =>
            {
                return Some(current);
            }
            SyntaxKind::COMMAND
            | SyntaxKind::BEGIN
            | SyntaxKind::END
            | SyntaxKind::NAME_GROUP
            | SyntaxKind::ENVIRONMENT
            | SyntaxKind::SUPERSCRIPT
            | SyntaxKind::SUBSCRIPT
            | SyntaxKind::LINE_BREAK
            | SyntaxKind::ROOT => return None,
            _ => node = current.parent(),
        }
    }
    None
}

/// Whether `at` sits strictly inside `group`'s brackets: past the opener, and
/// before the closer when the group is closed. An *unclosed* group (mid-typing
/// `\frac{` — the parser keeps it in the tree to EOF/recovery) admits its end
/// offset, so help shows right after the `{` is typed. Sitting exactly *between*
/// two arguments (`\frac{a}|{b}`) is inside neither, dismissing the popup.
fn cursor_inside(group: &SyntaxNode, at: TextSize) -> bool {
    let range = group.text_range();
    if at <= range.start() {
        return false;
    }
    let closer = match group.kind() {
        SyntaxKind::OPTIONAL => SyntaxKind::R_BRACKET,
        _ => SyntaxKind::R_BRACE,
    };
    let closed = matches!(
        group.last_child_or_token(),
        Some(rowan::NodeOrToken::Token(t)) if t.kind() == closer
    );
    if closed {
        at < range.end()
    } else {
        at <= range.end()
    }
}

/// Align `owner`'s source arguments against the signature's `specs` and return
/// the slot index of the `cursor` argument, or `None` when it matches no slot
/// (suppress — see the module docs). Each source `GROUP`/`OPTIONAL` consumes the
/// next spec of its bracket kind, skipping omitted *optional* slots of the other
/// kind; a mismatch against a pending *required* slot leaves that slot unconsumed
/// so a spurious argument never shifts its successors.
fn active_parameter(specs: &[ArgSpec], owner: &SyntaxNode, cursor: &SyntaxNode) -> Option<u32> {
    let mut next = 0usize;
    for node in owner
        .children()
        .filter(|c| matches!(c.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL))
    {
        let kind = match node.kind() {
            SyntaxKind::GROUP => ArgKind::Brace,
            _ => ArgKind::Bracket,
        };
        let slot = match_arg_slot_index(specs, &mut next, kind);
        if &node == cursor {
            return slot.map(|s| s as u32);
        }
    }
    None
}

/// Render the one-signature [`SignatureHelp`]: `prefix` + one `[#n]`/`{#n}` chunk
/// per slot, each chunk a UTF-16 range within the signature string, and a facts
/// line as documentation. ResponsePolicy converts ranges to strings when the
/// client lacks parameter label offset support.
fn render_help(prefix: &str, specs: &[ArgSpec], provenance: String, active: u32) -> SignatureHelp {
    let mut label = prefix.to_string();
    let mut parameters = Vec::with_capacity(specs.len());
    // These index a protocol string, not a document Position.
    let mut pos = prefix.encode_utf16().count() as u32;
    for (i, spec) in specs.iter().enumerate() {
        let chunk = match spec.kind {
            ArgKind::Brace => format!("{{#{}}}", i + 1),
            ArgKind::Bracket => format!("[#{}]", i + 1),
        };
        let len = chunk.len() as u32;
        parameters.push(ParameterInformation {
            label: ParameterInformationLabel::Tuple((pos, pos + len)),
            documentation: (provenance == "command")
                .then(|| {
                    crate::source_cards::argument_docs(prefix.trim_start_matches('\\'))
                        .get(i)
                        .copied()
                })
                .flatten()
                .map(|doc| Documentation::String(doc.into())),
        });
        label.push_str(&chunk);
        pos += len;
    }

    let mut facts = vec![provenance];
    if let Some(summary) = arg_summary(specs) {
        facts.push(summary);
    }
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: Some(Documentation::String(facts.join(" · "))),
            parameters: Some(parameters),
            active_parameter: Some(active.into()),
        }],
        active_signature: Some(0),
        active_parameter: Some(active.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    /// Signature help with the cursor `delta` bytes past the start of `needle`.
    fn help_at(src: &str, needle: &str, delta: usize) -> Option<SignatureHelp> {
        let offset = src.find(needle).expect("needle present") + delta;
        help_at_offset(src, offset)
    }

    fn help_at_offset(src: &str, offset: usize) -> Option<SignatureHelp> {
        let path = Path::new(fixture_path!("/p/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.to_string());
        let snapshot = db.snapshot();
        let idx = LineIndex::new(src);
        let (line, character) = idx.position(offset);
        compute_signature_help(
            &snapshot,
            path,
            Position { line, character },
            PositionEncoding::Utf16,
        )
    }

    /// The rendered label and active parameter, for the common assertions.
    fn active_at(src: &str, needle: &str, delta: usize) -> Option<(String, u32)> {
        let help = help_at(src, needle, delta)?;
        let sig = help.signatures.first().expect("one signature");
        Some((
            sig.label.clone(),
            match help.active_parameter.expect("active") {
                lsp_types::ActiveParameter::Int(value) => value,
                other => panic!("expected parameter index: {other:?}"),
            },
        ))
    }

    #[test]
    fn frac_second_group_is_active() {
        let (label, active) = active_at("\\frac{a}{b}\n", "{b", 2).expect("help");
        assert_eq!(label, "\\frac{#1}{#2}");
        assert_eq!(active, 1);
    }

    #[test]
    fn parameter_offsets_cover_delimited_chunks() {
        let help = help_at("\\frac{a}{b}\n", "{a", 1).expect("help");
        let params = help.signatures[0].parameters.as_ref().expect("params");
        let offsets: Vec<_> = params
            .iter()
            .map(|p| match p.label {
                ParameterInformationLabel::Tuple((start, end)) => [start, end],
                ref other => panic!("expected offsets, got {other:?}"),
            })
            .collect();
        // `\frac` is 5 units; `{#1}` and `{#2}` are 4 each.
        assert_eq!(offsets, vec![[5, 9], [9, 13]]);
    }

    #[test]
    fn brace_skips_omitted_optional_slot() {
        // `\sqrt` is `[opt]{req}`; with the optional omitted the brace group is
        // still slot 1.
        let (label, active) = active_at("\\sqrt{x}\n", "{x", 2).expect("help");
        assert_eq!(label, "\\sqrt[#1]{#2}");
        assert_eq!(active, 1);
    }

    #[test]
    fn authored_optional_takes_its_own_slot() {
        let src = "\\sqrt[3]{x}\n";
        assert_eq!(active_at(src, "[3", 2).expect("help").1, 0);
        assert_eq!(active_at(src, "{x", 2).expect("help").1, 1);
    }

    #[test]
    fn requiredness_controls_cross_delimiter_matching() {
        let optional_brace = "\\NewDocumentCommand{\\foo}{d{} r[] m}{#1#2#3}\n\\foo[x]{y}\n";
        assert_eq!(active_at(optional_brace, "[x", 2).expect("help").1, 1);
        assert_eq!(active_at(optional_brace, "{y", 2).expect("help").1, 2);

        let required_bracket = "\\NewDocumentCommand{\\foo}{r[] m}{#1#2}\n\\foo{x}\n";
        assert_eq!(active_at(required_bracket, "{x", 2), None);
    }

    #[test]
    fn includegraphics_optional_is_first_slot() {
        let src = "\\includegraphics[width=2cm]{fig}\n";
        assert_eq!(active_at(src, "[width", 6).expect("help").1, 0);
        assert_eq!(active_at(src, "{fig", 2).expect("help").1, 1);
    }

    #[test]
    fn unclosed_group_mid_typing_shows_first_slot() {
        // The freshly-typed `{` parses as an unclosed group kept to EOF; the
        // cursor at its end is still inside.
        let (label, active) = active_at("\\frac{", "{", 1).expect("help");
        assert_eq!(label, "\\frac{#1}{#2}");
        assert_eq!(active, 0);
    }

    #[test]
    fn empty_group_shows_its_slot() {
        assert_eq!(active_at("\\frac{}{b}\n", "{}", 1).expect("help").1, 0);
    }

    #[test]
    fn group_beyond_declared_arity_suppresses() {
        // `\label` takes one argument; the greedily-attached second group must
        // not highlight anything.
        assert_eq!(active_at("\\label{a}{b}\n", "{b", 2), None);
    }

    #[test]
    fn zero_arg_command_suppresses() {
        assert_eq!(active_at("\\alpha{x}\n", "{x", 2), None);
    }

    #[test]
    fn spurious_optional_suppresses_without_shifting_braces() {
        // `\frac` has no bracket slot: `[x]` matches nothing, but it must not
        // consume the pending required slot — `{a}` is still slot 0.
        let src = "\\frac[x]{a}{b}\n";
        assert_eq!(active_at(src, "[x", 2), None);
        assert_eq!(active_at(src, "{a", 2).expect("help").1, 0);
    }

    #[test]
    fn nested_command_innermost_wins() {
        let (label, active) = active_at("\\frac{\\sqrt{x}}{y}\n", "{x", 2).expect("help");
        assert_eq!(label, "\\sqrt[#1]{#2}");
        assert_eq!(active, 1);
    }

    #[test]
    fn nested_groups_keep_the_owning_argument_active() {
        for (src, needle, expected_label, expected_slot) in [
            ("\\frac{{a}}{b}\n", "{a", "\\frac{#1}{#2}", 0),
            ("\\frac{a}{{{b}}}\n", "{b", "\\frac{#1}{#2}", 1),
            ("\\sqrt[{3}]{x}\n", "{3", "\\sqrt[#1]{#2}", 0),
            ("\\frac{{a", "{a", "\\frac{#1}{#2}", 0),
        ] {
            assert_eq!(
                active_at(src, needle, 2),
                Some((expected_label.into(), expected_slot)),
                "source: {src}"
            );
        }
    }

    #[test]
    fn nested_groups_still_choose_the_innermost_command() {
        assert_eq!(
            active_at("\\frac{{\\sqrt{{x}}}}{y}\n", "{x", 2),
            Some(("\\sqrt[#1]{#2}".into(), 1))
        );
    }

    #[test]
    fn nested_groups_do_not_cross_unrelated_argument_boundaries() {
        for (src, needle) in [
            ("{{plain}}\n", "{plain"),
            ("\\label{key}{{extra}}\n", "{extra"),
            ("\\frac{\\unknown{{x}}}{y}\n", "{x"),
            ("\\frac{$x^{{2}}$}{y}\n", "{2"),
            ("\\frac{$x_{{2}}$}{y}\n", "{2"),
        ] {
            assert_eq!(active_at(src, needle, 2), None, "source: {src}");
        }
    }

    #[test]
    fn between_arguments_is_null_only_for_capable_clients() {
        let help = help_at("\\frac{a}{b}\n", "}{", 1).unwrap();
        assert_eq!(
            help.active_parameter,
            Some(lsp_types::ActiveParameter::Null)
        );
        let mut minimal = serde_json::to_value(&help).unwrap();
        crate::ResponsePolicy::default().response("textDocument/signatureHelp", None, &mut minimal);
        assert!(minimal.is_null());
        let policy = crate::ResponsePolicy::new(
            &serde_json::json!({"capabilities":{"textDocument":{"signatureHelp":{"signatureInformation":{"noActiveParameterSupport":true}}}}}),
        );
        let mut rich = serde_json::to_value(help).unwrap();
        policy.response("textDocument/signatureHelp", None, &mut rich);
        assert!(rich.is_object());
        assert_eq!(rich["activeParameter"], serde_json::Value::Null);
        assert!(help_at("\\label{a}{b}\n", "}{", 1).is_none());
    }

    #[test]
    fn no_active_parameter_uses_innermost_command_and_environment() {
        for (source, needle, label) in [
            ("\\frac{\\frac{a} {b}}{c}", "} {", "\\frac{#1}{#2}"),
            (
                "\\begin{tabular}[t] {cc}\n\\end{tabular}",
                "] {",
                "\\begin{tabular}[#1]{#2}",
            ),
        ] {
            let help = help_at(source, needle, 1).expect("known argument gap");
            assert_eq!(help.signatures[0].label, label);
            assert_eq!(
                help.active_parameter,
                Some(lsp_types::ActiveParameter::Null)
            );
        }
    }

    #[test]
    fn environment_begin_arguments() {
        let src = "\\begin{tabular}{cc}\na & b\n\\end{tabular}\n";
        let (label, active) = active_at(src, "{cc", 2).expect("help");
        assert_eq!(label, "\\begin{tabular}[#1]{#2}");
        assert_eq!(active, 1);
    }

    #[test]
    fn name_groups_suppress() {
        let src = "\\begin{tabular}{cc}\na & b\n\\end{tabular}\n";
        assert_eq!(active_at(src, "{tabular", 4), None);
        assert_eq!(active_at(src, "\\end{tab", 7), None);
    }

    #[test]
    fn verb_body_suppresses() {
        assert_eq!(active_at("\\verb|x|\n", "|x", 1), None);
    }

    #[test]
    fn math_script_group_suppresses() {
        assert_eq!(active_at("$x^{2}$\n", "{2", 2), None);
    }

    #[test]
    fn user_defined_command_resolves_from_scope() {
        let src = "\\newcommand{\\foo}[2]{#1#2}\n\\foo{a}{b}\n";
        let help = help_at(src, "{b", 2).expect("help");
        let sig = &help.signatures[0];
        assert_eq!(sig.label, "\\foo{#1}{#2}");
        assert_eq!(help.active_parameter, Some(1.into()));
        match sig.documentation.as_ref().expect("docs") {
            Documentation::String(s) => {
                assert!(s.contains("user-defined command"), "provenance: {s}")
            }
            other => panic!("expected string docs, got {other:?}"),
        }
    }
}
