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
//! the *same* renderers `hover` uses ([`crate::hover`]):
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

use crate::convert::file_kind_for;
use lsp_types::{CompletionItem, Documentation, MarkupContent, MarkupKind};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tex_ls_analysis::{
    bib::{ast as bib_ast, render::inline as citation_inline_detail},
    incremental::Analysis,
    source::FileKind,
};
use tex_ls_parser::semantic::{SignatureDb, signature::ArgSpec};

/// The opaque payload carried in a [`CompletionItem`]'s `data` field, identifying
/// what the item is so resolve can recompute its detail. `#[serde(tag = "kind")]`
/// tags the variant so an unrelated `data` shape (a future item type) fails the
/// deserialize cleanly and resolves to the item unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum CompletionResolveData {
    /// A `\cite` key: the citing file (its bibliography namespace) and the key.
    Citation {
        #[serde(with = "resolve_path")]
        lint_path: PathBuf,
        key: String,
    },
    Glossary {
        #[serde(with = "resolve_path")]
        file: PathBuf,
        key: String,
    },
    /// A command name plus the originating document (for scope-first lookup).
    Command {
        name: String,
        #[serde(with = "resolve_path")]
        file: PathBuf,
    },
    /// An environment name plus the originating document.
    Environment {
        name: String,
        #[serde(with = "resolve_path")]
        file: PathBuf,
    },
}

