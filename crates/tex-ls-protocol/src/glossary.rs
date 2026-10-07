//! One glossary/acronym identity shared by navigation, hover and edit operations.
use super::*;

pub fn target(model: &SemanticModel, offset: usize) -> Option<(SmolStr, TextRange)> {
    let at = TextSize::new(offset as u32);
    model
        .glossary_defs()
        .iter()
        .map(|site| (&site.key, site.key_range))
        .chain(
            model
                .glossary_uses()
                .iter()
                .map(|site| (&site.key, site.key_range)),
        )
        .find(|(_, range)| range.contains_inclusive(at))
        .map(|(key, range)| (key.clone(), range))
}

pub fn occurrences(snapshot: &Analysis, path: &Path, key: &str) -> Vec<(PathBuf, TextRange, bool)> {
    namespace_of(snapshot.resolve_labels(), path)
        .into_iter()
        .flat_map(|path| {
            let mut sites = Vec::new();
            if let Some(file) = snapshot.lookup_file(path) {
                let model = snapshot.semantic_model(file);
                sites.extend(
                    model
                        .glossary_defs()
                        .iter()
                        .filter(|site| site.key == key)
                        .map(|site| (path.to_owned(), site.key_range, true)),
                );
                sites.extend(
                    model
                        .glossary_uses()
                        .iter()
                        .filter(|site| site.key == key)
                        .map(|site| (path.to_owned(), site.key_range, false)),
                );
            }
            sites
        })
        .collect()
}

pub fn locations(
    snapshot: &Analysis,
    path: &Path,
    key: &str,
    definition_only: bool,
    include_definition: bool,
    enc: PositionEncoding,
) -> Vec<Location> {
    let sites = occurrences(snapshot, path, key);
    locations_for_ranges(
        snapshot,
        sites
            .iter()
            .filter(|(_, _, definition)| {
                if definition_only {
                    *definition
                } else {
                    include_definition || !definition
                }
            })
            .map(|(path, range, _)| (path.as_path(), *range)),
        enc,
    )
    .into_iter()
    .flatten()
    .collect()
}

pub fn rename_allowed(snapshot: &Analysis, path: &Path, key: &str) -> bool {
    let sites = occurrences(snapshot, path, key);
    let mut checked = HashSet::new();
    snapshot.resolve_labels().is_closed(path)
        && snapshot.resolve_labels().has_unique_root(path)
        && sites.iter().all(|(path, _, _)| {
            !checked.insert(path)
                || name_refs::macro_roots(snapshot.resolve_labels(), snapshot.package_graph(), path)
                    .len()
                    <= 1
        })
        && sites
            .iter()
            .filter(|(_, _, definition)| *definition)
            .count()
            == 1
}

pub fn rename(
    snapshot: &Analysis,
    path: &Path,
    key: &str,
    new_name: &str,
    enc: PositionEncoding,
) -> Option<WorkspaceEdit> {
    if !is_valid_key(new_name)
        || !rename_allowed(snapshot, path, key)
        || (key != new_name && !occurrences(snapshot, path, new_name).is_empty())
    {
        return None;
    }
    let sites = occurrences(snapshot, path, key);
    let locations: Option<Vec<_>> = locations_for_ranges(
        snapshot,
        sites
            .iter()
            .map(|(path, range, _)| (path.as_path(), *range)),
        enc,
    )
    .into_iter()
    .collect();
    finalize_rename(edits_for_locations(locations?, new_name))
}
