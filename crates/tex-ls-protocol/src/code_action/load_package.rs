//! Explicit package loading for a small, statically proved standard preamble.
use super::byte_ranges_overlap;
use crate::{
    convert::{byte_range_to_lsp, path_to_uri},
    diagnostics,
};
use lsp_types::{CodeAction, CodeActionKind, CodeActionResponse, Range, TextEdit, WorkspaceEdit};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
use tex_ls_analysis::{incremental::Analysis, text::PositionEncoding};
use tex_ls_parser::{
    ast::{AstNode, Environment, command_name, nth_group_text},
    semantic::signature::cwl,
    syntax::{SyntaxElement, SyntaxKind, SyntaxNode},
};

const PLAIN_PACKAGES: &[&str] = &[
    "amsmath", "amssymb", "amsfonts", "graphicx", "booktabs", "array",
];

struct Preamble {
    insertion: usize,
    sources: Vec<String>,
}

/// Inputs needed for an explicit package quick fix. Hosts acquire these on
/// their IO queue, then retry against a fresh snapshot. Ordinary typing does
/// not request unloaded package sources.
pub fn input_needs(
    snapshot: &Analysis,
    path: &Path,
    request: Range,
    enc: PositionEncoding,
) -> Vec<tex_ls_analysis::external::InputNeed> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let Some((root_path, root_file, preamble)) = context(snapshot, path) else {
        return Vec::new();
    };
    let providers = candidate_providers(snapshot, file, request, enc);
    if providers.is_empty() || !needs_source_proof(snapshot, root_file) {
        return Vec::new();
    }
    let mut needs = Vec::new();
    for package in providers {
        if !already_loaded(snapshot, root_file, package) {
            verify_sources(snapshot, &root_path, &preamble, package, &mut needs);
        }
    }
    let mut unique = Vec::new();
    for need in needs {
        if !unique.contains(&need) {
            unique.push(need);
        }
    }
    unique
}

/// Observations that a host must retain for a still-live standard preamble.
/// This describes candidate locations, without acquiring any new sources.
pub fn observation_paths(snapshot: &Analysis, path: &Path) -> Vec<std::path::PathBuf> {
    use tex_ls_analysis::external::FileCandidates;
    use tex_ls_analysis::project::installed_source::profile;
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let Some(preamble) = preamble(&snapshot.parsed_tree(file)) else {
        return Vec::new();
    };
    let mut pending = preamble.sources;
    pending.extend(PLAIN_PACKAGES.iter().map(|name| format!("{name}.sty")));
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if seen.len() > 32 {
            break;
        }
        let candidates = FileCandidates::new(
            &name,
            &[],
            name.ends_with(".sty") || name.ends_with(".cls"),
            path.parent(),
        );
        result.extend(candidates.local);
        let Some((stem, extensions)) = candidates.installed else {
            continue;
        };
        let extensions: Vec<_> = extensions.iter().map(String::as_str).collect();
        if let Some(actual) = snapshot.texmf().resolve(&stem, &extensions) {
            result.push(actual.to_owned());
            if let Some(file) = snapshot.lookup_file(actual)
                && let Some(identity) = snapshot.installed_source_identity(file)
            {
                let profile = profile(identity);
                pending.extend(
                    profile
                        .required
                        .iter()
                        .chain(profile.optional)
                        .map(|name| (*name).to_owned()),
                );
            }
        }
    }
    result
}

fn context(
    snapshot: &Analysis,
    path: &Path,
) -> Option<(
    std::path::PathBuf,
    tex_ls_analysis::incremental::SourceId,
    Preamble,
)> {
    let [root_path] = snapshot.document_roots(path) else {
        return None;
    };
    if !snapshot.document_scope_is_closed(path) {
        return None;
    }
    let root_file = snapshot.lookup_file(root_path)?;
    let preamble = preamble(&snapshot.parsed_tree(root_file))?;
    Some((root_path.to_owned(), root_file, preamble))
}

fn needs_source_proof(snapshot: &Analysis, root: tex_ls_analysis::incremental::SourceId) -> bool {
    use tex_ls_analysis::external::Observation;
    !snapshot.texmf().by_name.is_empty()
        || matches!(snapshot.installed_metadata(), Observation::Error(_))
        || (matches!(snapshot.installed_metadata(), Observation::Unknown)
            && snapshot.external_generations().installed != 0)
        || snapshot
            .file_references(root)
            .iter()
            .filter(|r| r.loads_commands)
            .any(|r| has_source_candidate(snapshot, &r.candidates))
}

