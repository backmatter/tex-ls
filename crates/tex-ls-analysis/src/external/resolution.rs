//! Ordered literal-file candidates shared by analysis and host acquisition.
use crate::project::package::dtx_source_of;
use crate::source::normalize_path;
use std::path::{Path, PathBuf};

/// Literal spellings we can insert without TeX expansion, quoting or escaping.
/// Spaces inside a filename are allowed when the caller supplies a braced argument.
pub fn is_literal_file_name(name: &str, list: bool) -> bool {
    !name.is_empty()
        && name.trim() == name
        && !name
            .chars()
            .any(|ch| ch.is_control() || "\\{}%#$&~^\"".contains(ch) || (list && ch == ','))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCandidates {
    pub local: Vec<PathBuf>,
    pub installed: Option<(String, Vec<String>)>,
}

impl FileCandidates {
    pub fn new(
        raw: &str,
        extensions: &[&str],
        literate_fallback: bool,
        base: Option<&Path>,
    ) -> Self {
        let raw = Path::new(raw);
        let mut candidates: Vec<_> = if raw.extension().is_some() || extensions.is_empty() {
            vec![raw.to_path_buf()]
        } else {
            extensions
                .iter()
                .map(|ext| raw.with_extension(ext))
                .collect()
        };
        if literate_fallback {
            let literate: Vec<_> = candidates
                .iter()
                .filter_map(|path| dtx_source_of(path))
                .collect();
            candidates.extend(literate);
        }
        let local = candidates
            .into_iter()
            .map(|candidate| {
                normalize_path(&match base {
                    Some(base) if candidate.is_relative() => base.join(candidate),
                    _ => candidate,
                })
            })
            .collect();
        let installed = if raw
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            None
        } else {
            raw.file_stem().and_then(|stem| stem.to_str()).map(|stem| {
                let mut extensions: Vec<String> = match raw.extension().and_then(|ext| ext.to_str())
                {
                    Some(ext) => vec![ext.into()],
                    None => extensions.iter().map(|ext| (*ext).into()).collect(),
                };
                if literate_fallback {
                    extensions.push("dtx".into());
                }
                (stem.into(), extensions)
            })
        };
        Self { local, installed }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePathContext {
    pub root_directory: PathBuf,
    pub bases: Vec<PathBuf>,
    pub graphics: Vec<PathBuf>,
}

/// Literal graphics directories in the root preamble before source inclusion.
pub fn inherited_graphics(root: &tex_ls_parser::syntax::SyntaxNode, base: &Path) -> Vec<PathBuf> {
    use tex_ls_parser::ast::{AstNode, Group, command_name, nth_group};
    use tex_ls_parser::semantic::roles::{FileRoleKind, file_role};
    let mut result = Vec::new();
    for command in root
        .descendants()
        .filter(|node| node.kind() == tex_ls_parser::syntax::SyntaxKind::COMMAND)
    {
        let Some(name) = command_name(&command) else {
            continue;
        };
        if file_role(&name).is_some_and(|role| matches!(role.kind, FileRoleKind::Source(_))) {
            break;
        }
        if name != "graphicspath" {
            continue;
        }
        result.clear();
        if let Some(group) = nth_group(&command, 0) {
            for entry in group.children().filter_map(Group::cast) {
                if let Some((_, path)) = entry.inner() {
                    result.push(normalize_path(&base.join(path.trim())));
                }
            }
        }
    }
    result
}

/// Literal search directories shared by file links and completion acquisition.
/// A dynamic directory has no proved search path.
pub fn command_directories(
    root: &tex_ls_parser::syntax::SyntaxNode,
    command: &tex_ls_parser::syntax::SyntaxNode,
    base: Option<&Path>,
    root_dir: Option<&Path>,
    inherited_graphics: &[PathBuf],
) -> Vec<PathBuf> {
    use tex_ls_parser::ast::{AstNode, Group, command_name, nth_group, nth_group_text};
    use tex_ls_parser::semantic::roles::{FileRoleKind, file_role};
    use tex_ls_parser::syntax::SyntaxKind;
    let Some(role) = command_name(command).and_then(|name| file_role(&name)) else {
        return Vec::new();
    };
    let base = if matches!(
        role.kind,
        FileRoleKind::Source(
            tex_ls_parser::semantic::roles::SourceRole::Include
                | tex_ls_parser::semantic::roles::SourceRole::Import
        )
    ) {
        root_dir.or(base)
    } else {
        base
    }
    .unwrap_or(Path::new(""));
    if let Some(index) = role.directory {
        return nth_group_text(command, index)
            .map(|dir| vec![normalize_path(&base.join(dir.trim()))])
            .unwrap_or_default();
    }
    let mut dirs = vec![base.to_path_buf()];
    if matches!(
        role.kind,
        FileRoleKind::Graphics | FileRoleKind::Svg | FileRoleKind::Inkscape
    ) {
        // The latest literal declaration preceding the use supplies search paths.
        let declaration = root
            .descendants()
            .filter(|node| {
                node.kind() == SyntaxKind::COMMAND
                    && node.text_range().start() < command.text_range().start()
                    && command_name(node).as_deref() == Some("graphicspath")
            })
            .last();
        if declaration.is_none() {
            for dir in inherited_graphics {
                if !dirs.contains(dir) {
                    dirs.push(dir.clone());
                }
            }
        }
        if let Some(group) = declaration.and_then(|node| nth_group(&node, 0)) {
            for entry in group.children().filter_map(Group::cast) {
                if let Some((_, path)) = entry.inner() {
                    let dir = normalize_path(&base.join(path.trim()));
                    if !dirs.contains(&dir) {
                        dirs.push(dir);
                    }
                }
            }
        }
    }
    dirs
}

pub fn completion_directories(
    root: &tex_ls_parser::syntax::SyntaxNode,
    offset: usize,
    base: Option<&Path>,
    root_dir: Option<&Path>,
    inherited_graphics: &[PathBuf],
) -> Vec<PathBuf> {
    use tex_ls_parser::syntax::SyntaxKind;
    let at = rowan::TextSize::new(offset as u32);
    let command = root
        .descendants()
        .filter(|node| {
            node.kind() == SyntaxKind::COMMAND && node.text_range().contains_inclusive(at)
        })
        .min_by_key(|node| node.text_range().len());
    command
        .map(|command| command_directories(root, &command, base, root_dir, inherited_graphics))
        .unwrap_or_else(|| base.map(Path::to_path_buf).into_iter().collect())
}
