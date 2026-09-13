//! Eager source-ranged label-number hints over last-build AUX facts.
use super::*;
use serde_json::{Value, json};

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
    let Some(aux) = document_aux(snapshot, snapshot.resolve_labels(), path) else {
        return json!([]);
    };
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
                .map(|label| (&label.name, label.key_range.end())),
        );
    }
    if options.references {
        sites.extend(
            model
                .refs()
                .iter()
                .filter(|reference| {
                    !matches!(
                        reference.command,
                        tex_ls_parser::semantic::label::RefCommand::PageRef
                            | tex_ls_parser::semantic::label::RefCommand::CpageRef
                            | tex_ls_parser::semantic::label::RefCommand::NameRef
                    )
                })
                .map(|reference| (&reference.name, reference.key_range.end())),
        );
    }
    sites.sort_by_key(|(_, at)| *at);
    sites.dedup();
    json!(sites.into_iter().filter_map(|(name, at)| {
        let at = usize::from(at);
        if at < start || at > end { return None; }
        if !snapshot.resolve_labels().is_defined(path, name) { return None; }
        let number = aux.labels.get(name.as_str()).filter(|number| !number.is_empty())?;
        let max = options.max_length.clamp(1, 256);
        let label = if number.chars().count() > max { format!("{}…", number.chars().take(max-1).collect::<String>()) } else { number.clone() };
        let (line, character) = idx.position(at);
        Some(json!({"position":{"line":line,"character":character},"label":label,"paddingLeft":true,"tooltip":format!("Last build label number for {name}: {number}. Source freshness is unverified.")}))
    }).collect::<Vec<_>>())
}
