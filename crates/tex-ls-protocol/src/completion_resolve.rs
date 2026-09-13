//! `completionItem/resolve` computation. Completion items ship lean — only
//! `label`/`kind`/`insert_text` — so the list stays cheap to build over a large
//! candidate universe (the CWL command tier, a whole bibliography). When the
//! client highlights an item it sends it back here, and we attach the expensive
//! detail lazily: a one-line `detail` shown inline and a markdown `documentation`
//! card.
//!
//! The item carries an opaque [`CompletionResolveData`] in its `data` field
//! (serialized when the item was built, echoed back verbatim by the client per
//! the LSP spec). We deserialize it and recompute against the snapshot, reusing
//! the *same* renderers `hover` uses ([`super::hover`]):
//!
//! - **Citation** → the resolved `.bib` entry's card (author/title/year), walked
//!   cross-file against the project bibliography like hover's `render_citation`.
//! - **Command / environment** → the synthesized signature prototype + facts,
//!   looked up scope-first (the document's own + package defs) then built-in then
//!   CWL.
//!
//! Items with no `data` (file paths, bib fields, labels) round-trip unchanged.
//! Like hover, resolution uses the captured snapshot. The native response
//! boundary handles cancellation.

use super::*;
use lsp_types::{Documentation, MarkupContent, MarkupKind};
use serde::{Deserialize, Serialize};
use tex_ls_analysis::bib::ast as bib_ast;
use tex_ls_parser::semantic::signature::ArgSpec;

/// The opaque payload carried in a [`CompletionItem`]'s `data` field, identifying
/// what the item is so resolve can recompute its detail. `#[serde(tag = "kind")]`
/// tags the variant so an unrelated `data` shape (a future item type) fails the
/// deserialize cleanly and resolves to the item unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum CompletionResolveData {
    /// A `\cite` key: the citing file (its bibliography namespace) and the key.
    Citation { lint_path: PathBuf, key: String },
    /// A command name plus the originating document (for scope-first lookup).
    Command { name: String, file: PathBuf },
    /// An environment name plus the originating document.
    Environment { name: String, file: PathBuf },
}

impl CompletionResolveData {
    /// Serialize into a [`CompletionItem::data`] value. `None` on the (practically
    /// impossible) serialize failure, so the caller just omits `data`.
    pub fn into_value(self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }
}

/// Originating source used by the host to select the item's project context.
pub fn source_path(item: &CompletionItem) -> Option<PathBuf> {
    match serde_json::from_value::<CompletionResolveData>(item.data.clone()?).ok()? {
        CompletionResolveData::Citation { lint_path, .. } => Some(lint_path),
        CompletionResolveData::Command { file, .. }
        | CompletionResolveData::Environment { file, .. } => Some(file),
    }
}

/// Enrich `item` with `detail`/`documentation` from its `data`, or return it
/// unchanged when there is no (recognized) payload.
pub fn resolve(snapshot: &Analysis, mut item: CompletionItem) -> CompletionItem {
    if let Some(data) = item.data.as_ref()
        && let Some(revision) = data
            .get("sourceRevision")
            .and_then(serde_json::Value::as_u64)
    {
        let current = source_path(&item)
            .and_then(|path| snapshot.lookup_file(&path))
            .and_then(|file| snapshot.source_version(file));
        if current.is_none_or(|current| current.revision != revision)
            || data.get("storageEpoch").and_then(serde_json::Value::as_u64)
                != Some(snapshot.epoch())
        {
            return item;
        }
    }
    let Some(data) = item
        .data
        .clone()
        .and_then(|v| serde_json::from_value::<CompletionResolveData>(v).ok())
    else {
        return item;
    };

    let detail_doc = match data {
        CompletionResolveData::Citation { lint_path, key } => {
            citation_detail(snapshot, &lint_path, &key)
        }
        CompletionResolveData::Command { name, file } => command_detail(snapshot, &file, &name),
        CompletionResolveData::Environment { name, file } => {
            environment_detail(snapshot, &file, &name)
        }
    };

    if let Some((detail, documentation)) = detail_doc {
        item.detail = Some(detail);
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: documentation,
        }));
    }
    item
}

// --- Citation -----------------------------------------------------------------

