//! Tests for the salsa incremental harness (`incremental.rs`): memoization,
//! revision-driven re-runs, the unchanged-text short-circuit, and that the
//! cached parse path preserves losslessness.

use meaning_analysis::text::IntoSourceText;
use std::path::Path;

use meaning_analysis::incremental::{IncrementalDatabase, QueryKind};
use meaning_parser::declarations::{Declarations, ResolvedDeclarations};
use meaning_parser::parser::Edit;
use meaning_parser::syntax::SyntaxKind;

/// A byte-range edit, the currency the reparse side channel stages.
fn edit(range: std::ops::Range<usize>, insert: &str) -> Edit {
    Edit {
        range,
        insert: insert.to_string(),
    }
}

/// How many times `parsed_document` actually ran, per the query log.
fn parse_count(db: &meaning_analysis::incremental::Analysis) -> usize {
    db.query_log()
        .iter()
        .filter(|entry| entry.kind == QueryKind::ParsedDocument)
        .count()
}

/// How many times `document_signatures` actually ran, per the query log.
fn signatures_count(db: &IncrementalDatabase) -> usize {
    db.query_log()
        .iter()
        .filter(|entry| entry.kind == QueryKind::DocumentSignatures)
        .count()
}

/// How many times `parsed_bib_document` actually ran, per the query log.
fn bib_parse_count(db: &IncrementalDatabase) -> usize {
    db.query_log()
        .iter()
        .filter(|entry| entry.kind == QueryKind::ParsedBibDocument)
        .count()
}

/// How many times `bib_semantic_model` actually ran, per the query log.
fn bib_model_count(db: &IncrementalDatabase) -> usize {
    db.query_log()
        .iter()
        .filter(|entry| entry.kind == QueryKind::BibSemanticModel)
        .count()
}

/// How many times `doc_associations` actually ran, per the query log.
fn doc_assoc_count(db: &IncrementalDatabase) -> usize {
    db.query_log()
        .iter()
        .filter(|entry| entry.kind == QueryKind::DocAssociations)
        .count()
}

/// An owned, sorted projection of the scanned command names.
fn scanned_commands(
    db: &IncrementalDatabase,
    file: meaning_analysis::incremental::SourceId,
) -> Vec<String> {
    let mut names: Vec<String> = db
        .document_signatures(file)
        .command_names()
        .map(str::to_string)
        .collect();
    names.sort();
    names
}

#[test]
fn parsed_document_is_memoized() {
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("\\section{Hi}\n");
    db.clear_query_log();

    // Many reads — including two distinct consumers of the cached parse — but
    // the parse itself runs exactly once.
    let _ = db.parsed_tree(file);
    let _ = db.parsed_tree(file);
    let _ = db.parse_diagnostics(file);

    assert_eq!(parse_count(&db), 1);
}

#[test]
fn query_log_records_only_after_an_observation_window_opens() {
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("\\label{a}\n");

    let _ = db.parsed_tree(file);
    assert!(db.query_log().is_empty());

    db.clear_query_log();
    let _ = db.semantic_model(file);
    assert_eq!(db.query_log().len(), 1);
    assert_eq!(db.query_log()[0].kind, QueryKind::SemanticModel);
}

#[test]
fn editing_text_reparses() {
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("a\n");
    db.clear_query_log();

    let _ = db.parsed_tree(file);
    assert_eq!(parse_count(&db), 1);

    db.set_file_text(file, "b\n");
    let _ = db.parsed_tree(file);
    assert_eq!(parse_count(&db), 2);
}

#[test]
fn upsert_unchanged_text_does_not_reparse() {
    let mut db = IncrementalDatabase::default();
    let path = std::path::Path::new("/tmp/doc.tex");

    let file = db.upsert_file(path, "x\n".to_string());
    db.clear_query_log();
    let _ = db.parsed_tree(file);
    assert_eq!(parse_count(&db), 1);

    // Re-upserting identical text must not bump the revision, so the cached
    // parse stands.
    let same = db.upsert_file(path, "x\n".to_string());
    assert!(same == file);
    let _ = db.parsed_tree(same);
    assert_eq!(parse_count(&db), 1);

    // Changing the text does re-parse.
    let changed = db.upsert_file(path, "y\n".to_string());
    assert!(changed == file);
    let _ = db.parsed_tree(changed);
    assert_eq!(parse_count(&db), 2);
}

/// The language server re-upserts the *same* `Arc<str>` the live buffer holds
/// on every keystroke that lands on an unedited file, and asks whether a read
/// job's captured buffer is still current before every cached-tree read. Both
/// settle by pointer; both must still settle correctly for a text that arrived
/// by another route (a disk re-read), which shares no allocation.
#[test]
fn a_shared_text_handle_is_recognized_without_a_content_compare() {
    use std::sync::Arc;

    let mut db = IncrementalDatabase::default();
    let path = std::path::Path::new("/tmp/shared.tex");

    let held = "x\n".into_source_text();
    let file = db.upsert_file(path, Arc::clone(&held));
    db.clear_query_log();
    let _ = db.parsed_tree(file);
    assert_eq!(parse_count(&db), 1);

    // The same allocation, and an equal one built independently.
    let _ = db.upsert_file(path, Arc::clone(&held));
    let _ = db.upsert_file(path, "x\n".to_string());
    let _ = db.parsed_tree(file);
    assert_eq!(parse_count(&db), 1);

    assert!(db.text_is_current(file, &held));
    assert!(db.text_is_current(file, "x\n"));
    assert!(!db.text_is_current(file, "y\n"));
    // A prefix shares the pointer but not the length.
    assert!(!db.text_is_current(file, &held[..1]));
}

#[test]
fn cached_tree_is_lossless() {
    let mut db = IncrementalDatabase::default();
    let input = "\\section{Hi}\n\nbody $x^2$ % c\n";
    let file = db.add_file(input);

    assert_eq!(db.parsed_tree(file).to_string(), input);
}

