//! Literal file arguments, their exact byte spans, and ordered target candidates.
//!
//! Includes, package/class loads, bibliography resources, and graphics share
//! this extraction. Hosts acquire candidates; analysis resolves only supplied
//! observations. Unknown earlier candidates prevent a definitive fallback.

use std::path::{Path, PathBuf};

use rowan::{TextRange, TextSize};

use crate::completion::FileArgKind;
use crate::external::FileCandidates;
use crate::incremental::Analysis;
use crate::project::include::subfiles_parent_arg;
use tex_ls_parser::ast::{command_name, nth_group_inner};
use tex_ls_parser::syntax::{SyntaxKind, SyntaxNode};

/// Source facts that invalidate a built-in file argument interpretation.
/// Earlier calls retain their role; a local declaration makes later calls
/// unproved, without assuming that its replacement preserves the built-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRoleContext {
    protected: tex_ls_parser::semantic::context::TokenListContexts,
    operands: tex_ls_parser::semantic::context::PrimitiveOperandContexts,
    overrides: std::collections::BTreeMap<String, TextSize>,
    graphics: Vec<(TextSize, Vec<PathBuf>)>,
    commands_by_end: Vec<rowan::TextRange>,
}

impl FileRoleContext {
    pub fn new(root: &SyntaxNode) -> Self {
        Self::from_definition_sites(root, &tex_ls_parser::semantic::scan_definition_sites(root))
    }

    /// Reuse definition facts already collected by a feature request.
    pub fn from_definition_sites(
        root: &SyntaxNode,
        sites: &[tex_ls_parser::semantic::DefSite],
    ) -> Self {
        let operands =
            tex_ls_parser::semantic::context::PrimitiveOperandContexts::from_definition_sites(
                root, sites,
            );
        Self::from_definition_sites_and_operands(root, sites, &operands)
    }

    pub fn from_definition_sites_and_operands(
        root: &SyntaxNode,
        sites: &[tex_ls_parser::semantic::DefSite],
        operands: &tex_ls_parser::semantic::context::PrimitiveOperandContexts,
    ) -> Self {
        let mut overrides = std::collections::BTreeMap::new();
        for site in sites {
            if site.kind == tex_ls_parser::semantic::DefSiteKind::Command {
                overrides
                    .entry(site.name.to_string())
                    .and_modify(|start: &mut TextSize| {
                        *start = (*start).min(site.name_range.start())
                    })
                    .or_insert(site.name_range.start());
            }
        }
        let mut result = Self {
            protected: tex_ls_parser::semantic::context::TokenListContexts::new(root),
            operands: operands.clone(),
            overrides,
            graphics: Vec::new(),
            commands_by_end: Vec::new(),
        };
        use tex_ls_parser::ast::{AstNode, Group, nth_group};
        for command in root
            .descendants()
            .filter(|node| matches!(node.kind(), SyntaxKind::COMMAND | SyntaxKind::BEGIN))
        {
            result.commands_by_end.push(command.text_range());
            if command.kind() != SyntaxKind::COMMAND
                || command_name(&command).as_deref() != Some("graphicspath")
            {
                continue;
            }
            if !result.allows(&command) {
                continue;
            }
            let directories = nth_group(&command, 0)
                .into_iter()
                .flat_map(|group| group.children().filter_map(Group::cast).collect::<Vec<_>>())
                .filter_map(|entry| entry.inner().map(|(_, path)| PathBuf::from(path.trim())))
                .collect();
            result
                .graphics
                .push((command.text_range().start(), directories));
        }
        // Stable ordering preserves source-preorder ties for nested commands.
        result.commands_by_end.sort_by_key(|range| range.end());
        result
    }

    /// The last COMMAND/BEGIN ending strictly before a cursor, matching source
    /// preorder for equal ends. Cached source facts bound same-line fallback work.
    pub fn preceding_command_range(&self, at: TextSize) -> Option<rowan::TextRange> {
        let next = self
            .commands_by_end
            .partition_point(|range| range.end() < at);
        next.checked_sub(1).map(|index| self.commands_by_end[index])
    }