/// Walk the citing file's bibliography namespace for the `@entry` matching `key`
/// and render `(detail, documentation)`: a compact `author (year)` line and the
/// full hover-style card. Mirrors hover's `render_citation` walk, but keeps the
/// entry node so it can build the inline detail too.
fn citation_detail(snapshot: &Analysis, lint_path: &Path, key: &str) -> Option<(String, String)> {
    let Some((file, entry)) = snapshot.citation_definition(lint_path, key) else {
        return crate::source_cards::manual(snapshot, lint_path, key)
            .map(|card| ("Manual bibliography item".into(), card));
    };
    let node = bib_ast::entry_at_range(&snapshot.parsed_bib_tree(file), entry.range)?;
    let documentation = super::hover::render_entry(&entry.entry_type, &entry.key, &node);
    let detail = citation_inline_detail(&node).unwrap_or_else(|| format!("@{}", entry.entry_type));
    Some((detail, documentation))
}

use tex_ls_analysis::bib::render::inline as citation_inline_detail;

// --- Command / environment ----------------------------------------------------

/// `(detail, documentation)` for a command: the synthesized prototype as the
/// inline detail and the full hover card as the documentation. Scope-first lookup
/// (tracked-document scope, else built-in/CWL only).
fn command_detail(snapshot: &Analysis, file: &Path, name: &str) -> Option<(String, String)> {
    let scope = scope_for(snapshot, file);
    let (sig, provenance) = super::hover::lookup_command(&scope, name)?;
    let mut detail = format!("\\{name}");
    for arg in sig.args.iter() {
        detail.push_str(super::hover::arg_slot(arg.kind));
    }
    if matches!(provenance, super::hover::Provenance::Base)
        && let Some(glyph) = tex_ls_parser::semantic::math::command_glyph(name)
    {
        detail = format!("{glyph}  {detail}");
    }
    Some((detail, super::hover::render_command(name, sig, &provenance)))
}

/// `(detail, documentation)` for an environment, like [`command_detail`] but with a
/// `\begin{name}…` prototype.
fn environment_detail(snapshot: &Analysis, file: &Path, name: &str) -> Option<(String, String)> {
    let scope = scope_for(snapshot, file);
    let (sig, provenance) = super::hover::lookup_environment(&scope, name)?;
    let detail = format!("\\begin{{{name}}}{}", arg_slots(&sig.args));
    Some((
        detail,
        super::hover::render_environment(name, sig, &provenance),
    ))
}

/// The merged signature scope for `file` when it is a tracked document, else an
/// empty scope (lookup falls back to the built-in/CWL tiers). Cloned because
/// resolve does not hold the snapshot borrow past this point.
fn scope_for(snapshot: &Analysis, file: &Path) -> SignatureDb {
    if file_kind_for(file) == FileKind::Bib {
        return SignatureDb::default();
    }
    match snapshot.lookup_file(file) {
        Some(source) => snapshot.scope_signatures(source).clone(),
        None => SignatureDb::default(),
    }
}