// Acquisition can inspect candidate control words without running every lint
// rule on the writer. Only current unknown-command findings authorize an action.
fn candidate_providers(
    snapshot: &Analysis,
    file: tex_ls_analysis::incremental::SourceId,
    request: Range,
    enc: PositionEncoding,
) -> Vec<&'static str> {
    let index = snapshot.file_line_index(file, enc);
    let start = index.offset_at(request.start.line, request.start.character);
    let end = index.offset_at(request.end.line, request.end.character);
    if end < start {
        return Vec::new();
    }
    let root = snapshot.parsed_tree(file);
    let element = root.covering_element(rowan::TextRange::new(
        (start as u32).into(),
        (end as u32).into(),
    ));
    let tokens: Box<dyn Iterator<Item = SyntaxElement>> = if start == end {
        let tokens: Vec<_> = tex_ls_parser::syntax::token_at_offset(&root, (start as u32).into())
            .map(rowan::NodeOrToken::Token)
            .collect();
        Box::new(tokens.into_iter())
    } else {
        match element {
            rowan::NodeOrToken::Node(node) => Box::new(node.descendants_with_tokens()),
            rowan::NodeOrToken::Token(token) => {
                Box::new(std::iter::once(rowan::NodeOrToken::Token(token)))
            }
        }
    };
    let mut result = Vec::new();
    for element in tokens {
        snapshot.check_cancelled();
        let rowan::NodeOrToken::Token(token) = element else {
            continue;
        };
        if token.kind() != SyntaxKind::CONTROL_WORD
            || !byte_ranges_overlap(
                usize::from(token.text_range().start()),
                usize::from(token.text_range().end()),
                start,
                end,
            )
        {
            continue;
        }
        let Some(name) = token.text().strip_prefix('\\') else {
            continue;
        };
        if let [package] = cwl().command_packages(name)
            && PLAIN_PACKAGES.contains(package)
            && !result.contains(package)
        {
            result.push(*package);
        }
    }
    result
}

// Prove the selected installed bytes and the fixed dependency profile. An
// earlier unknown/error candidate, a local file, an alias or an editor overlay
// cannot authorize the edit, even if a familiar basename/header matches.
fn verify_sources(
    snapshot: &Analysis,
    root: &Path,
    preamble: &Preamble,
    provider: &str,
    needs: &mut Vec<tex_ls_analysis::external::InputNeed>,
) -> bool {
    let mut planned = Vec::new();
    let Some(proved) = source_proof(snapshot, root, preamble, provider, &mut planned) else {
        return false;
    };
    needs.extend(planned);
    proved
}

fn source_proof(
    snapshot: &Analysis,
    root: &Path,
    preamble: &Preamble,
    provider: &str,
    needs: &mut Vec<tex_ls_analysis::external::InputNeed>,
) -> Option<bool> {
    use tex_ls_analysis::external::{FileCandidates, InputNeed, Observation};
    use tex_ls_analysis::project::installed_source::profile;
    let mut pending = vec![(format!("{provider}.sty"), false)];
    pending.extend(
        preamble
            .sources
            .iter()
            .rev()
            .map(|name| (name.clone(), false)),
    );
    // No source bytes can establish a profile for an unsupported loader name.
    // Refuse before acquiring any speculative sources for this proof.
    if pending
        .iter()
        .any(|(name, _)| !tex_ls_analysis::project::installed_source::has_profile_for_name(name))
    {
        return None;
    }
    let mut seen = HashSet::new();
    let mut proved = true;
    while let Some((name, optional)) = pending.pop() {
        snapshot.check_cancelled();
        if !seen.insert((name.clone(), optional)) {
            continue;
        }
        if seen.len() > 32 {
            return None;
        }
        let candidates = FileCandidates::new(
            &name,
            &[],
            name.ends_with(".sty") || name.ends_with(".cls"),
            root.parent(),
        );
        let resolution = snapshot.resolve_file(&candidates);
        needs.extend(resolution.needs);
        let actual = match resolution.target {
            Observation::Present(actual) => actual,
            Observation::Absent if optional => continue,
            Observation::Unknown => {
                proved = false;
                continue;
            }
            Observation::Absent | Observation::Error(_) => return None,
        };
        if candidates.local.contains(&actual) || snapshot.file_alias(&actual).is_some() {
            return None;
        }
        // Navigation can use literate source when a generated file is missing.
        // TeX's package/class loader requires the exact requested filename.
        if actual.file_name() != Some(std::ffi::OsStr::new(&name))
            || snapshot.texmf().by_name.get(&name) != Some(&actual)
        {
            return None;
        }
        if matches!(
            snapshot.location_observation(&actual).kind,
            Observation::Absent | Observation::Error(_)
        ) {
            return None;
        }
        let Some(file) = snapshot.lookup_file(&actual) else {
            if !matches!(
                snapshot.location_kind(&actual),
                Observation::Absent | Observation::Error(_)
            ) {
                needs.push(InputNeed::Source(actual));
            }
            proved = false;
            continue;
        };
        if snapshot.source_has_overlay(file) {
            return None;
        }
        let identity = snapshot.installed_source_identity(file)?;
        let profile = profile(identity);
        if profile.name != name {
            return None;
        }
        pending.extend(
            profile
                .required
                .iter()
                .map(|name| ((*name).to_owned(), false)),
        );
        pending.extend(
            profile
                .optional
                .iter()
                .map(|name| ((*name).to_owned(), true)),
        );
    }
    Some(proved)
}