    pub(super) fn graphics_before(&self, start: TextSize) -> Option<&[PathBuf]> {
        let next = self.graphics.partition_point(|&(offset, _)| offset < start);
        next.checked_sub(1)
            .map(|index| self.graphics[index].1.as_slice())
    }

    pub fn allows(&self, command: &SyntaxNode) -> bool {
        !self.protected.contains(command.text_range())
            && !self.operands.contains_head(command)
            && !self.preceded_by_string(command)
            && command_name(command).is_some_and(|name| {
                self.overrides
                    .get(name.as_str())
                    .is_none_or(|start| command.text_range().start() < *start)
            })
    }

    pub fn is_operand_token(&self, range: TextRange) -> bool {
        self.operands.contains_token(range)
    }

    pub fn is_operand_head(&self, command: &SyntaxNode) -> bool {
        self.operands.contains_head(command)
    }

    fn preceded_by_string(&self, command: &SyntaxNode) -> bool {
        let mut previous = command.first_token().and_then(|token| token.prev_token());
        while let Some(token) = previous {
            if matches!(
                token.kind(),
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
            ) {
                previous = token.prev_token();
                continue;
            }
            return token.kind() == SyntaxKind::CONTROL_WORD
                && token.text().get(1..) == Some("string")
                && self
                    .overrides
                    .get("string")
                    .is_none_or(|start| token.text_range().start() < *start);
        }
        false
    }

    pub fn role(&self, command: &SyntaxNode) -> Option<tex_ls_parser::semantic::roles::FileRole> {
        if !self.allows(command) {
            return None;
        }
        command_name(command).and_then(|name| tex_ls_parser::semantic::roles::file_role(&name))
    }
}

/// A target established by the captured observations, with its source byte span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTarget {
    /// Byte range of the path argument (per comma-separated name) in the source.
    pub range: TextRange,
    /// The resolved logical target.
    pub target: PathBuf,
}

/// A literal target whose availability may still need host acquisition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileReference {
    pub range: TextRange,
    pub candidates: FileCandidates,
    /// Whether TeX executes this target as source. Asset and listing links are
    /// still acquired for navigation, but cannot contribute command definitions.
    pub loads_commands: bool,
}

pub fn document_links(
    root: &SyntaxNode,
    base_dir: Option<&Path>,
    inputs: &Analysis,
) -> Vec<LinkTarget> {
    file_references(root, base_dir, base_dir, &[])
        .into_iter()
        .filter_map(
            |reference| match inputs.resolve_file(&reference.candidates).target {
                crate::external::Observation::Present(target) => Some(LinkTarget {
                    range: reference.range,
                    target,
                }),
                _ => None,
            },
        )
        .collect()
}

pub fn file_references(
    root: &SyntaxNode,
    base_dir: Option<&Path>,
    root_dir: Option<&Path>,
    inherited_graphics: &[PathBuf],
) -> Vec<FileReference> {
    let roles = FileRoleContext::new(root);
    file_references_with_roles(root, base_dir, root_dir, inherited_graphics, &roles)
}

pub fn file_references_with_roles(
    root: &SyntaxNode,
    base_dir: Option<&Path>,
    root_dir: Option<&Path>,
    inherited_graphics: &[PathBuf],
    roles: &FileRoleContext,
) -> Vec<FileReference> {
    let mut links = Vec::new();
    for command in root
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::COMMAND)
        .filter(|node| roles.allows(node))
    {
        let Some(name) = command_name(&command) else {
            continue;
        };
        for (kind, range, name) in
            tex_ls_parser::semantic::roles::biblatex_style_arguments(&command)
        {
            if name.is_empty() {
                continue;
            }
            let FileArgKind::NamedFile { prefix, suffix } = FileArgKind::from_role(kind) else {
                continue;
            };
            let target = format!("{prefix}{name}{suffix}");
            let mut candidates = FileCandidates::new(
                &target,
                FileArgKind::from_role(kind).extensions(),
                false,
                None,
            );
            candidates.local = super::resolution::command_directories_with_roles(
                &command,
                base_dir,
                root_dir,
                inherited_graphics,
                roles,
            )
            .iter()
            .flat_map(|dir| {
                FileCandidates::new(
                    &target,
                    FileArgKind::from_role(kind).extensions(),
                    false,
                    Some(dir),
                )
                .local
            })
            .collect();
            links.push(FileReference {
                range,
                candidates,
                loads_commands: true,
            });
        }
        if name == "documentclass" {
            collect_subfiles_parent(&command, base_dir, &mut links);
        }
        let Some(class) = roles.role(&command) else {
            continue;
        };
        let directories = super::resolution::command_directories_with_roles(
            &command,
            base_dir,
            root_dir,
            inherited_graphics,
            roles,
        );
        collect_command(&command, class, &directories, &mut links);
    }
    links
}

