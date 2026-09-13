//! Queries for incremental analysis.
use super::*;

#[salsa::tracked(returns(ref), no_eq, unsafe(non_salsa_values))]
pub fn parsed_document(db: &dyn IncrementalDb, file: SourceInput) -> ParsedDocument {
    db.record_query(QueryLogEntry {
        kind: QueryKind::ParsedDocument,
        file: Some(*file.identity(db)),
    });

    // Parse with the config implied by the file's extension: a `.sty`/`.cls` is
    // loaded under an implicit `\makeatletter` (`LatexFlavor::Package`), so `@` is
    // a letter throughout, and a `.dtx` runs the docstrip mode.
    // `file_kind_or_tex` reads only the path name.
    let config = file_kind_or_tex(file.path(db)).lex_config();
    // The project's declarations seed the parse context, so a declared `\bea`
    // pairs here exactly as it does on the CLI's `parse_with_declarations` path.
    // Reading them registers a `HIGH`-durability dependency on the *parse-facing*
    // half: editing `[environments]` reparses every file, and nothing else does —
    // a `[commands]` edit backdates at the firewall.
    let declared = parse_declarations_of(db, file);
    let text = file.text(db);

    // The side channel (see `IncrementalDb::reparse_prev`). Everything below is a
    // hint: each branch produces exactly what a full parse of `text` would, so a
    // cold or stale cache costs a parse and nothing else.
    let state = db.reparse_state(file);
    let staged = state.pending;
    let prev = state.prev;

    if let Some(prev) = prev
        .as_ref()
        .filter(|prev| prev.is_current(text, config, declared))
    {
        return prev.clone();
    }
    let reparsed = prev
        .as_ref()
        .and_then(|prev| reparse_edits(&prev.as_reparse_base(declared), &staged, text));
    let (green, errors, ctx) = match reparsed {
        Some(parsed) => (
            parsed.green,
            parsed.errors,
            prev.as_ref().unwrap().ctx.clone(),
        ),
        None => {
            let (parsed, ctx) = parse_with_declarations_resolved(text, config, declared);
            (parsed.green, parsed.errors, ctx)
        }
    };
    let artifact = Arc::new(PrevParse {
        text: text.clone(),
        green,
        errors,
        ctx,
        config,
        declared: declared.clone(),
    });
    db.reparse_store(file, artifact.clone(), staged.len(), state.generation);
    artifact
}

/// The parse diagnostics for `file` (empty when the file parses cleanly).
pub fn parse_diagnostics(db: &dyn IncrementalDb, file: SourceInput) -> &[ParseDiagnosticData] {
    &parsed_document(db, file).errors
}

/// Materialize the cached parse for `file` as a fresh `SyntaxNode` cursor.
pub fn parsed_tree_root(db: &dyn IncrementalDb, file: SourceInput) -> SyntaxNode {
    SyntaxNode::new_root(parsed_document(db, file).green.clone())
}

/// The per-file label/reference model, built on the cached parse tree.
///
/// Unlike [`parsed_document`], this query is **not** `no_eq`: [`SemanticModel`]
/// *is* `Eq`, so salsa compares outputs and **backdates** when an edit leaves
/// the model unchanged (e.g. a prose edit that touches no `\label`/`\ref`),
/// keeping any downstream query from re-running. (`parsed_document` must be
/// `no_eq` only because its `GreenNode` is neither `Eq` nor `salsa::SalsaValue`, so
/// salsa cannot compare parses and falls back to text-input change detection.)
/// Cross-file label resolution reuses these facts after unrelated prose edits.
#[salsa::tracked(returns(ref))]
pub fn semantic_model(db: &dyn IncrementalDb, file: SourceInput) -> SemanticModel {
    db.record_query(QueryLogEntry {
        kind: QueryKind::SemanticModel,
        file: Some(*file.identity(db)),
    });
    SemanticModel::build_with_declarations(
        &parsed_tree_root(db, file),
        semantic_declarations_of(db, file),
    )
}