#[test]
fn remove_file_stops_tracking() {
    let mut db = IncrementalDatabase::default();
    let path = std::path::Path::new("/tmp/doc.tex");

    let file = db.upsert_file(path, "x\n".to_string());
    assert!(db.lookup_file(path) == Some(file));
    assert_eq!(db.snapshot().project_members().len(), 1);

    // Eviction returns the dropped handle and makes the path untracked.
    assert!(db.remove_file(path) == Some(file));
    assert!(db.lookup_file(path).is_none());
    assert!(db.snapshot().project_members().is_empty());
    assert!(db.remove_file(path).is_none());

    // Re-opening the same path mints a *fresh* input, not the evicted one.
    let reopened = db.upsert_file(path, "x\n".to_string());
    assert!(reopened != file);
    assert!(db.lookup_file(path) == Some(reopened));
}

#[test]
fn snapshot_reads_cached_parse() {
    let mut db = IncrementalDatabase::default();
    let path = std::path::Path::new("/tmp/snap.tex");
    let file = db.upsert_file(path, "\\emph{hi}\n".to_string());
    let _ = db.parsed_tree(file);

    // A read-only snapshot sees the same cached parse off the writer.
    let snap = db.snapshot();
    let snap_file = snap.lookup_file(path).expect("tracked file");
    assert!(snap_file == file);
    assert_eq!(snap.file_text(file), "\\emph{hi}\n");
    assert!(snap.parse_diagnostics(file).is_empty());
    assert_eq!(snap.parsed_tree(file).to_string(), "\\emph{hi}\n");
}

#[test]
fn document_signatures_is_memoized() {
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("\\newcommand{\\foo}{x}\n");
    db.clear_query_log();

    // Many reads, but the scan runs exactly once.
    let _ = db.document_signatures(file);
    let _ = db.document_signatures(file);
    let _ = db.document_signatures(file);

    assert_eq!(signatures_count(&db), 1);
    assert_eq!(scanned_commands(&db, file), vec!["foo".to_string()]);
}

#[test]
fn editing_definitions_rebuilds_signatures() {
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("\\newcommand{\\foo}{x}\n");
    db.clear_query_log();

    assert_eq!(scanned_commands(&db, file), vec!["foo".to_string()]);
    assert_eq!(signatures_count(&db), 1);

    // Adding a definition changes the text, so the scan re-runs.
    db.set_file_text(file, "\\newcommand{\\foo}{x}\n\\newcommand{\\bar}{y}\n");
    assert_eq!(
        scanned_commands(&db, file),
        vec!["bar".to_string(), "foo".to_string()]
    );
    assert_eq!(signatures_count(&db), 2);
}

#[test]
fn doc_associations_is_memoized() {
    // A `.dtx` path runs the docstrip mode, so the documentation margins parse and
    // the documented `macro` surfaces. Many reads, but the query runs once.
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(
        Path::new("doc.dtx"),
        "% \\begin{macro}{\\foo}\n% docs.\n% \\end{macro}\n".to_string(),
    );
    db.clear_query_log();

    let _ = db.doc_associations(file);
    let _ = db.doc_associations(file);
    let _ = db.doc_associations(file);

    assert_eq!(doc_assoc_count(&db), 1);
    let assocs = db.doc_associations(file);
    assert_eq!(assocs.len(), 1);
    assert_eq!(assocs[0].name, "\\foo");
}

#[test]
fn editing_dtx_rebuilds_doc_associations() {
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(
        Path::new("doc.dtx"),
        "% \\begin{macro}{\\foo}\n% docs.\n% \\end{macro}\n".to_string(),
    );
    db.clear_query_log();

    assert_eq!(db.doc_associations(file).len(), 1);
    assert_eq!(doc_assoc_count(&db), 1);

    // Documenting a second macro changes the text, so the query re-runs.
    db.upsert_file(
        Path::new("doc.dtx"),
        "% \\begin{macro}{\\foo}\n% docs.\n% \\end{macro}\n% \\begin{macro}{\\bar}\n% docs.\n% \\end{macro}\n".to_string(),
    );
    let names: Vec<_> = db
        .doc_associations(file)
        .iter()
        .map(|a| a.name.clone())
        .collect();
    assert_eq!(names, vec!["\\foo".to_string(), "\\bar".to_string()]);
    assert_eq!(doc_assoc_count(&db), 2);
}

#[test]
fn prose_edit_yields_equal_signatures() {
    // Value-stability stand-in for backdating: an edit touching no definition
    // leaves the scanned DB `==` its prior value, the precondition that makes
    // salsa backdate for completion's consumer.
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("\\newcommand{\\foo}{x}\n");

    // A fresh db with prose appended must scan to an equal DB.
    let mut other = IncrementalDatabase::default();
    let other_file = other.add_file("\\newcommand{\\foo}{x}\n\nsome text.\n");

    assert_eq!(
        db.document_signatures(file),
        other.document_signatures(other_file)
    );
}

#[test]
fn parsed_bib_document_is_memoized() {
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("@article{k, title = {Hi}}\n");
    db.clear_query_log();

    // Several consumers of the cached bib parse, but the parse runs once.
    let _ = db.parsed_bib_tree(file);
    let _ = db.parsed_bib_tree(file);
    let _ = db.bib_parse_diagnostics(file);
    let _ = db.bib_semantic_model(file);

    assert_eq!(bib_parse_count(&db), 1);
}

#[test]
fn editing_bib_text_reparses() {
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("@misc{a}\n");
    db.clear_query_log();

    let _ = db.parsed_bib_tree(file);
    assert_eq!(bib_parse_count(&db), 1);

    db.set_file_text(file, "@misc{b}\n");
    let _ = db.parsed_bib_tree(file);
    assert_eq!(bib_parse_count(&db), 2);
}

#[test]
fn cached_bib_tree_is_lossless() {
    let mut db = IncrementalDatabase::default();
    let input = "@article{k,\n  title = {Hi},\n  year = 2020,\n}\n";
    let file = db.add_file(input);

    assert_eq!(db.parsed_bib_tree(file).to_string(), input);
}

#[test]
fn bib_semantic_model_is_memoized() {
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("@book{k, publisher = cup}\n@string{cup = {C}}\n");
    db.clear_query_log();

    let _ = db.bib_semantic_model(file);
    let _ = db.bib_semantic_model(file);

    assert_eq!(bib_model_count(&db), 1);
}