pub fn actions(
    snapshot: &Analysis,
    path: &Path,
    request: Range,
    findings: &[tex_ls_analysis::linter::Diagnostic],
    enc: PositionEncoding,
) -> Vec<CodeActionResponse> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let Some((root_path, root_file, preamble)) = context(snapshot, path) else {
        return Vec::new();
    };
    let source_proof = needs_source_proof(snapshot, root_file);
    let source = snapshot.file_text(root_file);
    let index = snapshot.file_line_index(file, enc);
    let start = index.offset_at(request.start.line, request.start.character);
    let end = index.offset_at(request.end.line, request.end.character);
    let Some(uri) = path_to_uri(&root_path) else {
        return Vec::new();
    };
    let root_index = snapshot.file_line_index(root_file, enc);
    let insertion = byte_range_to_lsp(&root_index, preamble.insertion, preamble.insertion);
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut offered = HashSet::new();
    let mut actions = Vec::new();
    for finding in findings.iter().filter(|finding| {
        finding.rule == "unknown-command"
            && byte_ranges_overlap(finding.start, finding.end, start, end)
    }) {
        snapshot.check_cancelled();
        let Some(raw) = snapshot.file_text(file).get(finding.start..finding.end) else {
            continue;
        };
        let Some(name) = raw.strip_prefix('\\') else {
            continue;
        };
        let [package] = cwl().command_packages(name) else {
            continue;
        };
        if !PLAIN_PACKAGES.contains(package) || !offered.insert(*package) {
            continue;
        }
        let candidates = tex_ls_analysis::external::FileCandidates::new(
            package,
            &["sty"],
            true,
            root_path.parent(),
        );
        if if source_proof {
            !verify_sources(snapshot, &root_path, &preamble, package, &mut Vec::new())
        } else {
            has_source_candidate(snapshot, &candidates)
        } {
            continue;
        }
        // Metadata includes can make an already loaded provider indirect. An
        // existing diagnostic is not sufficient proof that a second load helps.
        if already_loaded(snapshot, root_file, package) {
            continue;
        }
        actions.push(CodeActionResponse::CodeAction(CodeAction {
            title: format!("Load package `{package}` for `\\{name}`"),
            kind: Some(CodeActionKind::QuickFix),
            diagnostics: Some(vec![diagnostics::lint_to_lsp_with_range(
                &index,
                finding.clone(),
                true,
                path,
                byte_range_to_lsp(&index, finding.start, finding.end),
            )]),
            edit: Some(WorkspaceEdit {
                changes: Some(HashMap::from([(
                    uri.clone(),
                    vec![TextEdit {
                        range: insertion,
                        new_text: format!("{newline}\\usepackage{{{package}}}{newline}"),
                    }],
                )])),
                ..Default::default()
            }),
            is_preferred: Some(false),
            ..Default::default()
        }));
    }
    actions
}

fn has_source_candidate(
    snapshot: &Analysis,
    candidates: &tex_ls_analysis::external::FileCandidates,
) -> bool {
    use tex_ls_analysis::external::{LocationKind, Observation};
    candidates.local.iter().any(|path| {
        matches!(
            snapshot.location_kind(path),
            Observation::Present(LocationKind::File)
        )
    }) || candidates
        .installed
        .as_ref()
        .is_some_and(|(name, extensions)| {
            let extensions: Vec<_> = extensions.iter().map(String::as_str).collect();
            snapshot.texmf().resolve(name, &extensions).is_some()
        })
}

