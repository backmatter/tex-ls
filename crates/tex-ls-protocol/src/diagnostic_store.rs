//! One immutable snapshot report used by push, document pull and workspace pull.
use super::*;
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tex_ls_analysis::external::{Observation, compiler::Level};

/// Identity from explicit inputs, before running lint queries. Source revisions
/// cache their byte digest; unrelated compilation roots do not invalidate it.
fn identity(
    snapshot: &Analysis,
    path: &Path,
    rules: &RuleSelection,
    enc: PositionEncoding,
) -> String {
    let mut state = DefaultHasher::new();
    let labels = snapshot.resolve_labels();
    let citations = snapshot.resolve_citations();
    let mut members = std::collections::BTreeSet::from([path.to_owned()]);
    members.extend(
        labels
            .namespace_members(path)
            .into_iter()
            .map(Path::to_owned),
    );
    members.extend(
        citations
            .namespace_members(path)
            .into_iter()
            .map(Path::to_owned),
    );
    members.extend(citations.bib_definers(path).iter().cloned());
    for citer in citations.bib_citers(path) {
        members.insert(citer.to_owned());
        members.extend(
            citations
                .namespace_members(citer)
                .into_iter()
                .map(Path::to_owned),
        );
        members.extend(citations.bib_definers(citer).iter().cloned());
    }
    // Local package declarations can contribute signatures and option facts.
    members.extend(
        snapshot
            .project_members()
            .iter()
            .filter(|m| {
                m.path
                    .extension()
                    .is_some_and(|e| matches!(e.to_str(), Some("sty" | "cls" | "dtx" | "def")))
            })
            .map(|m| m.path.clone()),
    );
    format!("{rules:?}{enc:?}{:?}", labels.candidate_roots(path)).hash(&mut state);
    for member in &members {
        // Hash text, not Path's platform-width component representation, so
        // equivalent native and wasm inputs produce the same result ID.
        member.to_string_lossy().hash(&mut state);
        format!("{:?}", snapshot.declarations_for(member)).hash(&mut state);
        format!(
            "{:?}",
            snapshot.file_alias(member).map(Path::to_string_lossy)
        )
        .hash(&mut state);
        if let Some(file) = snapshot.lookup_file(member) {
            snapshot.source_text(file).content_digest().hash(&mut state);
            if file_kind_for(member).is_latex() {
                for reference in snapshot.file_references(file) {
                    format!("{:?}", snapshot.resolve_file(&reference.candidates)).hash(&mut state);
                }
            }
        }
    }
    for (artifact_path, observation) in snapshot.compiler_artifacts() {
        if (members.contains(&artifact_path.with_extension("tex"))
            || labels
                .candidate_roots(path)
                .contains(&artifact_path.with_extension("tex")))
            && let Observation::Present(artifact) = observation
        {
            artifact_path.to_string_lossy().hash(&mut state);
            artifact.content_fingerprint.hash(&mut state);
        }
    }
    state.finish().to_string()
}

/// Return unchanged reports without computing or serializing diagnostics.
pub fn document_diagnostics(
    snapshot: &Analysis,
    path: &Path,
    kind: FileKind,
    rules: &RuleSelection,
    enc: PositionEncoding,
    previous: Option<&str>,
) -> Value {
    let result_id = identity(snapshot, path, rules, enc);
    if previous == Some(result_id.as_str()) {
        json!({"kind":"unchanged", "resultId":result_id})
    } else {
        DiagnosticEntry::capture_with_identity(snapshot, path, kind, rules, enc, result_id)
            .report(None)
    }
}

pub struct DiagnosticEntry {
    pub items: Vec<Diagnostic>,
    pub result_id: String,
}
impl DiagnosticEntry {
    pub fn capture(
        snapshot: &Analysis,
        path: &Path,
        kind: FileKind,
        rules: &RuleSelection,
        enc: PositionEncoding,
    ) -> Self {
        Self::capture_with_identity(
            snapshot,
            path,
            kind,
            rules,
            enc,
            identity(snapshot, path, rules, enc),
        )
    }