#[test]
fn equal_bib_edit_yields_equal_model() {
    // Value-stability stand-in for backdating: two files whose entries/keys and
    // `@string` set match build `==` models, the precondition that makes salsa
    // backdate `bib_semantic_model` (it is `Eq`, not `no_eq`).
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("@article{k, title = {A}}\n");

    let mut other = IncrementalDatabase::default();
    let other_file = other.add_file("@article{k, title = {A}}\n");

    assert_eq!(
        db.bib_semantic_model(file),
        other.bib_semantic_model(other_file)
    );
}

#[test]
fn snapshot_shares_storage() {
    let mut db = IncrementalDatabase::default();
    let file = db.add_file("\\emph{hi}\n");
    db.clear_query_log();
    let _ = db.parsed_tree(file);
    assert_eq!(parse_count(&db), 1);

    // A clone is a second handle onto the same storage: the file's cached parse
    // is visible without re-running, and both handles share the query log.
    let clone = db.snapshot();
    let _ = clone.parsed_tree(file);
    assert_eq!(parse_count(&clone), 1);
    assert_eq!(clone.file_text(file), "\\emph{hi}\n");
}

// --- declarations (`meaning.toml`; AGENTS.md decision #12) -------------------

/// A resolved declaration block, written in the TOML the user actually types.
fn declared(toml_src: &str) -> ResolvedDeclarations {
    toml::from_str::<Declarations>(toml_src)
        .expect("declarations deserialize")
        .resolve()
        .expect("declarations resolve")
}

/// Declares `mycode` to behave like `lstlisting`, so its body is verbatim — a
/// fact no scan of the document below could ever discover.
const MYCODE_VERBATIM: &str = "[environments.mycode]\nlike = 'lstlisting'\n";

/// A document whose `mycode` body only reads as protected if the environment is
/// known to be verbatim.
const MYCODE_DOC: &str = "\\begin{mycode}\n\\bad{x}\n\\end{mycode}\n";

/// Whether the cached parse captured a protected body.
fn has_verbatim_body(
    db: &IncrementalDatabase,
    file: meaning_analysis::incremental::SourceId,
) -> bool {
    db.parsed_tree(file)
        .descendants_with_tokens()
        .any(|el| el.kind() == SyntaxKind::VERBATIM_BODY)
}

#[test]
fn declaring_an_environment_reparses_the_file() {
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(Path::new("main.tex"), MYCODE_DOC.to_owned());
    db.clear_query_log();

    // Declaration-blind, `mycode` is an unknown environment with an ordinary body.
    assert!(!has_verbatim_body(&db, file));
    assert_eq!(parse_count(&db), 1);

    db.clear_query_log();
    assert!(db.set_declarations(declared(MYCODE_VERBATIM)));

    // Editing the declarations reparses: the memo may not survive a change to
    // the one non-text input the parse is allowed to read.
    assert!(has_verbatim_body(&db, file));
    assert_eq!(parse_count(&db), 1);
}

#[test]
fn unchanged_declarations_do_not_reparse() {
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(Path::new("main.tex"), MYCODE_DOC.to_owned());
    assert!(db.set_declarations(declared(MYCODE_VERBATIM)));
    let _ = db.parsed_tree(file);

    db.clear_query_log();
    // Re-publishing the same block is what the language server does on every
    // dispatch, so it must not bump the revision — a write here would reparse
    // the whole database per keystroke.
    assert!(!db.set_declarations(declared(MYCODE_VERBATIM)));
    let _ = db.parsed_tree(file);

    assert_eq!(parse_count(&db), 0);
}

/// The other half of the firewall: a command alias cannot change a tree, so
/// editing one must leave every parse memo — and every reparse base — standing.
///
/// The two tiers share one salsa input, so without the
/// `parse_declarations`/`semantic_declarations` split this write would bump the
/// revision for every parse in the project, and `parsed_document` (`no_eq`)
/// could never backdate its way out.
#[test]
fn command_declarations_do_not_reparse() {
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(Path::new("main.tex"), MYCODE_DOC.to_owned());
    assert!(db.set_declarations(declared(MYCODE_VERBATIM)));
    assert!(has_verbatim_body(&db, file));

    db.clear_query_log();
    // The environment half is byte-for-byte what it was; only a command is added.
    let both = format!("{MYCODE_VERBATIM}\n[commands.myref]\nlike = 'cref'\n");
    assert!(db.set_declarations(declared(&both)));

    assert!(
        has_verbatim_body(&db, file),
        "the declared environment still stands"
    );
    assert_eq!(
        parse_count(&db),
        0,
        "a `[commands]` edit must not reparse the project"
    );
}