/// Push the parent-document link of a `subfiles` class declaration, if any.
///
/// `\documentclass` is the one command here that names two files, and the
/// single-role-per-name dispatch cannot express that — hence the separate
/// pass. The gate and the span both come from
/// [`subfiles_parent_arg`], the same helper the include-graph edge extractor
/// uses, so a path that is clickable is exactly a path that is an edge.
fn collect_subfiles_parent(
    command: &SyntaxNode,
    base_dir: Option<&Path>,
    out: &mut Vec<FileReference>,
) {
    let Some(arg) = subfiles_parent_arg(command) else {
        return;
    };
    out.push(FileReference {
        range: arg.range,
        candidates: FileCandidates::new(&arg.text, &["tex"], false, base_dir),
        loads_commands: true,
    });
}

/// Emit candidates from the same role used by completion and source edges.
fn collect_command(
    command: &SyntaxNode,
    role: tex_ls_parser::semantic::roles::FileRole,
    directories: &[PathBuf],
    out: &mut Vec<FileReference>,
) {
    let kind = FileArgKind::from_role(role.kind);
    let dtx = matches!(
        kind,
        FileArgKind::Package | FileArgKind::NamedFile { suffix: ".sty", .. } | FileArgKind::Class
    );
    let names = file_argument_spans(command, role);
    for (name, range) in names {
        let name = match kind {
            FileArgKind::NamedFile { prefix, suffix } => format!("{prefix}{name}{suffix}"),
            _ => name,
        };
        let mut candidates = FileCandidates::new(&name, kind.extensions(), dtx, None);
        candidates.local = directories
            .iter()
            .flat_map(|dir| FileCandidates::new(&name, kind.extensions(), dtx, Some(dir)).local)
            .collect();
        if role.directory.is_some() {
            candidates.installed = None;
        }
        let loads_commands = matches!(
            role.kind,
            tex_ls_parser::semantic::roles::FileRoleKind::Source(_)
                | tex_ls_parser::semantic::roles::FileRoleKind::Package
                | tex_ls_parser::semantic::roles::FileRoleKind::Class
                | tex_ls_parser::semantic::roles::FileRoleKind::NamedFile {
                    suffix: ".sty" | ".bbx" | ".cbx" | ".lbx" | ".code.tex",
                    ..
                }
        );
        out.push(FileReference {
            range,
            candidates,
            loads_commands,
        });
    }
}

/// Literal file arguments shared by links, acquisition, and highlighting.
pub fn file_argument_spans(
    command: &SyntaxNode,
    role: tex_ls_parser::semantic::roles::FileRole,
) -> Vec<(String, TextRange)> {
    literal_file_arguments(command, role)
        .into_iter()
        .filter(|(name, _)| crate::external::is_literal_file_name(name, role.list))
        .flat_map(|(name, ranges)| ranges.into_iter().map(move |range| (name.clone(), range)))
        .collect()
}

/// Names before presentation splits comment continuations; `None` keeps an
/// unproved list item visible to dependency graphs.
pub fn file_argument_names(
    command: &SyntaxNode,
    role: tex_ls_parser::semantic::roles::FileRole,
) -> Vec<Option<String>> {
    literal_file_arguments(command, role)
        .into_iter()
        .map(|(name, _)| crate::external::is_literal_file_name(&name, role.list).then_some(name))
        .collect()
}

