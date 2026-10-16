//! Source-ranged label hints over source contexts and last-build AUX facts.
use crate::{context::document_aux, convert::file_kind_for, presentation};
#[cfg(test)]
use lsp_types::Position;
use lsp_types::Range;
use serde_json::{Value, json};
use std::path::Path;
use tex_ls_analysis::{incremental::Analysis, source::FileKind, text::PositionEncoding};
use tex_ls_parser::semantic::LabelContext;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HintKind {
    Number,
    Page,
    Name,
}

pub fn compute(
    snapshot: &Analysis,
    path: &Path,
    range: Range,
    enc: PositionEncoding,
    options: &presentation::HintOptions,
) -> Value {
    let Some(file) = snapshot.lookup_file(path) else {
        return json!([]);
    };
    if file_kind_for(path) == FileKind::Bib {
        return json!([]);
    }
    let resolution = snapshot.resolve_labels();
    let aux = document_aux(snapshot, resolution, path);
    let model = snapshot.semantic_model(file);
    let idx = snapshot.file_line_index(file, enc);
    let start = idx.offset_at(range.start.line, range.start.character);
    let end = idx.offset_at(range.end.line, range.end.character);
    let mut sites = Vec::new();
    if options.definitions {
        sites.extend(
            model
                .labels()
                .iter()
                .map(|label| (&label.name, label.key_range.end(), HintKind::Number)),
        );
    }
    if options.references {
        use tex_ls_parser::semantic::label::RefCommand;
        sites.extend(model.refs().iter().filter_map(|reference| {
            let kind = match reference.command {
                RefCommand::PageRef | RefCommand::CpageRef if options.pages => HintKind::Page,
                RefCommand::NameRef if options.names => HintKind::Name,
                RefCommand::PageRef | RefCommand::CpageRef | RefCommand::NameRef => return None,
                _ => HintKind::Number,
            };
            Some((&reference.name, reference.key_range.end(), kind))
        }));
    }
    sites.sort_by_key(|(_, at, kind)| (*at, *kind));
    sites.dedup();
    let mut positions = idx.position_cursor();
    json!(sites.into_iter().filter_map(|(name, at, kind)| {
        let at = usize::from(at);
        if at < start || at > end || !resolution.is_defined(path, name) { return None; }
        let compiled = aux.as_ref().and_then(|aux| match kind {
            HintKind::Number => aux.labels.get(name.as_str()),
            HintKind::Page => aux.pages.get(name.as_str()),
            HintKind::Name => aux.names.get(name.as_str()),
        }).filter(|value| !value.is_empty());
        let max = options.max_length.clamp(1, 256);
        let source = if options.source_context && kind != HintKind::Page
            && resolution.has_unique_root(path) && resolution.definers(path, name).len() == 1
        { snapshot.label_context_ref(path, name) } else { None };
        let description = source.map(|context| source_description(context, max));
        let (text, tooltip) = if let Some(value) = compiled {
            let field = match kind { HintKind::Number => "number", HintKind::Page => "page", HintKind::Name => "name" };
            let text = if kind == HintKind::Number {
                description.map_or_else(|| value.clone(), |description| format!("{value}: {description}"))
            } else { value.clone() };
            (text, format!("Last build label {field} for {name}: {value}. Source freshness is unverified."))
        } else {
            let description = description?;
            (description.clone(), format!("Source label context for {name}: {description}."))
        };
        let label = truncate(&text, max);
        let (line, character) = positions.position(at);
        Some(json!({"position":{"line":line,"character":character},"label":label,"paddingLeft":true,"tooltip":tooltip}))
    }).collect::<Vec<_>>())
}

fn truncate(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let prefix = chars.by_ref().take(max).collect::<String>();
    if chars.next().is_some() {
        format!("{}…", prefix.chars().take(max - 1).collect::<String>())
    } else {
        prefix
    }
}

fn source_description(context: &LabelContext, max: usize) -> String {
    match context {
        LabelContext::Section { title } => format!("Section ({})", truncate(title, max)),
        LabelContext::Float { env, caption } => caption.as_ref().map_or_else(
            || env.clone(),
            |caption| format!("{env}: {}", truncate(caption, max)),
        ),
        LabelContext::Theorem { env, description } => description.as_ref().map_or_else(
            || env.clone(),
            |description| format!("{env} ({})", truncate(description, max)),
        ),
        LabelContext::Equation => "Equation".into(),
        LabelContext::Item => "Item".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::external::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    #[test]
    fn source_context_hints_work_without_aux_and_stay_opt_in() {
        let path = Path::new(fixture_path!("/hints/source.tex"));
        let source =
            "\\section{Résumé}\\label{sec:a}\n😀 \\ref{sec:a} \\pageref{sec:a} \\nameref{sec:a}";
        let mut db = IncrementalDatabase::default();
        let file = db.apply_change(path, source, None);
        let range = Range::new(Position::new(0, 0), Position::new(2, 0));
        assert_eq!(
            compute(
                &db,
                path,
                range,
                PositionEncoding::Utf16,
                &Default::default()
            ),
            json!([])
        );
        let options = presentation::HintOptions {
            source_context: true,
            definitions: false,
            names: true,
            ..Default::default()
        };
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let hints = compute(&db, path, range, enc, &options);
            assert_eq!(hints.as_array().unwrap().len(), 2);
            assert_eq!(hints[0]["label"], "Section (Résumé)");
            assert!(
                hints[0]["tooltip"]
                    .as_str()
                    .unwrap()
                    .contains("Source label context")
            );
            assert!(!hints[0]["tooltip"].as_str().unwrap().contains("Last build"));
            let offset = source.find("\\ref{sec:a}").unwrap() + "\\ref{sec:a".len();
            let (line, character) = db.file_line_index(file, enc).position(offset);
            assert_eq!(
                hints[0]["position"],
                json!({"line":line,"character":character})
            );
        }
        let limited = Range::new(Position::new(0, 0), Position::new(0, 1));
        assert_eq!(
            compute(&db, path, limited, PositionEncoding::Utf16, &options),
            json!([])
        );
    }

    #[test]
    fn page_and_name_hints_require_options_and_keep_build_provenance() {
        let path = Path::new(fixture_path!("/hints/compiled.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(path, "\\label{sec:a}\\pageref{sec:a}\\nameref{sec:a}", None);
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Compiler(vec![(
                path.with_extension("aux"),
                Observation::Present(CompilerArtifact::from_text(
                    &path.with_extension("aux"),
                    "\\newlabel{sec:a}{{7}{iv}{Résumé}{anchor}{}}",
                    "build".into(),
                )),
            )]),
        )
        .unwrap();
        let range = Range::new(Position::new(0, 0), Position::new(1, 0));
        let disabled = presentation::HintOptions {
            definitions: false,
            ..Default::default()
        };
        assert_eq!(
            compute(&db, path, range, PositionEncoding::Utf16, &disabled),
            json!([])
        );
        let enabled = presentation::HintOptions {
            pages: true,
            names: true,
            max_length: 4,
            ..disabled
        };
        let hints = compute(&db, path, range, PositionEncoding::Utf16, &enabled);
        assert_eq!(hints.as_array().unwrap().len(), 2);
        assert_eq!(hints[0]["label"], "iv");
        assert_eq!(hints[1]["label"], "Rés…");
        assert!(
            hints[0]["tooltip"]
                .as_str()
                .unwrap()
                .contains("Last build label page")
        );
        assert!(
            hints[1]["tooltip"]
                .as_str()
                .unwrap()
                .contains("Source freshness is unverified")
        );
    }
}