/// The file's scanned user-definition signatures — `\newcommand`,
/// `\newenvironment`, and the xparse `\NewDocument…` family
/// ([`crate::semantic::scan_definitions`]) — built on the cached parse tree.
///
/// Like [`semantic_model`] (and unlike [`parsed_document`]) this is **not**
/// `no_eq`: [`SignatureDb`] is `Eq`, so salsa backdates when an edit defines no
/// new command/environment (e.g. a prose or `\ref` edit), keeping completion's
/// consumer from re-running. Its first consumer is the language server's
/// completion request, which unions these scanned names with the built-in DB.
#[salsa::tracked(returns(ref))]
pub fn document_signatures(db: &dyn IncrementalDb, file: SourceInput) -> SignatureDb {
    db.record_query(QueryLogEntry {
        kind: QueryKind::DocumentSignatures,
        file: Some(*file.identity(db)),
    });
    scan_definitions(&parsed_tree_root(db, file))
}

/// The file's merged **signature scope**: the scanned definitions of every package
/// it transitively loads (local `.sty`/`.cls` members of `project`), unioned in
/// load order, with the file's *own* [`document_signatures`] overlaid on top so a
/// document redefinition wins over any package. Built from the cross-file
/// [`package_graph`](crate::project::package_graph) and the per-file
/// [`document_signatures`] firewall, with the project's declarations
/// on the source input folded in last.
///
/// Like [`document_signatures`] this is **not** `no_eq`: [`SignatureDb`] is `Eq`,
/// so it backdates when no definition-relevant edit occurred anywhere in the
/// loaded set. Its consumers are the formatter (package-defined arities/verbatim)
/// and completion. A name like `amsmath` with no sibling `amsmath.sty` simply
/// contributes nothing — resolution is local-only.
#[salsa::tracked(returns(ref))]
pub fn scope_signatures(db: &dyn IncrementalDb, file: SourceInput) -> SignatureDb {
    db.record_query(QueryLogEntry {
        kind: QueryKind::ScopeSignatures,
        file: Some(*file.identity(db)),
    });

    let context = db.project_input(*file.project(db));
    let graph = package_graph(db, context);
    let by_path = crate::project::root_views::project_sources(db, context);

    let mut merged = SignatureDb::default();
    for loaded in graph.transitively_loaded(file.path(db)) {
        if let Some(&member) = by_path.get(loaded.as_path()) {
            // Tag each merged name with the package's file stem, so hover can
            // name the defining package (mirrors `semantic::load`).
            match loaded.file_stem().and_then(|s| s.to_str()) {
                Some(origin) => {
                    merged.merge_from(document_signatures(db, member), Some(origin));
                }
                None => merged.merge_from(document_signatures(db, member), None),
            }
        }
    }
    // The document's own definitions are applied last, so they win over packages
    // (and clear any package origin for a shadowed name).
    merged.merge_from(document_signatures(db, file), None);
    // Except the project's declarations, the top tier: a declaration is the user
    // explicitly correcting an inference. Same order as the disk-backed
    // `collect_package_signatures`, so the two scope builders cannot disagree.
    // The parse-facing half is the whole of it here: `merge_declarations` reads
    // only the signature tier, so a `[commands]` edit must not rebuild a scope.
    merged.merge_declarations(parse_declarations_of(db, file));
    merged
}

/// The file's `.dtx` documentation↔code associations
/// ([`crate::semantic::doc_associations`]) — each documented `macro`/`environment`
/// or `\DescribeMacro`/`\DescribeEnv` paired with the `macrocode` it brackets.
///
/// Like [`semantic_model`] (and unlike [`parsed_document`]) this is **not** `no_eq`:
/// `Vec<DocAssociation>` is `Eq`, so salsa backdates when an edit changes no
/// documented construct. The query runs on any file; a non-`.dtx` source simply
/// carries none of the ltxdoc vocabulary, so the result is empty.
#[salsa::tracked(returns(ref))]
pub fn doc_associations(db: &dyn IncrementalDb, file: SourceInput) -> Vec<DocAssociation> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::DocAssociations,
        file: Some(*file.identity(db)),
    });
    build_doc_associations(&parsed_tree_root(db, file))
}

