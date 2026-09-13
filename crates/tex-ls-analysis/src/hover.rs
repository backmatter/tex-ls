//! Signature targets and lookup policy shared by feature presentation.
use rowan::{TextRange, TextSize};
use smol_str::SmolStr;
use tex_ls_parser::semantic::signature::{CommandSig, EnvironmentSig, SignatureDb, builtin, cwl};
use tex_ls_parser::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
pub enum TargetKind {
    Command,
    Environment,
}

/// A signature-hoverable construct under the cursor: a command name or an
/// environment name, with the byte range to highlight.
pub struct SigTarget {
    pub kind: TargetKind,
    pub name: String,
    pub range: TextRange,
}

/// The command/environment name token the cursor sits on, if any: a `CONTROL_WORD`
/// child of a `COMMAND`, or a name token inside the `NAME_GROUP` of a
/// `\begin`/`\end`. Mirrors completion's `command_name_context`/`group_context`, but
/// over a *complete* construct rather than a typed prefix.
pub fn signature_target_at(root: &SyntaxNode, offset: usize) -> Option<SigTarget> {
    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    let (left, right) = match root.token_at_offset(at) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => (Some(t.clone()), Some(t)),
        rowan::TokenAtOffset::Between(l, r) => (Some(l), Some(r)),
    };

    for token in [left, right].into_iter().flatten() {
        if token.kind() == SyntaxKind::CONTROL_WORD
            && let Some(parent) = token.parent()
            && parent.kind() == SyntaxKind::COMMAND
        {
            return Some(SigTarget {
                kind: TargetKind::Command,
                name: token.text().trim_start_matches('\\').to_string(),
                range: token.text_range(),
            });
        }
        if let Some(target) = environment_target(&token) {
            return Some(target);
        }
    }
    None
}

/// The `NAME_GROUP` of a `\begin`/`\end` an enclosing-named `token` sits in: its
/// inner name text (`*`-suffix included) and the range of that inner text.
fn environment_target(token: &SyntaxToken) -> Option<SigTarget> {
    let group = token
        .parent_ancestors()
        .find(|n| n.kind() == SyntaxKind::NAME_GROUP)?;
    let parent = group.parent()?;
    if !matches!(parent.kind(), SyntaxKind::BEGIN | SyntaxKind::END) {
        return None;
    }
    let (name, range) = name_group_inner(&group)?;
    Some(SigTarget {
        kind: TargetKind::Environment,
        name,
        range,
    })
}

/// The inner text of a `NAME_GROUP` (the `{name}` minus its braces) and that text's
/// byte range. Concatenates the non-brace tokens so a starred name (`figure*`)
/// reassembles. `None` for an empty group.
fn name_group_inner(group: &SyntaxNode) -> Option<(String, TextRange)> {
    let mut text = String::new();
    let mut start = None;
    let mut end = None;
    for token in group.children_with_tokens().filter_map(|e| e.into_token()) {
        match token.kind() {
            SyntaxKind::L_BRACE | SyntaxKind::R_BRACE => {}
            _ => {
                let r = token.text_range();
                start.get_or_insert(r.start());
                end = Some(r.end());
                text.push_str(token.text());
            }
        }
    }
    Some((text, TextRange::new(start?, end?)))
}

/// Where a resolved signature came from, for the rendered provenance label:
/// the static tiers (built-in/CWL), the document's own definitions, or a loaded
/// local package (whose file stem the merge recorded).
pub enum Provenance {
    Base,
    Document,
    Package(SmolStr),
}

pub fn lookup_command<'a>(
    scope: &'a SignatureDb,
    name: &str,
) -> Option<(&'a CommandSig, Provenance)> {
    if let Some(sig) = scope.command(name) {
        let provenance = match scope.command_origin(name) {
            Some(origin) => Provenance::Package(SmolStr::from(origin)),
            None => Provenance::Document,
        };
        return Some((sig, provenance));
    }
    builtin()
        .command(name)
        .or_else(|| cwl().command(name))
        .map(|sig| (sig, Provenance::Base))
}

/// An environment signature, with the same tiering as [`lookup_command`].
pub fn lookup_environment<'a>(
    scope: &'a SignatureDb,
    name: &str,
) -> Option<(&'a EnvironmentSig, Provenance)> {
    if let Some(sig) = scope.environment(name) {
        let provenance = match scope.environment_origin(name) {
            Some(origin) => Provenance::Package(SmolStr::from(origin)),
            None => Provenance::Document,
        };
        return Some((sig, provenance));
    }
    builtin()
        .environment(name)
        .or_else(|| cwl().environment(name))
        .map(|sig| (sig, Provenance::Base))
}