    fn capture_with_identity(
        snapshot: &Analysis,
        path: &Path,
        kind: FileKind,
        rules: &RuleSelection,
        enc: PositionEncoding,
        result_id: String,
    ) -> Self {
        let mut items = match kind {
            FileKind::Bib => analyze_bib(snapshot, path, rules, enc),
            _ => analyze_tex(snapshot, path, rules, enc),
        }
        .unwrap_or_default();
        // Content identities are independent of host allocation IDs and acquisition
        // counts. Identical observations remain unchanged; edits to a dependency or
        // artifact invalidate even when the visible messages happen to be identical.
        let members = snapshot.resolve_labels().namespace_members(path);
        let roots = snapshot.resolve_labels().candidate_roots(path);

        for (artifact_path, observation) in snapshot.compiler_artifacts() {
            let Observation::Present(artifact) = observation else {
                continue;
            };
            let owner = artifact_path.with_extension("tex");
            if !members.contains(&owner.as_path()) && !roots.contains(&owner) {
                continue;
            }

            // The logical path is stable across native physical aliases and browser
            // identities. Parsed content, including removed messages/edges, is state.

            for message in &artifact.messages {
                if !rules.external.accepts("compiler", message.level) {
                    continue;
                }
                // Unattributed build messages appear on the artifact's owner with an
                // explicit label; an explicit unknown path is never reassigned.
                let target = message.path.as_deref().unwrap_or(&owner);
                if target != path {
                    continue;
                }
                let range = snapshot
                    .lookup_file(path)
                    .map(|file| {
                        let text = snapshot.file_text(file);
                        let idx = snapshot.file_line_index(file, enc);
                        let line = message.line.unwrap_or(1).saturating_sub(1);
                        let start = idx.offset_at(line, 0);
                        let end = text[start..]
                            .find(['\r', '\n'])
                            .map_or(text.len(), |len| start + len);
                        if let Some(hint) = &message.hint {
                            let mut matches = text[start..end].match_indices(hint);
                            if let Some((at, matched)) = matches.next()
                                && matches.next().is_none()
                            {
                                return byte_range_to_lsp(
                                    &idx,
                                    start + at,
                                    start + at + matched.len(),
                                );
                            }
                        }
                        byte_range_to_lsp(&idx, start, end)
                    })
                    .unwrap_or_default();
                let provenance = if message.path.is_none() {
                    "Last build; source not identified"
                } else {
                    "Last build; source freshness unverified"
                };
                items.push(Diagnostic {
                    range,
                    severity: Some(match message.level {
                        Level::Error => DiagnosticSeverity::Error,
                        Level::Warning => DiagnosticSeverity::Warning,
                        Level::Information => DiagnosticSeverity::Information,
                    }),
                    source: Some("compiler".into()),
                    message: format!("{} ({provenance})", message.message).into(),
                    ..Default::default()
                });
            }
        }

        Self { items, result_id }
    }
    pub fn report(self, previous: Option<&str>) -> Value {
        if previous == Some(self.result_id.as_str()) {
            json!({"kind":"unchanged", "resultId":self.result_id})
        } else {
            json!({"kind":"full", "resultId":self.result_id, "items":self.items})
        }
    }
}

/// Previous reports for removed documents receive an empty full report, clearing
/// their ownership. URI order is stable across project enumeration order.
pub fn workspace_diagnostics(
    snapshots: &[Analysis],
    previous: &Value,
    enc: PositionEncoding,
    rules: impl Fn(&Path) -> RuleSelection,
    version: impl Fn(&Path) -> Option<i32>,
) -> Value {
    let mut items = Vec::new();
    stream_workspace_diagnostics(snapshots, previous, enc, rules, version, |batch| {
        items.extend(batch);
        true
    });
    json!({"items":items})
}

