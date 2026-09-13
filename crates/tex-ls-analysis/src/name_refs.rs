//! Name-based navigation targets: user **command names** (`\mycmd`) and
//! **environment names** (`\begin{myenv}`), the fallback tier of
//! references/rename/goto-definition behind the label/citation key resolution.
//!
//! Occurrence ranges always describe the current source revision.
//! Definition sites come from [`scan_definition_sites`], the range-bearing sibling
//! of the signature scan. Tokens inside comments and verbatim bodies never parse
//! to `CONTROL_WORD`/`BEGIN`, so protected regions are skipped by construction.
//!
//! [`scan_definition_sites`]: tex_ls_parser::semantic::scan_definition_sites

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rowan::{TextRange, TextSize};
use smol_str::SmolStr;

use crate::project::{PackageGraph, ResolvedLabels};
use tex_ls_parser::ast::{environment_name, environment_name_range};
use tex_ls_parser::semantic::{DefSite, DefSiteKind};
use tex_ls_parser::syntax::{SyntaxKind, SyntaxNode};

/// Which TeX namespace a [`NameTarget`] lives in. Command and environment names
/// never collide: `\proof` and `\begin{proof}` are unrelated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    Command,
    Environment,
}

/// The command or environment name under the cursor: the bare name (no `\`), and
/// the span to report from `prepareRename` — the name text without the backslash,
/// so the prepare range, the placeholder, and the rename edits all agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameTarget {
    pub kind: NameKind,
    pub name: SmolStr,
    pub span: TextRange,
}

/// Resolve the name target at byte `offset`, trying both boundary tokens.
/// Precedence per token:
/// 1. any token inside a `BEGIN`/`END` (the `\begin` word, braces, or the name
///    itself) → the delimiter's environment name;
/// 2. a token inside the name argument of an environment *definition*
///    (`\newenvironment{myenv}`), matched by containment against `def_sites` —
///    plain `WORD` text a `BEGIN`/`END` walk can never see;
/// 3. a `CONTROL_WORD` → that command name (covers uses, the `\mycmd` inside
///    `\newcommand{\mycmd}`, and `\def\mycmd` alike).
///
/// A `CONTROL_SYMBOL` (`\\`, `\%`) is not a nameable target and falls through.
/// The caller runs label/citation resolution *first*; this is strictly the
/// fallback tier.
pub fn name_target_under_cursor(
    root: &SyntaxNode,
    offset: usize,
    def_sites: &[DefSite],
) -> Option<NameTarget> {
    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    let (left, right) = match root.token_at_offset(at) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => (Some(t.clone()), Some(t)),
        rowan::TokenAtOffset::Between(l, r) => (Some(l), Some(r)),
    };
    let tokens: Vec<_> = [left, right].into_iter().flatten().collect();

    for token in &tokens {
        if let Some(delimiter) = token
            .parent_ancestors()
            .find(|n| matches!(n.kind(), SyntaxKind::BEGIN | SyntaxKind::END))
        {
            let name = environment_name(&delimiter)?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            return Some(NameTarget {
                kind: NameKind::Environment,
                name: SmolStr::new(name),
                span: environment_name_range(&delimiter)?,
            });
        }
    }

    if let Some(site) = def_sites
        .iter()
        .filter(|site| site.kind == DefSiteKind::Environment)
        .find(|site| site.name_range.contains_inclusive(at))
    {
        return Some(NameTarget {
            kind: NameKind::Environment,
            name: site.name.clone(),
            span: site.name_range,
        });
    }

    tokens
        .iter()
        .find(|token| token.kind() == SyntaxKind::CONTROL_WORD)
        .and_then(|token| {
            let name = token.text().strip_prefix('\\')?;
            (!name.is_empty()).then(|| NameTarget {
                kind: NameKind::Command,
                name: SmolStr::new(name),
                span: strip_backslash(token.text_range()),
            })
        })
}