/// Keep ordinary payload strings intact; file URIs preserve other native paths.
/// The shared URI conversions perform no filesystem access and support WASM.
mod resolve_path {
    use crate::convert::{path_to_uri, uri_to_fs_path};
    use lsp_types::Uri;
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FileUri {
        uri: Uri,
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum EncodedPath {
        Text(String),
        Native(FileUri),
    }

    pub fn serialize<S: serde::Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(text) = path.to_str() {
            serializer.serialize_str(text)
        } else {
            let uri = path_to_uri(path).ok_or_else(|| {
                serde::ser::Error::custom("Cannot encode source path as an absolute file URI")
            })?;
            FileUri { uri }.serialize(serializer)
        }
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PathBuf, D::Error> {
        match EncodedPath::deserialize(deserializer)? {
            EncodedPath::Text(text) => Ok(text.into()),
            EncodedPath::Native(value) => {
                if !value
                    .uri
                    .as_str()
                    .get(..7)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"))
                {
                    return Err(serde::de::Error::custom(
                        "Source path object requires an absolute file URI",
                    ));
                }
                uri_to_fs_path(&value.uri)
                    .filter(|path| path.is_absolute())
                    .ok_or_else(|| {
                        serde::de::Error::custom("Source path object requires an absolute file URI")
                    })
            }
        }
    }
}

impl CompletionResolveData {
    /// Serialize into a [`CompletionItem::data`] value. Unrepresentable source
    /// paths decline resolve metadata without inventing a different identity.
    pub fn into_value(self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }
}

/// Originating source used by the host to select the item's project context.
pub fn source_path(item: &CompletionItem) -> Option<PathBuf> {
    match serde_json::from_value::<CompletionResolveData>(item.data.clone()?).ok()? {
        CompletionResolveData::Citation { lint_path, .. } => Some(lint_path),
        CompletionResolveData::Glossary { file, .. }
        | CompletionResolveData::Command { file, .. }
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
        CompletionResolveData::Glossary { file, key } => {
            crate::source_cards::glossary(snapshot, &file, &key)
                .map(|card| ("Glossary/acronym entry".into(), card))
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
    let documentation = tex_ls_analysis::bib::render::entry(&entry.entry_type, &entry.key, &node);
    let detail = citation_inline_detail(&node).unwrap_or_else(|| format!("@{}", entry.entry_type));
    Some((detail, documentation))
}

// --- Command / environment ----------------------------------------------------

/// `(detail, documentation)` for a command: the synthesized prototype as the
/// inline detail and the full hover card as the documentation. Scope-first lookup
/// (tracked-document scope, else built-in/CWL only).
fn command_detail(snapshot: &Analysis, file: &Path, name: &str) -> Option<(String, String)> {
    let scope = scope_for(snapshot, file);
    if let Some(source) = snapshot.lookup_file(file)
        && snapshot.editor_symbols(source).commands.contains(name)
        && scope.command(name).is_none()
    {
        return Some((
            format!("\\{name}"),
            crate::command_docs::card(snapshot, file, name, false)?,
        ));
    }
    if crate::command_docs::entry(name, false).is_some_and(|doc| doc.primitive)
        && scope.command(name).is_none()
    {
        return Some((
            format!("\\{name}"),
            crate::command_docs::card(snapshot, file, name, false)?,
        ));
    }
    let (sig, provenance) = tex_ls_analysis::hover::lookup_command(&scope, name)?;
    let mut detail = format!("\\{name}");
    for arg in sig.args.iter() {
        detail.push_str(crate::presentation::signatures::arg_slot(arg.kind));
    }
    if matches!(provenance, tex_ls_analysis::hover::Provenance::Base)
        && let Some(glyph) = tex_ls_parser::semantic::math::command_glyph(name)
    {
        detail = format!("{glyph}  {detail}");
    }
    Some((
        detail,
        crate::command_docs::card(snapshot, file, name, false)?,
    ))
}

/// `(detail, documentation)` for an environment, like [`command_detail`] but with a
/// `\begin{name}…` prototype.
fn environment_detail(snapshot: &Analysis, file: &Path, name: &str) -> Option<(String, String)> {
    let scope = scope_for(snapshot, file);
    let documentation = crate::command_docs::card(snapshot, file, name, true)?;
    let args = if scope.environment(name).is_none()
        && snapshot
            .lookup_file(file)
            .is_some_and(|source| snapshot.editor_symbols(source).environments.contains(name))
    {
        String::new()
    } else {
        tex_ls_analysis::hover::lookup_environment(&scope, name)
            .map(|(sig, _)| arg_slots(&sig.args))
            .unwrap_or_default()
    };
    Some((format!("\\begin{{{name}}}{args}"), documentation))
}

/// The merged signature scope for `file` when it is a tracked document, else an
/// empty scope (lookup falls back to the built-in/CWL tiers). Resolution only
/// reads the snapshot, so tracked scopes can be borrowed without copying maps.
fn scope_for<'a>(snapshot: &'a Analysis, file: &Path) -> std::borrow::Cow<'a, SignatureDb> {
    if file_kind_for(file) == FileKind::Bib {
        return std::borrow::Cow::Owned(SignatureDb::default());
    }
    match snapshot.lookup_file(file) {
        Some(source) => std::borrow::Cow::Borrowed(snapshot.editor_signatures(source)),
        None => std::borrow::Cow::Owned(SignatureDb::default()),
    }
}

/// The concatenated `{}`/`[]` slots for an argument list.
fn arg_slots(args: &[ArgSpec]) -> String {
    args.iter()
        .map(|a| crate::presentation::signatures::arg_slot(a.kind))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::path_to_uri;
    use lsp_types::Position;
    use tex_ls_analysis::{
        incremental::IncrementalDatabase,
        text::{LineIndex, PositionEncoding},
    };

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
        super::super::compute_completion(
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
    fn completion_resolve_borrows_the_cached_signature_scope() {
        let mut source = String::new();
        let mut last_name = String::new();
        for index in 0..2048usize {
            last_name = format!(
                "defined{}{}{}",
                char::from(b'a' + (index / 676) as u8),
                char::from(b'a' + ((index / 26) % 26) as u8),
                char::from(b'a' + (index % 26) as u8),
            );
            source.push_str(&format!("\\newcommand{{\\{last_name}}}[1]{{#1}}\n"));
        }
        source.push_str("\\sec");
        let path = Path::new(fixture_path!("/p/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, source.clone());
        let item = complete(&db, path, &source, "\\sec")
            .into_iter()
            .find(|item| item.label == "section")
            .unwrap();
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(path).unwrap();
        let cached = snapshot.editor_signatures(file);
        assert!(cached.command(&last_name).is_some());
        let scope = scope_for(&snapshot, path);
        let scope_ref: &SignatureDb = &scope;
        assert_eq!(scope_ref, cached);
        assert!(
            std::ptr::eq(scope_ref, cached),
            "completion resolve must borrow its immutable cached scope"
        );
        let actual = resolve(&snapshot, item);
        assert_eq!(actual.detail.as_deref(), Some("\\section[]{}"));
        assert!(documentation(&actual).contains("sectioning level"));
    }

    #[test]
    fn completion_path_strings_keep_their_existing_shape() {
        for path in [
            fixture_path!("/p/main.tex"),
            "relative.tex",
            "file:///literal",
        ] {
            let value = serde_json::json!({"kind":"Command", "name":"section", "file":path});
            let data: CompletionResolveData = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(data).unwrap(), value);
        }
    }

    #[test]
    fn invalid_completion_path_objects_decline_resolution() {
        for file in [
            serde_json::json!({"uri":"file:relative.tex"}),
            serde_json::json!({"uri":"https://example.test/main.tex"}),
            serde_json::json!({"uri":"file:///p/main.tex?query"}),
            serde_json::json!({"uri":"file:///p/main.tex#fragment"}),
            serde_json::json!({"uri":"file:///p/a%00.tex"}),
            serde_json::json!({"uri":"file:///p/main.tex", "extra":true}),
        ] {
            let item = CompletionItem {
                label: "section".into(),
                data: Some(serde_json::json!({"kind":"Command", "name":"section", "file":file})),
                ..CompletionItem::default()
            };
            assert!(source_path(&item).is_none(), "{item:?}");
            let mut db = IncrementalDatabase::default();
            db.upsert_file(Path::new(fixture_path!("/p/main.tex")), "\\sec".to_owned());
            assert_eq!(resolve_item(&db, item.clone()), item);
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_source_completions_keep_resolve_data_and_documentation() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(OsString::from_vec(b"/project-\xff/main.tex".to_vec()));
        let bib_path = path.parent().unwrap().join("refs.bib");
        for (source, needle, label, expected) in [
            ("\\sec", "\\sec", "section", "\\section"),
            ("\\begin{ali", "{ali", "align", "\\begin{align}"),
            (
                "\\newcommand{\\localcmd}[1]{#1}\n \\localc",
                " \\localc",
                "localcmd",
                "\\localcmd",
            ),
            (
                "\\addbibresource{refs.bib}\n\\cite{knu",
                "knu",
                "knuth1984",
                "The TeXbook",
            ),
        ] {
            let mut db = IncrementalDatabase::default();
            db.upsert_file(&path, source.to_owned());
            db.upsert_file(
                &bib_path,
                "@book{knuth1984, author={Donald Knuth}, title={The TeXbook}, year={1984}}"
                    .to_owned(),
            );
            let item = complete(&db, &path, source, needle)
                .into_iter()
                .find(|item| item.label == label)
                .expect("candidate");
            assert!(item.documentation.is_none());
            assert!(item.data.is_some(), "{label} has no resolve data");
            assert_eq!(source_path(&item), Some(path.clone()));
            let resolved = resolve_item(&db, item);
            assert_eq!(source_path(&resolved), Some(path.clone()));
            assert!(documentation(&resolved).contains(expected), "{label}");
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