#[test]
fn command_declarations_rebuild_the_semantic_model() {
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(
        Path::new("main.tex"),
        "\\label{a}\\label{b}\\eqrefs{a,b}\n".to_owned(),
    );
    assert!(db.semantic_model(file).refs().is_empty());

    db.clear_query_log();
    assert!(db.set_declarations(declared("[commands.eqrefs]\nlike = 'cref'\n")));
    let model = db.semantic_model(file);
    assert_eq!(
        model
            .refs()
            .iter()
            .map(|reference| reference.name.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert!(model.labels().iter().all(|label| label.referenced));
    assert!(
        db.query_log()
            .iter()
            .any(|entry| entry.kind == QueryKind::SemanticModel)
    );
    assert_eq!(
        parse_count(&db),
        0,
        "the model rebuilt on the cached tree, not a fresh parse"
    );
}

#[test]
fn declarations_are_the_top_tier_of_the_signature_scope() {
    let mut db = IncrementalDatabase::default();
    // The file defines `mycode` itself, one-argument and non-verbatim; the
    // declaration corrects that inference and must win.
    let main = db.upsert_file(
        Path::new("main.tex"),
        "\\newenvironment{mycode}[1]{#1}{}\n".to_owned(),
    );
    let scanned = db
        .snapshot()
        .scope_signatures(main)
        .environment("mycode")
        .expect("scanned environment")
        .clone();
    assert_eq!(scanned.args.len(), 1);
    assert!(!scanned.verbatim_body);

    db.set_declarations(declared(MYCODE_VERBATIM));
    let scope = db.snapshot();
    let declared_sig = scope
        .scope_signatures(main)
        .environment("mycode")
        .expect("declared environment");
    assert!(
        declared_sig.verbatim_body,
        "a declaration outranks the file's own definition"
    );
}

// ---------------------------------------------------------------------------
// The incremental-reparse side channel
// ---------------------------------------------------------------------------
//
// Reuse is invisible in a query's value by construction — the governing invariant
// is that a splice and a full parse agree byte for byte — so these assert on the
// cache's own state, and assert the value is right anyway alongside it. Phase 1
// implements no tier, so nothing splices yet; what is pinned here is the channel's
// bookkeeping, which every later phase runs on.

/// A base is installed by the first parse and refreshed by the next one, so a
/// reparse always has something to splice against.
#[test]
fn parsing_populates_and_refreshes_the_reparse_base() {
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(Path::new("a.tex"), "\\section{One}\n".to_owned());

    assert!(db.reparse_prev(file).is_none(), "nothing parsed yet");

    db.parsed_tree(file);
    let first = db.reparse_prev(file).expect("a base after the first parse");
    assert_eq!(&**first.text, "\\section{One}\n");

    db.upsert_file(Path::new("a.tex"), "\\section{Two}\n".to_owned());
    db.parsed_tree(file);
    let second = db.reparse_prev(file).expect("a base after the edit");
    assert_eq!(&**second.text, "\\section{Two}\n");
}

/// The base shares the tracked text rather than copying it, so holding one costs a
/// refcount bump. A copy here would be a whole extra document per open buffer.
#[test]
fn the_reparse_base_shares_the_tracked_text() {
    let mut db = IncrementalDatabase::default();
    let text = "\\section{Hi}\n".into_source_text();
    let file = db.upsert_file(Path::new("a.tex"), text.clone());
    db.parsed_tree(file);

    let base = db.reparse_prev(file).expect("a base");
    assert!(
        std::sync::Arc::ptr_eq(&base.text, &text),
        "the base should hold the same allocation, not a copy"
    );
}

/// The base's tree is the one a full parse produces, so answering from it (which
/// the query does when it re-executes on unchanged text after a memo eviction) is
/// lossless like every other route.
///
/// The fast path's *predicate* is unit-tested in `src/incremental.rs`, where it is
/// visible; salsa gives no deterministic way to force a memo eviction from out here.
#[test]
fn the_reparse_base_holds_a_lossless_tree() {
    let mut db = IncrementalDatabase::default();
    let source = "\\section{Hi}\n\nbody $x^2$ % c\n\\begin{verbatim}\n  raw {\n\\end{verbatim}\n";
    let file = db.upsert_file(Path::new("a.tex"), source.to_owned());
    db.parsed_tree(file);

    let base = db.reparse_prev(file).expect("a base");
    let from_base = meaning_parser::syntax::SyntaxNode::new_root(base.green.clone());
    assert_eq!(from_base.to_string(), source);
    assert_eq!(db.parsed_tree(file).to_string(), source);
}

/// Long edit batches remain exact and are released when analysis consumes them.
#[test]
fn unread_edits_preserve_the_complete_transform() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new("a.tex");
    let mut text = "x\n".to_owned();
    let file = db.upsert_file(path, text.clone());
    db.parsed_tree(file);
    for inserted in std::iter::repeat_n("a".to_owned(), 100).chain(["z".repeat(65 * 1024)]) {
        let change = edit(0..0, &inserted);
        text = change.apply(&text);
        db.apply_change(path, text.clone(), Some(vec![change]));
        assert_eq!(db.reparse_pending_edits(file).len(), 1);
    }
    let full = meaning_parser::parser::parse(&text);
    assert_eq!(db.parsed_tree(file).green(), &*full.green);
    assert!(db.reparse_pending_edits(file).is_empty());
}

/// The language server's write phase, end to end: splice the `didChange` into the
/// live buffer, hand the text to salsa, stage the transform. This is the phase-2
/// contract — the chain the editor stages is *exactly* the transform out of the
/// base, which is the property `reparse_edits` verifies before it splices anything.
///
/// Driven through the real `apply_content_changes` rather than hand-built edits,
/// because the thing under test is precisely that the LSP-side offset resolution
/// and the parser-side chain agree.
#[test]
fn the_language_server_write_phase_stages_the_transform_out_of_the_base() {
    use lsp_types::{Position, Range};
    use meaning_analysis::text::{PositionEncoding, TextBuffer};
    use meaning_protocol::apply_content_changes;

    let source = "\\section{Hi}\n\nbody $x^2$\n";
    let mut buffer = std::sync::Arc::new(TextBuffer::new(source, PositionEncoding::Utf16));
    let mut db = IncrementalDatabase::default();
    let path = Path::new("a.tex");
    let file = db.upsert_file(path, buffer.text_arc());
    db.parsed_tree(file);

    let base = db.reparse_prev(file).expect("a base after the first parse");
    assert_eq!(&**base.text, source);

    // Three keystrokes, no parse demanded in between — the pull-diagnostics shape.
    for (line, character, insert) in [(2, 4, "X"), (2, 5, "Y"), (0, 9, "Z")] {
        let at = Position::new(line, character);
        let edits = apply_content_changes(
            &mut buffer,
            vec![
                lsp_types::TextDocumentContentChangePartial {
                    range: Range::new(at, at),
                    text: insert.to_owned(),
                    ..Default::default()
                }
                .into(),
            ],
        )
        .unwrap();
        db.apply_change(path, buffer.text_arc(), edits);
    }

    assert!(db.reparse_pending_edits(file).is_empty());
    assert!(
        db.reparse_prev(file).is_none(),
        "disjoint edits discard the reparse base"
    );

    // And the parse it feeds still answers exactly what a full parse would, then
    // drains what it consumed.
    assert_eq!(db.parsed_tree(file).to_string(), buffer.text());
    assert!(db.reparse_pending_edits(file).is_empty());
    let refreshed = db.reparse_prev(file).expect("a refreshed base");
    assert_eq!(&**refreshed.text, buffer.text());
}

/// Staging `None` means "the text changed by a route carrying no edits" — a disk
/// reload, a sweep. The chain must go, or it would claim to describe a transform it
/// does not.
#[test]
fn staging_an_unknown_transform_clears_the_chain() {
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(Path::new("a.tex"), "x\n");
    db.parsed_tree(file);
    db.apply_change(Path::new("a.tex"), "ax\n", Some(vec![edit(0..0, "a")]));
    assert_eq!(db.reparse_pending_edits(file).len(), 1);

    db.apply_change(Path::new("a.tex"), db.file_text(file).to_owned(), None);
    assert!(db.reparse_pending_edits(file).is_empty());
}

/// Clearing a chain a file does not have is a no-op, entry included. The language
/// server pairs *every* `upsert_file` with a stage so the rule needs no exceptions,
/// and most of those writes are project seeding — a directory walk that reads every
/// sibling off disk. Minting an empty entry per sibling would fill the cache with
/// files nothing is editing, and only a later store ever sweeps them.
#[test]
fn staging_an_unknown_transform_does_not_mint_a_cache_entry() {
    let mut db = IncrementalDatabase::default();
    let file = db.apply_change(Path::new("a.tex"), "x\n".to_owned(), None);
    assert_eq!(
        db.reparse_cache_len(),
        0,
        "nothing to clear, nothing to hold"
    );

    // A transform without a parsed base has nothing to reuse.
    db.apply_change(
        Path::new("a.tex"),
        db.file_text(file).to_owned(),
        Some(vec![edit(0..0, "a")]),
    );
    assert_eq!(db.reparse_cache_len(), 0);
    db.apply_change(Path::new("a.tex"), db.file_text(file).to_owned(), None);
    assert_eq!(db.reparse_cache_len(), 0);
}

/// A parse drains the chain it consumed **even when it did not splice**. A chain
/// kept back because it failed to verify is stale forever after — it describes a
/// transform out of a text the base no longer holds — so it would fail on every
/// later parse and poison them all.
#[test]
fn a_chain_is_drained_even_when_it_does_not_splice() {
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(Path::new("a.tex"), "x\n".to_owned());
    db.parsed_tree(file);

    // A chain that does not describe the transform at all.
    db.apply_change(
        Path::new("a.tex"),
        db.file_text(file).to_owned(),
        Some(vec![edit(0..0, "nonsense")]),
    );
    db.upsert_file(Path::new("a.tex"), "y\n".to_owned());
    assert_eq!(db.parsed_tree(file).to_string(), "y\n");

    assert!(
        db.reparse_pending_edits(file).is_empty(),
        "the consumed chain must be dropped whether or not it spliced"
    );
}

/// Each tracked source keeps its reparse base until that source is removed.
#[test]
fn reparse_cache_ownership_matches_tracked_sources() {
    let mut db = IncrementalDatabase::default();
    let mut files = Vec::new();
    for n in 0..200 {
        let file = db.upsert_file(
            Path::new(&format!("f{n}.tex")),
            format!("\\section{{S{n}}}\n"),
        );
        db.parsed_tree(file);
        files.push(file);
    }

    assert_eq!(db.reparse_cache_len(), files.len());
    for n in 1..files.len() {
        db.remove_file(Path::new(&format!("f{n}.tex")));
    }
    assert_eq!(db.reparse_cache_len(), 1);

    let first = files[0];
    assert_eq!(db.parsed_tree(first).to_string(), "\\section{S0}\n");
}

/// A project-wide sweep parses every member, each storing a base it will never hit.
/// Under a plain LRU that would cost the buffer being edited its base and turn the
/// next keystroke into a full parse, so the sweep's cold entries must go first.
#[test]
fn a_sweep_does_not_evict_an_edited_buffer() {
    let mut db = IncrementalDatabase::default();
    let edited = db.upsert_file(Path::new("edited.tex"), "\\section{Hi}\n".to_owned());
    db.parsed_tree(edited);
    // An editor staging a real chain is what marks the entry hot.
    db.apply_change(
        Path::new("edited.tex"),
        db.file_text(edited).to_owned(),
        Some(vec![edit(0..0, "x")]),
    );

    // Now sweep far more files than the cache holds.
    for n in 0..200 {
        let file = db.upsert_file(
            Path::new(&format!("swept{n}.tex")),
            format!("\\section{{S{n}}}\n"),
        );
        db.parsed_tree(file);
    }

    assert!(
        db.reparse_prev(edited).is_some(),
        "the swept files should have evicted each other, not the edited buffer"
    );
}

/// Closing a file drops its base: the buffer it described is gone, and a later
/// `didOpen` mints a fresh input that could never hit the entry anyway.
#[test]
fn closing_a_file_evicts_its_reparse_base() {
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(Path::new("a.tex"), "\\section{Hi}\n".to_owned());
    db.parsed_tree(file);
    assert!(db.reparse_prev(file).is_some());

    db.remove_file(Path::new("a.tex"));
    assert!(db.reparse_prev(file).is_none());
}

#[test]
fn batch_removal_preserves_survivors_and_reopening_changes_identity() {
    let mut db = IncrementalDatabase::default();
    let first = db.upsert_file(Path::new("a.tex"), "a");
    let survivor = db.upsert_file(Path::new("b.tex"), "\\section{Kept}\n");
    db.upsert_file(Path::new("c.tex"), "c");
    db.parsed_tree(survivor);
    let base = db.reparse_prev(survivor).unwrap();
    let snapshot = db.snapshot();
    let epoch = snapshot.epoch();
    let version = snapshot.source_version(survivor).unwrap();

    db.remove_files(&["a.tex".into(), "c.tex".into(), "a.tex".into()]);
    assert_eq!(db.tracked_files(), vec![("b.tex".into(), survivor)]);
    assert_eq!(snapshot.tracked_files().len(), 3);
    assert_eq!(snapshot.file_text(first), "a");
    assert_eq!(snapshot.epoch(), epoch);
    assert_ne!(db.epoch(), epoch);
    assert_eq!(db.source_version(survivor), Some(version));
    assert!(std::sync::Arc::ptr_eq(
        &base,
        &db.reparse_prev(survivor).unwrap()
    ));
    assert_eq!(db.parsed_tree(survivor).to_string(), "\\section{Kept}\n");
    drop(snapshot);

    assert_ne!(db.upsert_file(Path::new("a.tex"), "a"), first);
}

#[test]
fn checked_edits_are_atomic_and_reject_stale_source_incarnations() {
    use meaning_analysis::incremental::ChangeError;
    let mut db = IncrementalDatabase::default();
    let path = Path::new("a.tex");
    let source = db.upsert_file(path, "𝕏\r\n");
    let version = db.source_version(source).unwrap();
    let epoch = db.epoch();
    db.parsed_tree(source);
    let base = db.reparse_prev(source).unwrap();
    assert_eq!(
        db.edit_source(version, vec![edit(4..4, "a"), edit(1..2, "bad")]),
        Err(ChangeError::InvalidEdit { index: 1 }),
    );
    assert_eq!(db.file_text(source), "𝕏\r\n");
    assert_eq!(db.source_version(source), Some(version));
    assert!(std::sync::Arc::ptr_eq(
        &base,
        &db.reparse_prev(source).unwrap()
    ));

    let next = db
        .edit_source(version, vec![edit(4..4, "a"), edit(5..5, "b")])
        .unwrap();
    assert_eq!(db.file_text(source), "𝕏ab\r\n");
    assert_ne!(next, version);
    assert_eq!(db.epoch(), epoch);
    assert_eq!(
        db.replace_source(version, "stale"),
        Err(ChangeError::StaleRevision)
    );
    assert_eq!(db.file_text(source), "𝕏ab\r\n");
    assert_eq!(db.replace_source(next, "𝕏ab\r\n"), Ok(next));

    db.remove_file(path);
    let reopened = db.upsert_file(path, "reopened");
    assert_eq!(
        db.replace_source(next, "stale"),
        Err(ChangeError::SourceRemoved)
    );
    assert_eq!(db.file_text(reopened), "reopened");
}

#[test]
fn shared_sources_have_project_specific_parse_contexts() {
    let mut db = IncrementalDatabase::default();
    let plain = db.project_id();
    let verbatim = db.create_project(declared(MYCODE_VERBATIM));
    let path = Path::new("/shared/common.tex");
    let source = "\\begin{mycode}\n{ raw \\section{Hidden}\n\\end{mycode}\n".into_source_text();
    let id = db.upsert_file(path, source.clone());
    assert_eq!(
        db.apply_project_change(verbatim, path, source, None)
            .unwrap(),
        id
    );
    {
        let left = db.snapshot_for(plain).unwrap();
        let right = db.snapshot_for(verbatim).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            left.source_text(id),
            right.source_text(id)
        ));
        assert_ne!(left.parsed_tree(id).green(), right.parsed_tree(id).green());
        assert_eq!(
            left.declarations_for(path),
            &ResolvedDeclarations::default()
        );
        assert_eq!(right.declarations_for(path), &declared(MYCODE_VERBATIM));
    }
    let version = db
        .snapshot_for(verbatim)
        .unwrap()
        .source_version(id)
        .unwrap();
    let next = db
        .edit_source(version, vec![edit(0..0, "prefix\n")])
        .unwrap();
    assert_eq!(db.source_version(id).unwrap().revision, next.revision);
    assert!(db.file_text(id).starts_with("prefix\n"));
    {
        let right = db.snapshot_for(verbatim).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            db.source_text(id),
            right.source_text(id)
        ));
        assert_eq!(right.parsed_tree(id).to_string(), db.file_text(id));
    }
    let old = db.snapshot_for(verbatim).unwrap();
    assert!(db.remove_project(verbatim));
    assert!(db.snapshot_for(verbatim).is_err());
    assert_eq!(old.source_version(id), Some(next));
    assert!(old.parsed_tree(id).to_string().starts_with("prefix\n"));
    assert_eq!(db.lookup_file(path), Some(id));
    drop(old);
    assert!(db.replace_source(next, "stale").is_err());
}

