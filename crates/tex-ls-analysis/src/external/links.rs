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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReference {
    pub range: TextRange,
    pub candidates: FileCandidates,
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
    let Some((range, raw)) = nth_group_inner(command, role.argument) else {
        return;
    };
    let kind = FileArgKind::from_role(role.kind);
    let dtx = matches!(kind, FileArgKind::Package | FileArgKind::Class);
    let directories =
        crate::external::command_directories(root, command, base_dir, root_dir, inherited_graphics);
    let names = if role.list {
        comma_spans(&raw, range)
    } else {
        let name = raw.trim();
        if name.is_empty() {
            return;
        }
        let start = range.start() + TextSize::from((raw.len() - raw.trim_start().len()) as u32);
        vec![(
            name,
            TextRange::at(start, TextSize::from(name.len() as u32)),
        )]
    };
    for (name, range) in names {
        let mut candidates = FileCandidates::new(name, kind.extensions(), dtx, None);
        candidates.local = directories
            .iter()
            .flat_map(|dir| FileCandidates::new(name, kind.extensions(), dtx, Some(dir)).local)
            .collect();
        if role.directory.is_some() {
            candidates.installed = None;
        }
        out.push(FileReference { range, candidates });
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