#[cfg(test)]
fn insertion_point(root: &SyntaxNode) -> Option<usize> {
    preamble(root).map(|preamble| preamble.insertion)
}

fn preamble(root: &SyntaxNode) -> Option<Preamble> {
    let mut class_end = None;
    let mut sources = Vec::new();
    let mut pending: Vec<_> = root.children_with_tokens().collect();
    pending.reverse();
    while let Some(element) = pending.pop() {
        match element {
            rowan::NodeOrToken::Node(node) => {
                if node.kind() == SyntaxKind::PARAGRAPH {
                    let children: Vec<_> = node.children_with_tokens().collect();
                    pending.extend(children.into_iter().rev());
                    continue;
                }
                if let Some(environment) = Environment::cast(node.clone()) {
                    if environment.begin()?.name().as_deref() != Some("document") {
                        return None;
                    }
                    return Some(Preamble {
                        insertion: class_end?,
                        sources,
                    });
                }
                if node.kind() != SyntaxKind::COMMAND {
                    return None;
                }
                // Optional package options, class options, dynamic arguments,
                // declarations and conditionals require a broader load model.
                if node
                    .children()
                    .any(|child| child.kind() == SyntaxKind::OPTIONAL)
                    || node
                        .children()
                        .filter(|child| child.kind() == SyntaxKind::GROUP)
                        .count()
                        != 1
                {
                    return None;
                }
                match command_name(&node)?.as_str() {
                    "documentclass" => {
                        if class_end.is_some()
                            || !matches!(
                                nth_group_text(&node, 0)?.trim(),
                                "article" | "report" | "book"
                            )
                        {
                            return None;
                        }
                        class_end = Some(usize::from(node.text_range().end()));
                        sources.push(format!("{}.cls", nth_group_text(&node, 0)?.trim()));
                    }
                    "usepackage" | "RequirePackage" => {
                        class_end?;
                        if !nth_group_text(&node, 0)?
                            .split(',')
                            .all(|name| PLAIN_PACKAGES.contains(&name.trim()))
                        {
                            return None;
                        }
                        sources.extend(
                            nth_group_text(&node, 0)?
                                .split(',')
                                .map(|name| format!("{}.sty", name.trim())),
                        );
                        class_end = Some(usize::from(node.text_range().end()));
                    }
                    _ => return None,
                }
            }
            rowan::NodeOrToken::Token(token)
                if token.kind() == SyntaxKind::COMMENT || token.text().trim().is_empty() => {}
            _ => return None,
        }
    }
    None
}