#[test]
fn overlays_hide_backing_updates_until_close() {
    let mut db = IncrementalDatabase::default();
    let project = db.project_id();
    let path = Path::new("/project/main.tex");
    let source = db
        .set_backing(project, path, Some("disk".into_source_text()))
        .unwrap()
        .unwrap();
    assert_eq!(db.open_overlay(project, path, "editor").unwrap(), source);
    db.parsed_tree(source);
    db.clear_query_log();
    db.set_backing(project, path, Some("changed disk".into_source_text()))
        .unwrap();
    assert_eq!(db.parsed_tree(source).to_string(), "editor");
    assert!(db.query_log().is_empty());
    let version = db.source_version(source).unwrap();
    db.edit_source(version, vec![edit(6..6, " edit")]).unwrap();
    assert_eq!(db.file_text(source), "editor edit");
    assert_eq!(db.close_overlay(project, path).unwrap(), Some(source));
    assert_eq!(db.parsed_tree(source).to_string(), "changed disk");
}

#[test]
fn backing_deletion_preserves_an_open_overlay_and_close_releases_it() {
    let mut db = IncrementalDatabase::default();
    let project = db.project_id();
    let path = Path::new("/project/main.tex");
    db.set_backing(project, path, Some("disk".into_source_text()))
        .unwrap();
    let overlay = "unsaved".into_source_text();
    let weak = std::sync::Arc::downgrade(&overlay);
    let source = db.open_overlay(project, path, overlay).unwrap();
    db.parsed_tree(source);
    db.set_backing(project, path, None).unwrap();
    assert_eq!(db.file_text(source), "unsaved");
    assert_eq!(db.close_overlay(project, path).unwrap(), None);
    assert!(db.lookup_file(path).is_none());
    assert_eq!(weak.strong_count(), 0);
}

