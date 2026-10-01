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
    let mut links = Vec::new();
    for command in root
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::COMMAND)
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
            candidates.local = crate::external::command_directories(
                root,
                &command,
                base_dir,
                root_dir,
                inherited_graphics,
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
        let Some(class) = tex_ls_parser::semantic::roles::file_role(&name) else {
            continue;
        };
        collect_command(
            root,
            &command,
            class,
            base_dir,
            root_dir,
            inherited_graphics,
            &mut links,
        );
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
    root: &SyntaxNode,
    command: &SyntaxNode,
    role: tex_ls_parser::semantic::roles::FileRole,
    base_dir: Option<&Path>,
    root_dir: Option<&Path>,
    inherited_graphics: &[PathBuf],
    out: &mut Vec<FileReference>,
) {
    let kind = FileArgKind::from_role(role.kind);
    let dtx = matches!(
        kind,
        FileArgKind::Package | FileArgKind::NamedFile { suffix: ".sty", .. } | FileArgKind::Class
    );
    let directories =
        crate::external::command_directories(root, command, base_dir, root_dir, inherited_graphics);
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

/// Split a group's inner text into comma-separated names paired with their precise
/// source ranges, dropping empties. The document-link analog of the semantic
/// builder's `key_spans`: each name's range is sliced off `inner_range` by byte
/// offset (exact because trimming removes only single-byte ASCII whitespace).
///
/// Shared with protocol hover presentation's package-name hover (`pub`), which picks the
/// single segment covering the cursor.
pub fn comma_spans(inner: &str, inner_range: TextRange) -> Vec<(&str, TextRange)> {
    let base = inner_range.start();
    let mut out = Vec::new();
    let mut seg_off = 0usize;
    for segment in inner.split(',') {
        let name = segment.trim();
        if !name.is_empty() {
            let lo = segment.len() - segment.trim_start().len();
            let start = base + TextSize::from((seg_off + lo) as u32);
            let end = start + TextSize::from(name.len() as u32);
            out.push((name, TextRange::new(start, end)));
        }
        seg_off += segment.len() + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