/// The file's inclusion edges, range-free
/// ([`crate::project::collect_include_edge_keys`]), as a tracked query. Resolves
/// relative targets against the file's own directory (`path.parent()`); the path
/// is an input field set once, so this re-runs only on a text edit and backdates
/// when the edges are unchanged — the firewall that keeps a body edit from
/// rebuilding the cross-file [`crate::project::project_graph`].
#[salsa::tracked(returns(ref))]
pub fn include_edges(db: &dyn IncrementalDb, file: SourceInput) -> Vec<IncludeEdgeKey> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::IncludeEdges,
        file: Some(*file.identity(db)),
    });
    let root = parsed_tree_root(db, file);
    collect_include_edge_keys(&root, file.path(db).parent())
}

/// The file's package/class load edges, range-free
/// ([`crate::project::collect_package_edge_keys`]), as a tracked query — the
/// load-graph analog of [`include_edges`]. Resolves relative `.sty`/`.cls` targets
/// against the file's own directory; backdates when the load edges are unchanged,
/// the firewall that keeps a body edit from rebuilding
/// [`crate::project::package_graph`].
#[salsa::tracked(returns(ref))]
pub fn package_edges(db: &dyn IncrementalDb, file: SourceInput) -> Vec<PackageEdgeKey> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::PackageEdges,
        file: Some(*file.identity(db)),
    });
    let root = parsed_tree_root(db, file);
    collect_package_edge_keys(&root, file.path(db).parent())
}

/// The file's distinct `\label` names, sorted — a range-free, ref-free
/// projection of [`semantic_model`].
///
/// This is the per-file firewall the cross-file
/// [`crate::project::resolved_labels`] resolver consumes. Stripping ranges and
/// refs means a prose edit, or a
/// `\ref` edit, or a body edit that shifts a `\label`'s offset, leaves this
/// `Vec` *equal* — salsa backdates and the project-level union is not rebuilt.
/// Unlike [`project_graph`](crate::project::project_graph) it is **not** `no_eq`:
/// `Vec<SmolStr>` is `Eq`, which is exactly what makes the firewall hold (same
/// reasoning as [`semantic_model`]).
#[salsa::tracked(returns(ref))]
pub fn file_labels(db: &dyn IncrementalDb, file: SourceInput) -> Vec<SmolStr> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::FileLabels,
        file: Some(*file.identity(db)),
    });
    document_label_names(semantic_model(db, file))
}

/// The file's distinct `\ref`-family key names, sorted — a range-free projection
/// of [`semantic_model`], the reference mirror of [`file_labels`].
///
/// This is the per-file firewall the cross-file
/// [`crate::project::resolved_labels`] resolver consumes for `unreferenced-label`
/// (a label with no reference anywhere in its namespace). Stripping ranges means
/// a prose edit, or a body edit that only shifts a `\ref`'s offset, leaves this
/// `Vec` *equal* — salsa backdates and the project union is not rebuilt. Adding or
/// removing a `\ref` key *does* change it, so the resolution rebuilds (that is
/// exactly the dependency `unreferenced-label` needs). `Eq` for the same firewall
/// reason as [`file_labels`].
#[salsa::tracked(returns(ref))]
pub fn file_refs(db: &dyn IncrementalDb, file: SourceInput) -> Vec<SmolStr> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::FileRefs,
        file: Some(*file.identity(db)),
    });
    document_ref_names(semantic_model(db, file))
}

/// The file's distinct glossary/acronym keys, sorted — a range-free projection
/// of [`semantic_model`], the glossary analog of [`file_labels`].
///
/// The per-file firewall glossary key completion consumes: a prose or `\gls`
/// edit leaves this `Vec` *equal*, so salsa backdates and the completion path's
/// per-member reads stay memoized. Cross-file union needs no dedicated resolver —
/// the namespace is the same include-graph component
/// [`crate::project::resolved_labels`] already computes, so the LSP layer walks
/// `namespace_members` and unions these per-file sets directly.
#[salsa::tracked(returns(ref))]
pub fn file_glossary_keys(db: &dyn IncrementalDb, file: SourceInput) -> Vec<SmolStr> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::FileGlossaryKeys,
        file: Some(*file.identity(db)),
    });
    document_glossary_keys(semantic_model(db, file))
}