fn already_loaded(
    snapshot: &Analysis,
    file: tex_ls_analysis::incremental::SourceId,
    target: &str,
) -> bool {
    use tex_ls_analysis::project::PackageTarget;
    let root = snapshot.parsed_tree(file);
    let mut loaded: HashSet<String> =
        tex_ls_analysis::project::package::collect_package_edges_with_roles(
            &root,
            None,
            snapshot.file_roles(file),
        )
        .into_iter()
        .filter_map(|edge| {
            if let PackageTarget::Path(path) = edge.target {
                path.file_stem()?.to_str().map(str::to_owned)
            } else {
                None
            }
        })
        .collect();
    let mut pending: Vec<_> = loaded.iter().cloned().collect();
    while let Some(package) = pending.pop() {
        for &included in cwl().package_includes(&package) {
            if loaded.insert(included.to_owned()) {
                pending.push(included.to_owned());
            }
        }
    }
    loaded.contains(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Position;
    use tex_ls_analysis::incremental::IncrementalDatabase;
    use tex_ls_analysis::text::IntoSourceText;

    #[test]
    fn unique_plain_provider_offers_explicit_root_edit() {
        let path = Path::new(fixture_path!("/load-package/main.tex"));
        let source = "\\documentclass{article}\n\\begin{document}$\\dfrac{1}{2}$\\end{document}";
        let mut db = IncrementalDatabase::default();
        let file = db.apply_change(path, source, None);
        assert!(
            insertion_point(&db.parsed_tree(file)).is_some(),
            "preamble proof"
        );
        assert!(db.resolve_labels().is_closed(path), "namespace proof");
        assert_eq!(
            cwl().command_packages("dfrac"),
            &["amsmath"],
            "provider proof"
        );
        assert!(
            db.latex_lint_findings(file)
                .iter()
                .any(|finding| finding.rule == "unknown-command"
                    && finding.message.contains("dfrac")),
            "diagnostic proof"
        );
        let request = Range::new(Position::new(0, 0), Position::new(5, 0));
        let result = actions(
            &db,
            path,
            request,
            db.latex_lint_findings(file),
            PositionEncoding::Utf16,
        );
        assert_eq!(result.len(), 1);
        let CodeActionResponse::CodeAction(action) = &result[0] else {
            panic!("action");
        };
        assert_eq!(action.is_preferred, Some(false));
        assert!(action.title.contains("amsmath"));
        let edits =
            &action.edit.as_ref().unwrap().changes.as_ref().unwrap()[&path_to_uri(path).unwrap()];
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "\n\\usepackage{amsmath}\n");
        let index = db.file_line_index(file, PositionEncoding::Utf16);
        let at = index.offset_at(edits[0].range.start.line, edits[0].range.start.character);
        let changed = format!("{}{}{}", &source[..at], edits[0].new_text, &source[at..]);
        db.apply_change(path, changed.as_str(), None);
        assert!(
            !db.latex_lint_findings(file)
                .iter()
                .any(|finding| finding.rule == "unknown-command"
                    && finding.message.contains("dfrac"))
        );
    }

    #[test]
    fn ambiguous_providers_and_unproved_preambles_do_not_offer_loads() {
        let path = Path::new(fixture_path!("/load-package/main.tex"));
        for source in [
            r"\documentclass{article}\begin{document}\includegraphics{plot}\end{document}",
            r"\documentclass{custom}\begin{document}\dfrac{1}{2}\end{document}",
            r"\documentclass[12pt]{article}\begin{document}\dfrac{1}{2}\end{document}",
            r"\documentclass{article}\usepackage[foo]{array}\begin{document}\dfrac{1}{2}\end{document}",
            r"\documentclass{article}\PassOptionsToPackage{foo}{amsmath}\begin{document}\dfrac{1}{2}\end{document}",
            r"\documentclass{article}\iftrue\usepackage{array}\fi\begin{document}\dfrac{1}{2}\end{document}",
            r"\documentclass{article}\begin{document}\input{missing}\dfrac{1}{2}\end{document}",
        ] {
            let mut db = IncrementalDatabase::default();
            let file = db.apply_change(path, source, None);
            let result = actions(
                &db,
                path,
                Range::new(Position::new(0, 0), Position::new(5, 0)),
                db.latex_lint_findings(file),
                PositionEncoding::Utf16,
            );
            assert!(result.is_empty(), "{source}");
        }
    }

    #[test]
    fn source_class_and_provider_overrides_do_not_offer_loads() {
        let path = Path::new(fixture_path!("/load-package/main.tex"));
        for (loader, filename) in [
            (r"\documentclass{article}", "article.cls"),
            (r"\documentclass{report}", "report.cls"),
            (r"\documentclass{book}", "book.dtx"),
            (r"\documentclass{article}\usepackage{array}", "array.sty"),
            (r"\documentclass{article}", "amsmath.sty"),
        ] {
            let mut db = IncrementalDatabase::default();
            let source = format!(r"{loader}\begin{{document}}\dfrac{{1}}{{2}}\end{{document}}");
            let file = db.apply_change(path, source.as_str(), None);
            db.apply_change(
                &path.parent().unwrap().join(filename),
                r"\def\usepackage#1{\relax}",
                None,
            );
            assert!(insertion_point(&db.parsed_tree(file)).is_some());
            assert!(
                actions(
                    &db,
                    path,
                    Range::new(Position::new(0, 0), Position::new(5, 0)),
                    db.latex_lint_findings(file),
                    PositionEncoding::Utf16
                )
                .is_empty(),
                "{filename}"
            );
        }
    }

    #[test]
    fn installed_override_is_rejected_despite_unknown_local_precedence() {
        use tex_ls_analysis::external::{
            ExternalInputKind, ExternalInputs, InstalledMetadata, Observation,
        };
        use tex_ls_analysis::project::texmf::TexmfIndex;
        let path = Path::new(fixture_path!("/load-package/main.tex"));
        for (loader, filename) in [
            (r"\documentclass{article}", "article.cls"),
            (r"\documentclass{article}\usepackage{array}", "array.sty"),
            (r"\documentclass{article}", "amsmath.sty"),
        ] {
            let mut db = IncrementalDatabase::default();
            let source = format!(r"{loader}\begin{{document}}\dfrac{{1}}{{2}}\end{{document}}");
            let file = db.apply_change(path, source.as_str(), None);
            let actual = path.parent().unwrap().join("texmf").join(filename);
            db.apply_change(&actual, r"\def\usepackage#1{\relax}", None);
            let token = db
                .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
                .unwrap();
            db.apply_external_inputs(
                token,
                ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                    toolchain: "test".into(),
                    index: TexmfIndex::from_files([(filename.into(), actual)].into()),
                })),
            )
            .unwrap();
            assert!(
                actions(
                    &db,
                    path,
                    Range::new(Position::new(0, 0), Position::new(5, 0)),
                    db.latex_lint_findings(file),
                    PositionEncoding::Utf16
                )
                .is_empty(),
                "{filename}"
            );
        }
    }

    fn installed_fixture(class: &str, loads: &str) -> (IncrementalDatabase, std::path::PathBuf) {
        use tex_ls_analysis::external::*;
        use tex_ls_analysis::text::IntoSourceText;
        let path = std::path::PathBuf::from(fixture_path!("/verified-load/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, format!(r"\documentclass{{{class}}}{loads}\begin{{document}}\dfrac{{1}}{{2}}\end{{document}}"), None);
        let mut files = FileInputs::default();
        files.locations.push((
            path.parent().unwrap().to_owned(),
            LocationObservation {
                kind: Observation::Present(LocationKind::Directory),
                directory: Observation::Present(DirectoryContents {
                    complete: true,
                    ..Default::default()
                }),
            },
        ));
        let mut index = std::collections::HashMap::new();
        for group in ["2025", "common"] {
            let folder = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../tex-ls-analysis/tests/fixtures/installed-sources")
                .join(group);
            for entry in std::fs::read_dir(folder).unwrap() {
                let entry = entry.unwrap();
                let filename = entry.file_name().to_string_lossy().into_owned();
                if !matches!(
                    entry.path().extension().and_then(|e| e.to_str()),
                    Some("sty" | "cls" | "clo")
                ) {
                    continue;
                }
                let actual = path.parent().unwrap().join("texmf").join(&filename);
                index.insert(filename, actual.clone());
                files.backing.push((
                    actual,
                    Some(
                        std::fs::read_to_string(entry.path())
                            .unwrap()
                            .into_source_text(),
                    ),
                ));
            }
        }
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                toolchain: "fixture".into(),
                index: tex_ls_analysis::project::texmf::TexmfIndex::from_files(index),
            })),
        )
        .unwrap();
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Files)
            .unwrap();
        db.apply_external_inputs(token, ExternalInputs::Files(files))
            .unwrap();
        (db, path)
    }

    fn fixture_actions(db: &IncrementalDatabase, path: &Path) -> Vec<CodeActionResponse> {
        let file = db.lookup_file(path).unwrap();
        actions(
            db,
            path,
            Range::new(Position::new(0, 0), Position::new(20, 0)),
            db.latex_lint_findings(file),
            PositionEncoding::Utf16,
        )
    }

    #[test]
    fn verified_installed_classes_and_packages_preserve_load_order() {
        use tex_ls_analysis::incremental::QueryKind;
        for class in ["article", "report", "book"] {
            let loads = r"\usepackage{array}\usepackage{booktabs}";
            let (db, path) = installed_fixture(class, loads);
            assert!(
                input_needs(
                    &db,
                    &path,
                    Range::new(Position::new(0, 0), Position::new(20, 0)),
                    PositionEncoding::Utf16
                )
                .is_empty()
            );
            let result = fixture_actions(&db, &path);
            assert_eq!(result.len(), 1, "{class}");
            let CodeActionResponse::CodeAction(action) = &result[0] else {
                panic!("action");
            };
            let edits = &action.edit.as_ref().unwrap().changes.as_ref().unwrap()
                [&path_to_uri(&path).unwrap()];
            let expected = format!(r"\documentclass{{{class}}}{loads}").len();
            assert_eq!(edits[0].range.start.character as usize, expected);
            assert_eq!(edits[0].new_text, "\n\\usepackage{amsmath}\n");
            db.clear_query_log();
            assert_eq!(fixture_actions(&db, &path), result);
            assert!(
                !db.query_log()
                    .iter()
                    .any(|e| e.kind == QueryKind::InstalledSourceIdentity),
                "warm identity cache"
            );
        }
    }

    #[test]
    fn altered_sources_shadows_aliases_overlays_and_failed_reads_decline() {
        use tex_ls_analysis::external::*;
        for dependency in [
            "article.cls",
            "size10.clo",
            "amsmath.sty",
            "amstext.sty",
            "amsbsy.sty",
            "amsopn.sty",
            "amsgen.sty",
            "array.sty",
        ] {
            for mode in ["modified", "local", "alias", "overlay", "failed"] {
                let (mut db, path) = installed_fixture("article", r"\usepackage{array}");
                assert_eq!(fixture_actions(&db, &path).len(), 1, "initial proof");
                let actual = db.texmf().by_name[dependency].clone();
                match mode {
                    "modified" => {
                        let text = format!(
                            "{}\n\\def\\usepackage#1{{}}",
                            db.file_text(db.lookup_file(&actual).unwrap())
                        );
                        db.set_backing(db.project_id(), &actual, Some(text.into_source_text()))
                            .unwrap();
                    }
                    "local" => {
                        db.apply_change(
                            &path.parent().unwrap().join(dependency),
                            r"\def\usepackage#1{}",
                            None,
                        );
                    }
                    "alias" => {
                        db.set_file_alias(&actual, &path);
                    }
                    "overlay" => {
                        let text = db.file_text(db.lookup_file(&actual).unwrap()).to_owned();
                        db.open_overlay(db.project_id(), &actual, text).unwrap();
                    }
                    "failed" => {
                        let token = db
                            .begin_external_refresh(db.project_id(), ExternalInputKind::Files)
                            .unwrap();
                        db.apply_external_inputs(
                            token,
                            ExternalInputs::Files(FileInputs {
                                locations: vec![(
                                    actual,
                                    LocationObservation {
                                        kind: Observation::Error("read failed".into()),
                                        ..Default::default()
                                    },
                                )],
                                ..Default::default()
                            }),
                        )
                        .unwrap();
                    }
                    _ => unreachable!(),
                }
                assert!(
                    fixture_actions(&db, &path).is_empty(),
                    "{dependency}: {mode}"
                );
            }
        }
    }

    #[test]
    fn proof_requests_missing_inputs_and_preserves_uncertainty() {
        use tex_ls_analysis::external::*;
        let (mut db, path) = installed_fixture("article", "");
        let package = db.texmf().by_name["amsmath.sty"].clone();
        db.set_backing(db.project_id(), &package, None).unwrap();
        let range = Range::new(Position::new(0, 0), Position::new(20, 0));
        assert!(fixture_actions(&db, &path).is_empty());
        assert!(
            input_needs(&db, &path, range, PositionEncoding::Utf16)
                .contains(&InputNeed::Source(package.clone()))
        );
        // An acquisition error must stop retries and cannot use stale bytes.
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Files)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Files(FileInputs {
                locations: vec![(
                    package,
                    LocationObservation {
                        kind: Observation::Error("failed".into()),
                        ..Default::default()
                    },
                )],
                ..Default::default()
            }),
        )
        .unwrap();
        assert!(input_needs(&db, &path, range, PositionEncoding::Utf16).is_empty());
        assert!(fixture_actions(&db, &path).is_empty());
    }

    #[test]
    fn unique_vetted_providers_and_command_boundaries_are_supported() {
        for (command, package) in [
            ("dfrac", "amsmath"),
            ("mathbb", "amsfonts"),
            ("blacktriangle", "amssymb"),
            ("toprule", "booktabs"),
            ("newcolumntype", "array"),
        ] {
            let (mut db, path) = installed_fixture("article", "");
            let source = format!(
                r"\documentclass{{article}}\begin{{document}}\{command}{{x}}\end{{document}}"
            );
            db.apply_change(&path, source.as_str(), None);
            let result = fixture_actions(&db, &path);
            assert_eq!(result.len(), 1, "{package}");
            let CodeActionResponse::CodeAction(action) = &result[0] else {
                panic!("action");
            };
            assert!(action.title.contains(&format!("`{package}`")));
            let actual = db.texmf().by_name[&format!("{package}.sty")].clone();
            db.set_backing(db.project_id(), &actual, None).unwrap();
            let at = source.find(&format!("\\{command}")).unwrap();
            for offset in [at, at + 1, at + command.len() + 1] {
                let range = Range::new(
                    Position::new(0, offset as u32),
                    Position::new(0, offset as u32),
                );
                assert!(
                    input_needs(&db, &path, range, PositionEncoding::Utf16).contains(
                        &tex_ls_analysis::external::InputNeed::Source(actual.clone())
                    ),
                    "{command} cursor {offset}"
                );
            }
        }
    }

    #[test]
    fn verified_bytes_do_not_override_unknown_or_failed_local_precedence() {
        use tex_ls_analysis::external::*;
        for kind in [
            Observation::Unknown,
            Observation::Error("unavailable".into()),
        ] {
            let (mut db, path) = installed_fixture("article", "");
            let token = db
                .begin_external_refresh(db.project_id(), ExternalInputKind::Files)
                .unwrap();
            db.apply_external_inputs(
                token,
                ExternalInputs::Files(FileInputs {
                    locations: vec![(
                        path.parent().unwrap().to_owned(),
                        LocationObservation {
                            kind,
                            ..Default::default()
                        },
                    )],
                    ..Default::default()
                }),
            )
            .unwrap();
            assert!(fixture_actions(&db, &path).is_empty());
        }
        let (mut db, path) = installed_fixture("article", r"\usepackage{amsmath}");
        assert!(
            fixture_actions(&db, &path).is_empty(),
            "already loaded provider"
        );
        db.apply_change(
            &path,
            r"\documentclass[12pt]{article}\begin{document}\dfrac{1}{2}\end{document}",
            None,
        );
        assert!(
            fixture_actions(&db, &path).is_empty(),
            "unsupported options remain refused"
        );
    }

    #[test]
    fn installation_refresh_uncertainty_retires_a_previous_proof() {
        use tex_ls_analysis::external::*;
        for observation in [
            Observation::Unknown,
            Observation::Error("refresh failed".into()),
        ] {
            let (mut db, path) = installed_fixture("article", "");
            assert_eq!(fixture_actions(&db, &path).len(), 1);
            let token = db
                .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
                .unwrap();
            db.apply_external_inputs(token, ExternalInputs::Installed(observation))
                .unwrap();
            assert!(fixture_actions(&db, &path).is_empty());
        }
    }

    #[test]
    fn navigation_fallbacks_cannot_prove_an_installed_tex_load() {
        use tex_ls_analysis::external::*;
        for name in ["article.cls", "amsmath.sty", "amsgen.sty"] {
            for mislabeled_key in [false, true] {
                let (mut db, path) = installed_fixture("article", "");
                assert_eq!(fixture_actions(&db, &path).len(), 1);
                let mut index = db.texmf().clone();
                let original = index.by_name.remove(name).unwrap();
                let source = db.source_text(db.lookup_file(&original).unwrap()).clone();
                let fallback = original.with_extension("dtx");
                db.set_backing(db.project_id(), &fallback, Some(source))
                    .unwrap();
                let key = if mislabeled_key {
                    name.to_owned()
                } else {
                    fallback.file_name().unwrap().to_str().unwrap().to_owned()
                };
                index.by_name.insert(key, fallback);
                let token = db
                    .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
                    .unwrap();
                db.apply_external_inputs(
                    token,
                    ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                        toolchain: "fixture".into(),
                        index,
                    })),
                )
                .unwrap();
                assert!(
                    fixture_actions(&db, &path).is_empty(),
                    "{name}: mislabeled={mislabeled_key}"
                );
            }
        }
    }

    #[test]
    fn unprovable_preambles_do_not_acquire_an_unloaded_provider() {
        let (mut db, path) = installed_fixture("article", "");
        let provider = db.texmf().by_name["amsmath.sty"].clone();
        db.set_backing(db.project_id(), &provider, None).unwrap();
        let class = db.texmf().by_name["article.cls"].clone();
        let changed = format!(
            "{}\n% altered source",
            db.file_text(db.lookup_file(&class).unwrap())
        );
        db.set_backing(db.project_id(), &class, Some(changed.into_source_text()))
            .unwrap();
        assert!(fixture_actions(&db, &path).is_empty());
        assert!(
            input_needs(
                &db,
                &path,
                Range::new(Position::new(0, 0), Position::new(20, 0)),
                PositionEncoding::Utf16
            )
            .is_empty()
        );
    }

    #[test]
    fn unsupported_installed_profiles_decline_without_reading_sources() {
        use tex_ls_analysis::external::*;
        let (mut db, path) = installed_fixture("article", r"\usepackage{graphicx}");
        let mut index = db.texmf().clone();
        index
            .by_name
            .insert("graphicx.sty".into(), "/installed/graphicx.sty".into());
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                toolchain: "fixture".into(),
                index,
            })),
        )
        .unwrap();
        assert!(fixture_actions(&db, &path).is_empty());
        assert!(
            input_needs(
                &db,
                &path,
                Range::new(Position::new(0, 0), Position::new(20, 0)),
                PositionEncoding::Utf16
            )
            .is_empty()
        );
    }
}