fn literal_file_arguments(
    command: &SyntaxNode,
    role: tex_ls_parser::semantic::roles::FileRole,
) -> Vec<(String, Vec<TextRange>)> {
    let Some((range, raw)) = nth_group_inner(command, role.argument) else {
        return Vec::new();
    };
    if !raw.contains('%') {
        return file_name_spans(&raw, range, role.list)
            .into_iter()
            .map(|(name, range)| (name.to_owned(), vec![range]))
            .collect();
    }
    let mut text = String::new();
    let mut positions = Vec::new();
    let mut after_comment = false;
    let mut skip_indent = false;
    for token in command
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
    {
        if !range.contains_range(token.text_range()) {
            continue;
        }
        match token.kind() {
            SyntaxKind::COMMENT => {
                after_comment = true;
                continue;
            }
            SyntaxKind::NEWLINE if after_comment => {
                after_comment = false;
                skip_indent = true;
                continue;
            }
            SyntaxKind::WHITESPACE if skip_indent => continue,
            _ => skip_indent = false,
        }
        text.push_str(token.text());
        positions.extend(
            (0..token.text().len()).map(|i| token.text_range().start() + TextSize::from(i as u32)),
        );
    }
    // The lexical view preserves ordinary file paths exactly; comments remove
    // their terminating newline and indentation, as TeX does before reading names.
    let logical_range = TextRange::at(TextSize::new(0), TextSize::from(text.len() as u32));
    let names = file_name_spans(&text, logical_range, role.list);
    names
        .into_iter()
        .map(|(name, range)| {
            let mut ranges: Vec<TextRange> = Vec::new();
            for &position in &positions[usize::from(range.start())..usize::from(range.end())] {
                match ranges.last_mut() {
                    Some(previous) if previous.end() == position => {
                        *previous = TextRange::new(previous.start(), position + TextSize::new(1));
                    }
                    _ => ranges.push(TextRange::at(position, TextSize::new(1))),
                }
            }
            (name.to_owned(), ranges)
        })
        .collect()
}

fn file_name_spans(raw: &str, range: TextRange, list: bool) -> Vec<(&str, TextRange)> {
    if list {
        comma_spans(raw, range)
    } else {
        let name = raw.trim();
        let start = range.start() + TextSize::from((raw.len() - raw.trim_start().len()) as u32);
        vec![(
            name,
            TextRange::at(start, TextSize::from(name.len() as u32)),
        )]
    }
}