/// Emit bounded batches during analysis; false stops before the next document.
pub fn stream_workspace_diagnostics(
    snapshots: &[Analysis],
    previous: &Value,
    enc: PositionEncoding,
    rules: impl Fn(&Path) -> RuleSelection,
    version: impl Fn(&Path) -> Option<i32>,
    mut emit: impl FnMut(Vec<Value>) -> bool,
) {
    let previous: std::collections::BTreeMap<String, String> = previous
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some((
                entry.get("uri")?.as_str()?.into(),
                entry.get("value")?.as_str()?.into(),
            ))
        })
        .collect();
    let mut sources = std::collections::BTreeMap::new();
    for snapshot in snapshots {
        for (path, _) in snapshot.tracked_files() {
            if let Some(uri) = path_to_uri(&path) {
                sources
                    .entry(uri.as_str().to_owned())
                    .or_insert((snapshot, path));
            }
        }
    }
    let mut batch = Vec::new();
    let all = sources
        .keys()
        .chain(previous.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for uri in all {
        let report = if let Some((snapshot, path)) = sources.get(&uri) {
            let mut report = document_diagnostics(
                snapshot,
                path,
                file_kind_for(path),
                &rules(path),
                enc,
                previous.get(&uri).map(String::as_str),
            );
            report["uri"] = json!(uri);
            report["version"] = json!(version(path));
            report
        } else {
            json!({"uri":uri,"version":null,"kind":"full","items":[],"resultId":"removed"})
        };
        batch.push(report);
        if batch.len() == 64 && !emit(std::mem::take(&mut batch)) {
            return;
        }
    }
    if !batch.is_empty() {
        emit(batch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::{
        external::{CompilerArtifact, ExternalInputKind, ExternalInputs},
        incremental::IncrementalDatabase,
    };

    fn assert_document_workspace_parity(snapshots: &[Analysis]) -> Value {
        let workspace = workspace_diagnostics(
            snapshots,
            &json!([]),
            PositionEncoding::Utf16,
            |_| RuleSelection::all(),
            |_| Some(7),
        );
        let reports = workspace["items"].as_array().unwrap();
        for snapshot in snapshots {
            for (path, _) in snapshot.tracked_files() {
                let mut expected = DiagnosticEntry::capture(
                    snapshot,
                    &path,
                    file_kind_for(&path),
                    &RuleSelection::all(),
                    PositionEncoding::Utf16,
                )
                .report(None);
                let uri = json!(path_to_uri(&path).unwrap());
                expected["uri"] = uri.clone();
                expected["version"] = json!(7);
                let actual = reports.iter().find(|report| report["uri"] == uri).unwrap();
                assert_eq!(*actual, expected);
            }
        }
        let previous = json!(
            reports
                .iter()
                .map(|report| json!({"uri":report["uri"], "value":report["resultId"]}))
                .collect::<Vec<_>>()
        );
        let unchanged = workspace_diagnostics(
            snapshots,
            &previous,
            PositionEncoding::Utf16,
            |_| RuleSelection::all(),
            |_| Some(7),
        );
        for report in unchanged["items"].as_array().unwrap() {
            assert_eq!(report["kind"], "unchanged");
            assert!(report.get("items").is_none());
        }
        workspace
    }

    fn report_for<'a>(workspace: &'a Value, path: &Path) -> &'a Value {
        let uri = json!(path_to_uri(path).unwrap());
        workspace["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|report| report["uri"] == uri)
            .unwrap()
    }

    fn set_aux(db: &mut IncrementalDatabase, text: &str) {
        let path = Path::new(fixture_path!("/diagnostic-parity/main.aux"));
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Compiler(vec![(
                path.to_owned(),
                Observation::Present(CompilerArtifact::from_text(path, text, "aux".into())),
            )]),
        )
        .unwrap();
    }

    #[test]
    fn workspace_identity_matches_document_pulls_after_dependency_and_artifact_changes() {
        let mut db = IncrementalDatabase::default();
        let main = Path::new(fixture_path!("/diagnostic-parity/main.tex"));
        let child = Path::new(fixture_path!("/diagnostic-parity/child.tex"));
        db.apply_change(
            main,
            "\\documentclass{article}\n\\input{child}\n\\ref{sec:child}\n",
            None,
        );
        db.apply_change(child, "\\section{Intro}\\label{sec:child}\n", None);
        db.apply_change(
            Path::new(fixture_path!("/diagnostic-parity/refs.bib")),
            "@book{key, title={Title}, author={Doe, Jane}, year={2026}}\n",
            None,
        );
        let mut other = IncrementalDatabase::default();
        other.apply_change(
            Path::new(fixture_path!("/other-project/main.tex")),
            "Other project\n",
            None,
        );
        let original = assert_document_workspace_parity(&[db.snapshot(), other.snapshot()]);

        // An edit can leave visible diagnostics unchanged but must still update
        // dependent report identities, including with a reused source hash.
        db.apply_change(child, "\\section{Intro}\\label{sec:child}\n% café\n", None);
        let edited = assert_document_workspace_parity(&[db.snapshot(), other.snapshot()]);
        let before = report_for(&original, main);
        let after = report_for(&edited, main);
        assert_eq!(before["items"], after["items"]);
        assert_ne!(before["resultId"], after["resultId"]);
        assert_eq!(
            report_for(
                &original,
                Path::new(fixture_path!("/other-project/main.tex"))
            ),
            report_for(&edited, Path::new(fixture_path!("/other-project/main.tex")))
        );

        set_aux(&mut db, "\\newlabel{sec:child}{{1}{1}}\n");
        let compiled = assert_document_workspace_parity(&[db.snapshot(), other.snapshot()]);
        set_aux(&mut db, "\\newlabel{sec:child}{{2}{1}}\n");
        let rebuilt = assert_document_workspace_parity(&[db.snapshot(), other.snapshot()]);
        for path in [main, child] {
            let before = report_for(&compiled, path);
            let after = report_for(&rebuilt, path);
            assert_eq!(before["items"], after["items"]);
            assert_ne!(before["resultId"], after["resultId"]);
        }

        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Compiler(vec![(
                Path::new(fixture_path!("/diagnostic-parity/main.aux")).to_owned(),
                Observation::Absent,
            )]),
        )
        .unwrap();
        let removed = assert_document_workspace_parity(&[db.snapshot(), other.snapshot()]);
        assert_eq!(
            removed, edited,
            "removing artifacts restores source-only reports"
        );
    }
}