#[test]
fn removing_an_overlay_project_restores_other_projects_backing() {
    let mut db = IncrementalDatabase::default();
    let backing_project = db.project_id();
    let editor_project = db.create_project(ResolvedDeclarations::default());
    let path = Path::new("/shared/main.tex");
    let source = db
        .set_backing(backing_project, path, Some("disk".into_source_text()))
        .unwrap()
        .unwrap();
    db.open_overlay(editor_project, path, "unsaved").unwrap();
    let old = db.snapshot_for(editor_project).unwrap();
    db.remove_project(editor_project);
    assert_eq!(db.parsed_tree(source).to_string(), "disk");
    assert_eq!(old.parsed_tree(source).to_string(), "unsaved");
}

#[test]
fn a_detached_project_membership_cannot_reuse_an_old_edit_precondition() {
    let mut db = IncrementalDatabase::default();
    let project = db.project_id();
    let other = db.create_project(ResolvedDeclarations::default());
    let path = Path::new("/shared/member.tex");
    let source = db.upsert_file(path, "original");
    db.apply_project_change(other, path, "original", None)
        .unwrap();
    let old = db.source_version(source).unwrap();
    db.remove_project_files(project, &[path.to_path_buf()]);
    assert_eq!(db.upsert_file(path, "original"), source);
    assert!(db.replace_source(old, "late update").is_err());
    assert_eq!(db.file_text(source), "original");
}