/// Split a group's inner text into nonempty comma-separated names and source ranges.
/// Shared with protocol package-name hover, which selects the cursor's segment.
pub fn comma_spans(inner: &str, inner_range: TextRange) -> Vec<(&str, TextRange)> {
    crate::syntax::comma_separated_spans(inner, inner_range.start()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_operand_mentions_have_no_file_roles_but_following_commands_do() {
        for prefix in [
            r"\let\copy\input{fake}",
            r"\let\copy=\input{fake}",
            r"\meaning\input{fake}",
            r"\show\input{fake}",
            r"\noexpand\input{fake}",
            r"\NewCommandCopy{\copy}{\input}{fake}",
            r"\RenewCommandCopy{\copy}{\input}{fake}",
            r"\DeclareCommandCopy{\copy}{\input}{fake}",
            r"\ifx\relax\input{fake}\fi",
            r"\ifx a\input{fake}\fi",
            r"\detokenize{\meaning}",
        ] {
            let source = format!("{prefix}\\input{{real}}");
            let root = crate::parser::parse(&source).syntax();
            let references = file_references(&root, None, None, &[]);
            assert_eq!(
                references
                    .iter()
                    .map(|reference| &source[reference.range])
                    .collect::<Vec<_>>(),
                ["real"],
                "{source}"
            );
            let edges = crate::project::include::collect_include_edges(&root, None);
            assert_eq!(edges.len(), 1, "{source}");
        }
        for source in [
            r"\meaning\input{\input{real}}",
            r"\let\copy\input{\input{real}}",
            r"\ifx ab\input{real}\fi",
            r"\meaning\input{fake}\renewcommand{\meaning}[1]{#1}\meaning\input{real}",
        ] {
            let root = crate::parser::parse(source).syntax();
            let references = file_references(&root, None, None, &[]);
            assert_eq!(
                references
                    .iter()
                    .map(|reference| &source[reference.range])
                    .collect::<Vec<_>>(),
                ["real"],
                "{source}"
            );
        }
    }

    #[test]
    fn indexed_graphics_paths_preserve_order_resets_and_literal_ownership() {
        let source = concat!(
            "\\includegraphics{first.png}\n",
            "\\graphicspath{{old/}{old/}}\\includegraphics{second.png}\n",
            "\\graphicspath{}\\includegraphics{reset.png}\n",
            "\\detokenize{\\graphicspath{{printed/}}}\\includegraphics{literal.png}\n",
            "\\graphicspath{{live/}}\\renewcommand{\\graphicspath}[1]{#1}\n",
            "\\graphicspath{{replacement/}}\\includegraphics{final.png}\n",
        );
        let root = crate::parser::parse(source).syntax();
        let inherited = [PathBuf::from("/inherited")];
        let references = file_references(&root, Some(Path::new("/project")), None, &inherited);
        let expected = [
            vec!["/project/first.png", "/inherited/first.png"],
            vec!["/project/second.png", "/project/old/second.png"],
            vec!["/project/reset.png"],
            vec!["/project/literal.png"],
            vec!["/project/final.png", "/project/live/final.png"],
        ];
        assert_eq!(references.len(), expected.len());
        for (reference, expected) in references.iter().zip(expected) {
            assert_eq!(
                reference.candidates.local,
                expected.into_iter().map(PathBuf::from).collect::<Vec<_>>()
            );
        }
        for command in root
            .descendants()
            .filter(|node| command_name(node).as_deref() == Some("includegraphics"))
        {
            let reference = references
                .iter()
                .find(|reference| command.text_range().contains_range(reference.range))
                .unwrap();
            let dirs = super::super::resolution::completion_directories(
                &root,
                usize::from(reference.range.start()),
                Some(Path::new("/project")),
                None,
                &inherited,
            );
            assert_eq!(
                dirs.iter()
                    .map(|dir| dir.join(&source[reference.range]))
                    .collect::<Vec<_>>(),
                reference.candidates.local
            );
        }
    }

    #[test]
    fn string_prefixed_commands_share_file_ownership_and_allow_local_overrides() {
        for (source, expected) in [
            (
                r"\string\input{printed}\input{real}\string\usepackage{fake}\usepackage{valid}",
                vec!["real", "valid"],
            ),
            (
                r"\renewcommand{\string}[1]{#1}\string\usepackage{real}",
                vec!["real"],
            ),
        ] {
            let root = crate::parser::parse(source).syntax();
            let references = file_references(&root, None, None, &[]);
            assert_eq!(
                references
                    .iter()
                    .map(|reference| &source[reference.range])
                    .collect::<Vec<_>>(),
                expected
            );
            let packages = crate::project::collect_package_edge_keys(&root, None);
            assert_eq!(packages.len(), 1);
            for needle in ["printed", "fake"] {
                if let Some(at) = source.find(needle) {
                    assert_eq!(
                        crate::completion::classify_context(&root, at + 1),
                        crate::completion::CompletionContext::None
                    );
                }
            }
        }
    }

    #[test]
    fn local_overrides_share_link_edge_and_completion_ownership() {
        let source = concat!(
            "\\input{before} \\renewcommand{\\input}[1]{#1} \\input{after}\n",
            "\\usepackage[style=numeric]{biblatex} \\renewcommand{\\usepackage}[1]{#1} \\usepackage[style=other]{printed}\n",
            "\\bibliography{refs} \\renewcommand{\\bibliography}[1]{#1} \\bibliography{prose}\n",
            "\\includegraphics{real.png} \\renewcommand{\\includegraphics}[1]{#1} \\includegraphics{printed.png}\n",
        );
        let root = crate::parser::parse(source).syntax();
        let references = file_references(
            &root,
            Some(Path::new("/project")),
            Some(Path::new("/project")),
            &[],
        );
        let names: Vec<_> = references
            .iter()
            .map(|reference| &source[reference.range])
            .collect();
        assert_eq!(
            names,
            [
                "before", "numeric", "numeric", "biblatex", "refs", "real.png"
            ]
        );
        let include_edges =
            crate::project::collect_include_edges(&root, Some(Path::new("/project")));
        assert_eq!(include_edges.len(), 1);
        assert_eq!(
            include_edges[0].literal.as_deref(),
            Some(Path::new("before.tex"))
        );
        let packages =
            crate::project::collect_package_edge_keys(&root, Some(Path::new("/project")));
        assert_eq!(packages.len(), 1);
        assert_eq!(
            packages[0].target,
            crate::project::PackageTarget::Path(PathBuf::from("/project/biblatex.sty"))
        );
        let bibliography =
            crate::project::collect_bib_resource_targets(&root, Some(Path::new("/project")));
        assert_eq!(
            bibliography,
            [crate::project::BibTarget::Path(PathBuf::from(
                "/project/refs.bib"
            ))]
        );
        for needle in ["after", "printed}", "prose", "printed.png", "other"] {
            let at = source.find(needle).unwrap() + needle.len() - 1;
            assert_eq!(
                crate::completion::classify_context(&root, at),
                crate::completion::CompletionContext::None,
                "{needle}"
            );
        }
    }

    #[test]
    fn printed_package_loads_cannot_create_range_free_graph_edges() {
        let root =
            crate::parser::parse(r"\detokenize{\usepackage{fake}}\usepackage{real}").syntax();
        let base = Some(Path::new("/project"));
        let edges = crate::project::collect_package_edges(&root, base);
        let keys = crate::project::collect_package_edge_keys(&root, base);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            keys,
            edges
                .iter()
                .map(crate::project::PackageEdge::key)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            keys[0].target,
            crate::project::PackageTarget::Path(PathBuf::from("/project/real.sty"))
        );
    }

    #[test]
    fn commented_package_names_keep_exact_literal_ranges() {
        for (source, expected) in [
            (
                "\\usepackage{% 😀,ignored\r\n mhchem, xcolor% trailing\r\n}",
                vec![("mhchem", "mhchem"), ("xcolor", "xcolor")],
            ),
            (
                "\\usepackage{mh% continued\n  chem}",
                vec![("mhchem", "mh"), ("mhchem", "chem")],
            ),
            ("\\usepackage{mh% comment\n\nchem}", vec![]),
        ] {
            let root = crate::parser::parse(source).syntax();
            let command = root
                .descendants()
                .find(|node| command_name(node).as_deref() == Some("usepackage"))
                .unwrap();
            let role = crate::semantic::roles::file_role("usepackage").unwrap();
            let arguments = file_argument_spans(&command, role);
            let actual: Vec<_> = arguments
                .iter()
                .map(|(name, range)| (name.as_str(), &source[*range]))
                .collect();
            assert_eq!(actual, expected, "{source}");
            let references = file_references(
                &root,
                Some(Path::new("/project")),
                Some(Path::new("/project")),
                &[],
            );
            assert_eq!(references.len(), expected.len(), "{source}");
            for (reference, (name, _)) in references.iter().zip(expected) {
                assert!(
                    reference
                        .candidates
                        .local
                        .contains(&Path::new("/project").join(format!("{name}.sty")))
                );
            }
            let edges =
                crate::project::collect_package_edge_keys(&root, Some(Path::new("/project")));
            if source.contains("continued") {
                assert_eq!(edges.len(), 1);
                assert_eq!(
                    edges[0].target,
                    crate::project::PackageTarget::Path(PathBuf::from("/project/mhchem.sty"))
                );
            }
        }
    }
}