/// Whether `file` looks like a document *root* — it carries a `\documentclass`
/// or a `\begin{document}`. The cross-file `undefined-ref` lint only fires
/// inside a namespace that contains a root, so a bare chapter fragment opened
/// alone (whose labels live in the main document) is never flagged.
///
/// A cheap `bool` projection of the parse tree, `Eq` for the same firewall
/// reason as [`file_labels`]: it changes only when a `\documentclass` /
/// `\begin{document}` is added or removed, so ordinary edits backdate.
#[salsa::tracked(returns(ref))]
pub fn file_is_document_root(db: &dyn IncrementalDb, file: SourceInput) -> bool {
    db.record_query(QueryLogEntry {
        kind: QueryKind::FileIsDocumentRoot,
        file: Some(*file.identity(db)),
    });
    // Root discovery also visits bibliography members. They cannot be TeX
    // compilation roots, even when a field contains a literal documentclass.
    // Do not build a second, unrelated TeX tree for a large bibliography.
    file_kind_or_tex(file.path(db)).is_latex() && is_document_root(&parsed_tree_root(db, file))
}

/// A `.sty` file's statically-declared option surface
/// ([`crate::project::package_option_facts`]), or `None` for any other file
/// kind — the per-file firewall the cross-file package-option resolver
/// consumes. `Eq` for the same reason as [`file_labels`]: a body edit that
/// leaves the `\DeclareOption` set and the dynamic-processor signals unchanged
/// backdates, and the project-level model is not rebuilt.
#[salsa::tracked(returns(ref))]
pub fn file_package_option_facts(
    db: &dyn IncrementalDb,
    file: SourceInput,
) -> Option<PackageOptionFacts> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::FilePackageOptionFacts,
        file: Some(*file.identity(db)),
    });
    // The pure extractor returns None for every non-.sty path. Check before
    // demanding its tree/model so a package-options pull does not parse all TeX.
    if file_kind_or_tex(file.path(db)) != crate::source::FileKind::Sty {
        return None;
    }
    package_option_facts(
        file.path(db),
        &parsed_tree_root(db, file),
        semantic_model(db, file),
    )
}

/// Raw findings shared by diagnostics and code actions. Selection is applied by
/// callers afterwards, so changing selected rules does not rerun the traversal.
/// The value contains only owned diagnostics, not Salsa handles.
#[salsa::tracked(returns(ref), unsafe(non_salsa_values))]
pub fn latex_lint_findings(
    db: &dyn IncrementalDb,
    file: SourceInput,
) -> Vec<crate::linter::Diagnostic> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::LatexLintFindings,
        file: Some(*file.identity(db)),
    });
    let root = parsed_tree_root(db, file);
    let model = semantic_model(db, file);
    // Cross-file rules inspect these local sites before consulting resolution.
    // Keep dependency demand aligned with those rules, including declared aliases.
    let labels = (!model.labels().is_empty() || !model.refs().is_empty())
        .then(|| resolved_labels(db, db.project_input(*file.project(db))));
    let citations = (!model.citations().is_empty())
        .then(|| resolved_citations(db, db.project_input(*file.project(db))));
    crate::linter::lint_document(
        file.path(db),
        &root,
        model,
        labels,
        citations,
        Some(resolved_package_options(
            db,
            db.project_input(*file.project(db)),
        )),
    )
}

/// BibTeX's current rules depend only on the local syntax and semantic model.
#[salsa::tracked(returns(ref), unsafe(non_salsa_values))]
pub fn bib_lint_findings(
    db: &dyn IncrementalDb,
    file: SourceInput,
) -> Vec<crate::linter::Diagnostic> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::BibLintFindings,
        file: Some(*file.identity(db)),
    });
    let context = db.project_input(*file.project(db));
    let sources = crate::project::root_views::project_sources(db, context);
    let project = crate::bib::linter::project::ProjectFacts::build(
        file.path(db),
        crate::project::resolved_citations(db, context),
        sources
            .iter()
            .filter(|(path, _)| {
                crate::source::lint_file_kind(path).is_some_and(|kind| kind.is_latex())
            })
            .map(|(path, source)| (path.as_path(), semantic_model(db, *source))),
        sources
            .iter()
            .filter(|(path, _)| {
                crate::source::lint_file_kind(path) == Some(crate::source::FileKind::Bib)
            })
            .map(|(path, source)| (path.as_path(), bib_semantic_model(db, *source))),
    );
    crate::bib::linter::lint_document_with_project(
        file.path(db),
        &parsed_bib_tree_root(db, file),
        bib_semantic_model(db, file),
        Some(&project),
    )
}