#[test]
fn attaching_an_unchanged_source_preserves_existing_reparse_history() {
    let mut db = IncrementalDatabase::default();
    let other = db.create_project(ResolvedDeclarations::default());
    let path = Path::new("/shared/common.tex");
    let source = db.upsert_file(path, "original text\n");
    db.parsed_tree(source);
    let base = db.reparse_prev(source).unwrap();
    db.edit_source(
        db.source_version(source).unwrap(),
        vec![edit(0..8, "changed")],
    )
    .unwrap();
    let pending = db.reparse_pending_edits(source);
    let text = db.source_text(source).clone();
    db.apply_project_change(other, path, text, None).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &base,
        &db.reparse_prev(source).unwrap()
    ));
    assert_eq!(db.reparse_pending_edits(source), pending);
    assert_eq!(db.parsed_tree(source).to_string(), "changed text\n");
}

#[test]
fn project_membership_and_resolution_do_not_cross_contexts() {
    let mut db = IncrementalDatabase::default();
    let left = db.project_id();
    let right = db.create_project(ResolvedDeclarations::default());
    let main = Path::new("/shared/main.tex");
    let source = "\\input{chapter}\n\\ref{key}\n".into_source_text();
    db.upsert_file(main, source.clone());
    db.apply_project_change(right, main, source, None).unwrap();
    db.apply_project_change(
        right,
        Path::new("/shared/chapter.tex"),
        "\\label{key}\n",
        None,
    )
    .unwrap();
    {
        let a = db.snapshot_for(left).unwrap();
        let b = db.snapshot_for(right).unwrap();
        assert_eq!(a.project_members().len(), 1);
        assert_eq!(b.project_members().len(), 2);
        assert_eq!(a.resolve_labels().namespace_members(main).len(), 1);
        assert_eq!(b.resolve_labels().namespace_members(main).len(), 2);
        a.project_graph();
    }
    db.clear_query_log();
    db.apply_project_change(right, Path::new("/right/unrelated.tex"), "prose", None)
        .unwrap();
    db.project_graph();
    assert!(
        db.query_log()
            .iter()
            .all(|entry| entry.kind != QueryKind::ProjectGraph)
    );
    db.remove_project_files(right, &[main.to_owned()]);
    assert!(db.lookup_file(main).is_some());
    assert!(db.snapshot_for(right).unwrap().lookup_file(main).is_none());
}

#[test]
fn source_declarations_survive_defaults_and_membership_changes() {
    let mut db = IncrementalDatabase::default();
    let explicit = Path::new("explicit.tex");
    // An explicit default value must remain an override when defaults change.
    db.set_source_declarations(explicit, ResolvedDeclarations::default());
    db.set_source_declarations(Path::new("future.tex"), declared(MYCODE_VERBATIM));
    db.upsert_file(explicit, "");
    db.upsert_file(Path::new("removed.tex"), "");
    db.set_declarations(declared(MYCODE_VERBATIM));
    assert_eq!(
        db.declarations_for(explicit),
        &ResolvedDeclarations::default()
    );
    db.remove_file(Path::new("removed.tex"));
    assert_eq!(
        db.declarations_for(explicit),
        &ResolvedDeclarations::default()
    );
    assert_eq!(
        db.declarations_for(Path::new("future.tex")),
        &declared(MYCODE_VERBATIM)
    );
}

/// The base carries the inputs its tree was produced under, not just the text, so a
/// later parse can tell whether it is usable. Editing `meaning.toml` changes what a
/// parse means for the same bytes.
#[test]
fn the_reparse_base_carries_the_declarations_it_was_parsed_under() {
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(
        Path::new("main.tex"),
        "\\newenvironment{mycode}[1]{#1}{}\n".to_owned(),
    );
    db.parsed_tree(file);
    let before = db.reparse_prev(file).expect("a base");
    assert_eq!(before.declared, ResolvedDeclarations::default());

    db.set_declarations(declared(MYCODE_VERBATIM));
    db.parsed_tree(file);
    let after = db.reparse_prev(file).expect("a base");
    assert_ne!(
        after.declared,
        ResolvedDeclarations::default(),
        "the refreshed base must record the declarations in force"
    );
}