/// The concatenated `{}`/`[]` slots for an argument list.
fn arg_slots(args: &[ArgSpec]) -> String {
    args.iter()
        .map(|a| super::hover::arg_slot(a.kind))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    /// Run completion at the first byte of `needle`, returning the items (each
    /// carrying its `data` payload) so a test can resolve them.
    fn complete(
        db: &IncrementalDatabase,
        path: &Path,
        src: &str,
        needle: &str,
    ) -> Vec<CompletionItem> {
        let snapshot = db.snapshot();
        let offset = src.find(needle).expect("needle present") + needle.len();
        let idx = LineIndex::new(src);
        let (line, character) = idx.position(offset);
        let uri = path_to_uri(path).expect("absolute fixture path");
        // These tests never complete a package name, so the index is never resolved;
        // a disabled config keeps that guaranteed (and hermetic).
        super::compute_completion(
            &snapshot,
            &uri,
            path,
            PositionEncoding::Utf16,
            Position { line, character },
        )
        .items
    }

    /// Resolve `item` against a fresh snapshot of `db`.
    fn resolve_item(db: &IncrementalDatabase, item: CompletionItem) -> CompletionItem {
        let snapshot = db.snapshot();
        resolve(&snapshot, item)
    }

    fn documentation(item: &CompletionItem) -> String {
        match item.documentation.as_ref().expect("documentation") {
            Documentation::MarkupContent(m) => m.value.clone(),
            other => panic!("expected markup, got {other:?}"),
        }
    }

    #[test]
    fn citation_resolves_to_card_and_detail() {
        let tex = "\\addbibresource{refs.bib}\n\\cite{knu";
        let bib = "@book{knuth1984,\n  author = {Knuth, Donald E.},\n  title = {The TeXbook},\n  year = {1984},\n}\n";
        let tex_path = Path::new(fixture_path!("/p/main.tex"));
        let bib_path = Path::new(fixture_path!("/p/refs.bib"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(tex_path, tex.to_string());
        db.upsert_file(bib_path, bib.to_string());

        let items = complete(&db, tex_path, tex, "knu");
        let item = items
            .into_iter()
            .find(|i| i.label == "knuth1984")
            .expect("knuth1984 candidate");
        // Lean before resolve.
        assert!(item.documentation.is_none(), "documentation is lazy");
        assert!(item.data.is_some(), "carries resolve data");

        let resolved = resolve_item(&db, item);
        let doc = documentation(&resolved);
        assert!(doc.contains("@book"), "type: {doc}");
        assert!(doc.contains("The TeXbook"), "title: {doc}");
        assert!(doc.contains("Knuth"), "author: {doc}");
        let detail = resolved.detail.expect("detail");
        assert!(detail.contains("Knuth"), "detail author: {detail}");
        assert!(detail.contains("1984"), "detail year: {detail}");
    }

    #[test]
    fn citation_filter_text_carries_key_title_author() {
        let tex = "\\addbibresource{refs.bib}\n\\cite{knu";
        let bib = "@book{knuth1984,\n  author = {Knuth, Donald E.},\n  title = {The TeXbook},\n  year = {1984},\n}\n";
        let mut db = IncrementalDatabase::default();
        db.upsert_file(Path::new(fixture_path!("/p/main.tex")), tex.to_string());
        db.upsert_file(Path::new(fixture_path!("/p/refs.bib")), bib.to_string());

        let items = complete(&db, Path::new(fixture_path!("/p/main.tex")), tex, "knu");
        let item = items
            .into_iter()
            .find(|i| i.label == "knuth1984")
            .expect("knuth1984 candidate");
        let filter = item.filter_text.expect("filter_text");
        assert!(filter.contains("knuth1984"), "key: {filter}");
        assert!(filter.contains("The TeXbook"), "title: {filter}");
        assert!(filter.contains("Knuth"), "author: {filter}");
        assert_eq!(item.sort_text.as_deref(), Some("000000"), "sort_text");
    }

    #[test]
    fn citation_completion_is_not_key_prefixed() {
        // The typed prefix `Te` matches only the *title*, not the key. The server must
        // still return the entry (the client filters by filterText), so title-word
        // matching works on any editor.
        let tex = "\\addbibresource{refs.bib}\n\\cite{Te";
        let bib = "@book{knuth1984,\n  author = {Knuth, Donald E.},\n  title = {The TeXbook},\n}\n";
        let mut db = IncrementalDatabase::default();
        db.upsert_file(Path::new(fixture_path!("/p/main.tex")), tex.to_string());
        db.upsert_file(Path::new(fixture_path!("/p/refs.bib")), bib.to_string());

        let items = complete(&db, Path::new(fixture_path!("/p/main.tex")), tex, "Te");
        assert!(
            items.iter().any(|i| i.label == "knuth1984"),
            "full namespace returned regardless of key prefix: {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn command_resolves_to_signature() {
        let src = "\\sec";
        let path = Path::new(fixture_path!("/p/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.to_string());

        let items = complete(&db, path, src, "\\sec");
        let item = items
            .into_iter()
            .find(|i| i.label == "section")
            .expect("section candidate");
        assert!(item.documentation.is_none(), "documentation is lazy");

        let resolved = resolve_item(&db, item);
        let doc = documentation(&resolved);
        assert!(doc.contains("\\section"), "prototype: {doc}");
        assert!(doc.contains("sectioning level"), "facts: {doc}");
        // `\section` takes an optional short-title plus the mandatory title.
        assert_eq!(resolved.detail.as_deref(), Some("\\section[]{}"), "detail");
    }

    #[test]
    fn environment_resolves_to_signature() {
        let src = "\\begin{ali";
        let path = Path::new(fixture_path!("/p/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.to_string());

        let items = complete(&db, path, src, "{ali");
        let item = items
            .into_iter()
            .find(|i| i.label == "align")
            .expect("align candidate");

        let resolved = resolve_item(&db, item);
        let doc = documentation(&resolved);
        assert!(doc.contains("\\begin{align}"), "prototype: {doc}");
        assert!(doc.contains("math"), "facts: {doc}");
    }

    #[test]
    fn item_without_data_round_trips_unchanged() {
        let mut db = IncrementalDatabase::default();
        db.upsert_file(Path::new(fixture_path!("/p/main.tex")), String::new());
        let item = CompletionItem {
            label: "bare".to_owned(),
            ..Default::default()
        };
        let resolved = resolve_item(&db, item.clone());
        assert_eq!(resolved, item);
    }
}