/// A `.bib` file's cached parse: the green tree plus parse diagnostics. The bib
/// analog of [`parsed_document`].
///
/// `no_eq, unsafe(non_salsa_values)` for the same reason — `GreenNode` is neither
/// `Eq` nor `salsa::SalsaValue`, so salsa never compares parses and relies on
/// text-input change detection. The same [`SourceInput`] input feeds both this and
/// [`parsed_document`]: queries dispatch on the function, not the path, so a
/// buffer's `.bib`-ness is decided by which query the caller runs, not by the
/// input's synthetic extension.
#[salsa::tracked(returns(ref), no_eq, unsafe(non_salsa_values))]
pub fn parsed_bib_document(db: &dyn IncrementalDb, file: SourceInput) -> ParsedBibDocument {
    db.record_query(QueryLogEntry {
        kind: QueryKind::ParsedBibDocument,
        file: Some(*file.identity(db)),
    });

    let text = file.text(db);
    let state = db.reparse_state(file);
    let staged = state.pending;
    let prev = state.bib_prev;
    let reparsed = prev.as_ref().and_then(|prev| {
        if prev.text == *text {
            return Some(prev.parsed.clone());
        }
        if let [edit] = staged.as_slice() {
            return crate::bib::reparse::reparse(&prev.text, &prev.parsed, edit, text);
        }
        let edit = crate::parser::edit::coalesce_touching_edits(&prev.text, &staged)?;
        crate::bib::reparse::reparse(&prev.text, &prev.parsed, &edit, text)
    });
    let parsed = reparsed.unwrap_or_else(|| crate::bib::parse(text));
    let artifact = Arc::new(PrevBibParse {
        text: text.clone(),
        parsed,
    });
    db.reparse_bib_store(file, artifact.clone(), staged.len(), state.generation);
    artifact
}

/// The `.bib` parse diagnostics for `file` (empty when it parses cleanly).
pub fn bib_parse_diagnostics(db: &dyn IncrementalDb, file: SourceInput) -> &[ParseDiagnosticData] {
    &parsed_bib_document(db, file).parsed.errors
}

/// Materialize the cached `.bib` parse for `file` as a fresh bib `SyntaxNode`.
pub fn parsed_bib_tree_root(db: &dyn IncrementalDb, file: SourceInput) -> BibSyntaxNode {
    BibSyntaxNode::new_root(parsed_bib_document(db, file).parsed.green.clone())
}

/// The per-file bib model (entries, `@string` defs/uses), built on the cached
/// `.bib` parse.
///
/// Like [`semantic_model`] and unlike [`parsed_bib_document`] this is **not**
/// `no_eq`: [`crate::bib::semantic::Model`] is `Eq`, so salsa backdates when an
/// edit leaves the model unchanged.
#[salsa::tracked(returns(ref))]
pub fn bib_semantic_model(db: &dyn IncrementalDb, file: SourceInput) -> BibModel {
    db.record_query(QueryLogEntry {
        kind: QueryKind::BibSemanticModel,
        file: Some(*file.identity(db)),
    });
    BibModel::build(&parsed_bib_tree_root(db, file))
}

/// A `.bib` file's distinct cite keys, sorted — a range-free projection of
/// [`bib_semantic_model`].
///
/// The per-file firewall the cross-file [`crate::project::resolved_citations`]
/// resolver consumes (the bib analog of [`file_labels`]). Stripping ranges means
/// an edit that shifts a `@entry`'s offset, or touches a field but not a key,
/// leaves this `Vec` *equal* — salsa backdates and the project-level union is not
/// rebuilt. Like [`file_labels`] it is **not** `no_eq`: `Vec<SmolStr>` is `Eq`,
/// which is what makes the firewall hold.
#[salsa::tracked(returns(ref))]
pub fn file_cite_names(db: &dyn IncrementalDb, file: SourceInput) -> Vec<SmolStr> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::FileCiteNames,
        file: Some(*file.identity(db)),
    });
    document_cite_names(bib_semantic_model(db, file))
}