/// Every `\name` control-word token in `root`, as full token ranges (backslash
/// included, matching [`DefSite::name_range`] for commands so declaration
/// classification can compare ranges for equality). Definition-site names
/// (`\newcommand{\name}`, `\def\name`) are themselves `CONTROL_WORD` tokens, so
/// the walk finds them too.
pub fn command_occurrences(root: &SyntaxNode, name: &str) -> Vec<TextRange> {
    root.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            token.kind() == SyntaxKind::CONTROL_WORD
                && token.text().strip_prefix('\\') == Some(name)
        })
        .map(|token| token.text_range())
        .collect()
}

/// Every `\begin{name}`/`\end{name}` name span in `root`. **Name-based, not
/// pair-based** — each matching delimiter is collected independently (unlike the
/// pair-based change-environment refactor), so unbalanced files degrade
/// gracefully. Definition-site names (`\newenvironment{name}`) are *not* found
/// here; the caller folds those in from [`scan_definition_sites`].
///
/// [`scan_definition_sites`]: tex_ls_parser::semantic::scan_definition_sites
pub fn environment_occurrences(root: &SyntaxNode, name: &str) -> Vec<TextRange> {
    root.descendants()
        .filter(|node| matches!(node.kind(), SyntaxKind::BEGIN | SyntaxKind::END))
        .filter(|node| environment_name(node).as_deref().map(str::trim) == Some(name))
        .filter_map(|node| environment_name_range(&node))
        .collect()
}

/// Root candidates for a declared name, including package loading origins.
pub fn macro_roots(
    resolution: &ResolvedLabels,
    packages: &PackageGraph,
    origin: &Path,
) -> BTreeSet<PathBuf> {
    let mut loaders = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut pending = vec![origin.to_path_buf()];
    while let Some(path) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        if packages.loaded_by(&path).is_empty() {
            loaders.extend(resolution.candidate_roots(&path).iter().cloned());
        } else {
            pending.extend(packages.loaded_by(&path).iter().cloned());
        }
    }
    loaders
}

/// Root-local include members plus directed package dependencies. Reverse package
/// edges are used only to select a unique loading root for a package origin.
pub fn macro_namespace(
    resolution: &ResolvedLabels,
    packages: &PackageGraph,
    origin: &Path,
) -> Vec<PathBuf> {
    let loaders = macro_roots(resolution, packages, origin);
    let selected = if loaders.len() == 1 {
        loaders.first().unwrap().as_path()
    } else {
        origin
    };
    let mut seen = BTreeSet::new();
    let mut queue: Vec<_> = resolution
        .namespace_members(selected)
        .into_iter()
        .map(Path::to_path_buf)
        .chain(std::iter::once(origin.to_path_buf()))
        .collect();
    while let Some(path) = queue.pop() {
        if seen.insert(path.clone()) {
            queue.extend(packages.loads(&path).iter().map(|load| load.to.clone()));
        }
    }
    seen.into_iter().collect()
}