#[test]
fn external_refreshes_reject_late_results_and_preserve_snapshot_observations() {
    use meaning_analysis::external::*;
    use meaning_analysis::incremental::ChangeError;
    let mut db = IncrementalDatabase::default();
    let project = db.project_id();
    let path = Path::new("/project/image.pdf");
    let old = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .unwrap();
    let current = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .unwrap();
    let present = ExternalInputs::Files(FileInputs {
        locations: vec![(
            path.into(),
            LocationObservation {
                kind: Observation::Present(LocationKind::File),
                ..Default::default()
            },
        )],
        ..Default::default()
    });
    assert_eq!(
        db.apply_external_inputs(old, present.clone()),
        Err(ChangeError::StaleRevision)
    );
    assert_eq!(db.location_observation(path).kind, Observation::Unknown);
    db.apply_external_inputs(current, present.clone()).unwrap();
    assert_eq!(
        db.apply_external_inputs(current, present),
        Err(ChangeError::StaleRevision)
    );
    let temporary = Path::new("/project/temporary.tex");
    db.set_backing(project, temporary, Some("temporary".into_source_text()))
        .unwrap();
    let snapshot = db.snapshot();
    db.set_backing(project, temporary, None).unwrap();
    let next = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .unwrap();
    db.apply_external_inputs(
        next,
        ExternalInputs::Files(FileInputs {
            locations: vec![(
                path.into(),
                LocationObservation {
                    kind: Observation::Absent,
                    ..Default::default()
                },
            )],
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(
        snapshot.location_observation(path).kind,
        Observation::Present(LocationKind::File)
    );
    assert_eq!(db.location_observation(path).kind, Observation::Absent);
    assert_eq!(
        snapshot.external_generations().files + 1,
        db.external_generations().files
    );
}

#[test]
fn external_categories_refresh_independently_and_cannot_revive_removed_sources() {
    use meaning_analysis::external::*;
    use meaning_analysis::incremental::ChangeError;
    let mut db = IncrementalDatabase::default();
    let project = db.project_id();
    let path = Path::new("/project/main.tex");
    db.set_backing(project, path, Some("original".into_source_text()))
        .unwrap();
    let files = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .unwrap();
    let installed = db
        .begin_external_refresh(project, ExternalInputKind::Installed)
        .unwrap();
    db.set_backing(project, path, None).unwrap();
    assert_eq!(
        db.apply_external_inputs(
            files,
            ExternalInputs::Files(FileInputs {
                backing: vec![(path.into(), Some("late".into_source_text()))],
                ..Default::default()
            })
        ),
        Err(ChangeError::StaleRevision)
    );
    db.apply_external_inputs(installed, ExternalInputs::Installed(Observation::Absent))
        .unwrap();
    assert_eq!(db.installed_metadata(), &Observation::Absent);
    assert!(db.lookup_file(path).is_none());
    let pending = db
        .begin_external_refresh(project, ExternalInputKind::Compiler)
        .unwrap();
    db.remove_project(project);
    assert_eq!(
        db.apply_external_inputs(pending, ExternalInputs::Compiler(Vec::new())),
        Err(ChangeError::ProjectRemoved)
    );
}

#[test]
fn external_batch_removal_keeps_surviving_observations_and_last_duplicate_value() {
    use meaning_analysis::external::*;
    let mut db = IncrementalDatabase::default();
    let project = db.project_id();
    let a = Path::new("/project/a.tex");
    let b = Path::new("/project/b.tex");
    db.set_backing_batch(
        project,
        [
            (a.into(), Some("a".into_source_text())),
            (b.into(), Some("b".into_source_text())),
        ],
    )
    .unwrap();
    let token = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Files(FileInputs {
            backing: vec![
                (a.into(), None),
                (b.into(), None),
                (b.into(), Some("new b".into_source_text())),
            ],
            bibliography: vec![(
                "/project/refs.bib".into(),
                Observation::Present("/installed/refs.bib".into()),
            )],
            ..Default::default()
        }),
    )
    .unwrap();
    assert!(db.lookup_file(a).is_none());
    assert_eq!(&***db.source_text(db.lookup_file(b).unwrap()), "new b");
    assert_eq!(
        db.bibliography_alias(Path::new("/project/refs.bib")),
        Some(Path::new("/installed/refs.bib"))
    );
}

#[test]
fn incomplete_directory_observations_do_not_prove_absence() {
    use meaning_analysis::external::*;
    let mut db = IncrementalDatabase::default();
    let project = db.project_id();
    let directory = Path::new("/project");
    let present = Path::new("/project/present.tex");
    let absent = Path::new("/project/other.tex");
    for complete in [false, true] {
        let token = db
            .begin_external_refresh(project, ExternalInputKind::Files)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Files(FileInputs {
                locations: vec![(
                    directory.into(),
                    LocationObservation {
                        kind: Observation::Present(LocationKind::Directory),
                        directory: Observation::Present(DirectoryContents {
                            entries: [("present.tex".into(), LocationKind::File)].into(),
                            complete,
                        }),
                    },
                )],
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(
            db.location_kind(present),
            Observation::Present(LocationKind::File)
        );
        assert_eq!(
            db.location_kind(absent),
            if complete {
                Observation::Absent
            } else {
                Observation::Unknown
            }
        );
    }
    db.open_overlay(project, absent, "unsaved").unwrap();
    assert_eq!(
        db.location_kind(absent),
        Observation::Present(LocationKind::File)
    );
    db.close_overlay(project, absent).unwrap();
    assert_eq!(db.location_kind(absent), Observation::Absent);
}

#[test]
fn unknown_earlier_candidates_prevent_definitive_fallback_resolution() {
    use meaning_analysis::external::*;
    use meaning_analysis::project::texmf::TexmfIndex;
    let mut db = IncrementalDatabase::default();
    let project = db.project_id();
    let candidates = FileCandidates::new("package", &["sty"], true, Some(Path::new("/project")));
    let installed = db
        .begin_external_refresh(project, ExternalInputKind::Installed)
        .unwrap();
    db.apply_external_inputs(
        installed,
        ExternalInputs::Installed(Observation::Present(InstalledMetadata {
            toolchain: "test".into(),
            index: TexmfIndex::from_files(
                [("package.sty".into(), "/installed/package.sty".into())].into(),
            ),
        })),
    )
    .unwrap();
    assert_eq!(db.resolve_file(&candidates).target, Observation::Unknown);
    assert_eq!(
        db.resolve_file(&candidates).needs,
        vec![
            InputNeed::Location("/project/package.sty".into()),
            InputNeed::Location("/project/package.dtx".into())
        ]
    );
    db.open_overlay(project, Path::new("/project/package.dtx"), "source")
        .unwrap();
    assert_eq!(db.resolve_file(&candidates).target, Observation::Unknown);
    let files = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .unwrap();
    db.apply_external_inputs(
        files,
        ExternalInputs::Files(FileInputs {
            locations: vec![(
                "/project/package.sty".into(),
                LocationObservation {
                    kind: Observation::Absent,
                    ..Default::default()
                },
            )],
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(
        db.resolve_file(&candidates).target,
        Observation::Present("/project/package.dtx".into())
    );
    db.close_overlay(project, Path::new("/project/package.dtx"))
        .unwrap();
    let files = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .unwrap();
    db.apply_external_inputs(
        files,
        ExternalInputs::Files(FileInputs {
            locations: vec![(
                "/project/package.dtx".into(),
                LocationObservation {
                    kind: Observation::Absent,
                    ..Default::default()
                },
            )],
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(
        db.resolve_file(&candidates).target,
        Observation::Present("/installed/package.sty".into())
    );
    let files = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .unwrap();
    db.apply_external_inputs(
        files,
        ExternalInputs::Files(FileInputs {
            locations: vec![(
                "/project/package.sty".into(),
                LocationObservation {
                    kind: Observation::Error("permission denied".into()),
                    ..Default::default()
                },
            )],
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(
        db.resolve_file(&candidates).target,
        Observation::Error("permission denied".into())
    );
}