/// Cached full search fields; changing a query does not rerender the bibliography.
#[salsa::tracked(returns(ref))]
pub fn bib_search_fields(db: &dyn IncrementalDb, file: SourceInput) -> Vec<(SmolStr, String)> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::BibSearchFields,
        file: Some(*file.identity(db)),
    });
    let root = parsed_bib_tree_root(db, file);
    bib_semantic_model(db, file)
        .entries()
        .iter()
        .map(|entry| {
            (
                entry.key.clone(),
                crate::bib::render::search_text(entry, &root),
            )
        })
        .collect()
}

/// A `.tex` file's citation facts: its bibliography-resource targets
/// (`\bibliography`/`\addbibresource`) and whether it carries a `\nocite{*}`
/// wildcard. The per-file firewall feeding `crate::project::resolved_citations`
/// on the `.tex` side (the document-root flag reuses `file_is_document_root`).
///
/// `Eq` for the same firewall reason as `file_labels`: a prose or `\cite` edit
/// changes neither the resource targets nor the wildcard, so it backdates and the
/// cross-file resolution memo holds. Resolves relative targets against the file's
/// own directory (`path.parent()`), like `include_edges`.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct FileCiteFacts {
    pub bib_targets: Vec<BibTarget>,
    pub manual_keys: Vec<SmolStr>,
    pub nocite_all: bool,
}

#[salsa::tracked(returns(ref))]
pub fn file_cite_facts(db: &dyn IncrementalDb, file: SourceInput) -> FileCiteFacts {
    db.record_query(QueryLogEntry {
        kind: QueryKind::FileCiteFacts,
        file: Some(*file.identity(db)),
    });
    let root = parsed_tree_root(db, file);
    FileCiteFacts {
        bib_targets: collect_bib_resource_targets(&root, None),
        manual_keys: semantic_model(db, file)
            .bibitems()
            .iter()
            .map(|item| item.name.clone())
            .collect(),
        nocite_all: semantic_model(db, file).has_wildcard_nocite(),
    }
}

/// Current definition spans are shared by navigation, references, and rename.
#[salsa::tracked(returns(ref))]
pub fn definition_sites(
    db: &dyn IncrementalDb,
    file: SourceInput,
) -> Vec<crate::semantic::DefSite> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::DefinitionSites,
        file: Some(*file.identity(db)),
    });
    crate::semantic::scan_definition_sites(&parsed_tree_root(db, file))
}

#[salsa::tracked(returns(ref))]
pub fn file_outline(
    db: &dyn IncrementalDb,
    file: SourceInput,
) -> Vec<crate::semantic::OutlineItem> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::Outline,
        file: Some(*file.identity(db)),
    });
    crate::semantic::outline(&parsed_tree_root(db, file))
}

#[salsa::tracked(returns(ref))]
pub fn file_references(
    db: &dyn IncrementalDb,
    file: SourceInput,
) -> Vec<crate::external::links::FileReference> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::FileReferences,
        file: Some(*file.identity(db)),
    });
    let Some(context) = crate::project::root_views::file_context(db, file) else {
        return Vec::new();
    };
    let root = parsed_tree_root(db, file);
    let mut references: Vec<crate::external::links::FileReference> = Vec::new();
    for base in &context.bases {
        for reference in crate::external::links::file_references(
            &root,
            Some(base),
            Some(&context.root_directory),
            &context.graphics,
        ) {
            if !references.contains(&reference) {
                references.push(reference);
            }
        }
    }
    references
}

#[salsa::tracked(returns(ref))]
pub fn name_occurrences(
    db: &dyn IncrementalDb,
    file: SourceInput,
) -> crate::name_refs::NameOccurrences {
    db.record_query(QueryLogEntry {
        kind: QueryKind::NameOccurrences,
        file: Some(*file.identity(db)),
    });
    crate::name_refs::NameOccurrences::build(&parsed_tree_root(db, file))
}