/// `range` with its leading backslash byte dropped — the bare-name span a rename
/// edit rewrites (the new name is inserted without a `\`, keeping the backslash
/// byte untouched).
pub fn strip_backslash(range: TextRange) -> TextRange {
    TextRange::new(
        (range.start() + TextSize::new(1)).min(range.end()),
        range.end(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_parser::parser::parse;
    use tex_ls_parser::semantic::scan_definition_sites;

    fn root_of(src: &str) -> SyntaxNode {
        SyntaxNode::new_root(parse(src).green)
    }

    fn target_at(src: &str, offset: usize) -> Option<NameTarget> {
        let root = root_of(src);
        let sites = scan_definition_sites(&root);
        name_target_under_cursor(&root, offset, &sites)
    }

    #[test]
    fn command_use_under_cursor() {
        let src = "text \\foo{x}\n";
        let target = target_at(src, 7).expect("a command target");
        assert_eq!(target.kind, NameKind::Command);
        assert_eq!(target.name, "foo");
        assert_eq!(&src[target.span], "foo");
    }

    #[test]
    fn command_name_inside_newcommand() {
        let src = "\\newcommand{\\foo}{x}\n";
        // Cursor on the `\foo` inside the name group.
        let target = target_at(src, 14).expect("a command target");
        assert_eq!(target.kind, NameKind::Command);
        assert_eq!(target.name, "foo");
    }

    #[test]
    fn environment_name_in_begin() {
        let src = "\\begin{myenv}\nx\n\\end{myenv}\n";
        let target = target_at(src, 9).expect("an environment target");
        assert_eq!(target.kind, NameKind::Environment);
        assert_eq!(target.name, "myenv");
        assert_eq!(&src[target.span], "myenv");
    }

    #[test]
    fn begin_control_word_resolves_to_environment() {
        let src = "\\begin{myenv}\nx\n\\end{myenv}\n";
        // Cursor on `\begin` itself: the delimiter names the environment.
        let target = target_at(src, 2).expect("an environment target");
        assert_eq!(target.kind, NameKind::Environment);
        assert_eq!(target.name, "myenv");
    }

    #[test]
    fn environment_definition_name_targets_environment() {
        let src = "\\newenvironment{myenv}{a}{b}\n";
        // Cursor inside the plain-text name argument.
        let target = target_at(src, 18).expect("an environment target");
        assert_eq!(target.kind, NameKind::Environment);
        assert_eq!(target.name, "myenv");
        assert_eq!(&src[target.span], "myenv");
    }

    #[test]
    fn control_symbol_and_prose_decline() {
        assert_eq!(target_at("a \\\\ b\n", 3), None);
        assert_eq!(target_at("plain text\n", 2), None);
    }

    #[test]
    fn command_occurrences_include_definition_and_uses() {
        let src = "\\newcommand{\\foo}{x}\n\\foo and \\foo{y} but not \\foobar\n";
        let ranges = command_occurrences(&root_of(src), "foo");
        assert_eq!(ranges.len(), 3);
        assert!(ranges.iter().all(|r| &src[*r] == "\\foo"));
    }

    #[test]
    fn command_occurrences_skip_verbatim_and_comments() {
        let src = "\\foo\n% \\foo in a comment\n\\begin{verbatim}\n\\foo\n\\end{verbatim}\n";
        let ranges = command_occurrences(&root_of(src), "foo");
        assert_eq!(ranges.len(), 1, "only the real token counts");
    }

    #[test]
    fn environment_occurrences_are_name_based() {
        // Unbalanced: two begins, one end — all three collected independently.
        let src = "\\begin{myenv}\n\\begin{myenv}\nx\n\\end{myenv}\n";
        let ranges = environment_occurrences(&root_of(src), "myenv");
        assert_eq!(ranges.len(), 3);
        assert!(ranges.iter().all(|r| &src[*r] == "myenv"));
    }
}

/// Current byte occurrences, extracted once and shared across name-based features.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameOccurrences {
    commands: std::collections::BTreeMap<SmolStr, Vec<TextRange>>,
    environments: std::collections::BTreeMap<SmolStr, Vec<TextRange>>,
}
impl NameOccurrences {
    pub fn build(root: &SyntaxNode) -> Self {
        let mut result = Self::default();
        for element in root.descendants_with_tokens() {
            match element {
                rowan::NodeOrToken::Token(token) if token.kind() == SyntaxKind::CONTROL_WORD => {
                    if let Some(name) = token.text().strip_prefix('\\') {
                        result
                            .commands
                            .entry(name.into())
                            .or_default()
                            .push(token.text_range());
                    }
                }
                rowan::NodeOrToken::Node(node)
                    if matches!(node.kind(), SyntaxKind::BEGIN | SyntaxKind::END) =>
                {
                    if let (Some(name), Some(range)) =
                        (environment_name(&node), environment_name_range(&node))
                    {
                        result
                            .environments
                            .entry(name.trim().into())
                            .or_default()
                            .push(range);
                    }
                }
                _ => {}
            }
        }
        result
    }
    pub fn command_names(&self) -> impl Iterator<Item = &str> {
        self.commands.keys().map(|name| name.as_str())
    }
    pub fn environment_names(&self) -> impl Iterator<Item = &str> {
        self.environments.keys().map(|name| name.as_str())
    }
    pub fn commands(&self, name: &str) -> &[TextRange] {
        self.commands.get(name).map_or(&[], Vec::as_slice)
    }
    pub fn environments(&self, name: &str) -> &[TextRange] {
        self.environments.get(name).map_or(&[], Vec::as_slice)
    }
}
