//! `textDocument/hover` computation. The content is LaTeX-specific: the cursor's
//! command/environment **signature** (from the package-merged signature scope) or
//! its `\cite` key's resolved **`.bib` entry**.
//!
//! Two targets, tried in order (they sit at disjoint offsets, so order is only a
//! tie-break):
//!
//! - **Command / environment signature.** A `\command` control word, or an
//!   environment name in a `\begin{…}`/`\end{…}`, renders a synthesized prototype
//!   plus a facts line (arity, argument kinds, sectioning/float/theorem level,
//!   verbatim/math/list flags, and provenance — built-in, user-defined, or the
//!   defining local package by name, preserved through the scope merge).
//!   Looked up scope-first (the document's own + loaded packages' scanned defs),
//!   then the curated built-in DB, then the bulk CWL tier — mirroring
//!   [`super::build_completion_items`]'s tiering.
//! - **Citation → `.bib` entry.** A `\cite`-family key resolves cross-file against
//!   the project bibliography ([`Analysis::resolve_project`]); the matched
//!   `@entry`'s author/title/year/journal are pulled from the cached bib CST.
//! - **Label preview.** A `\label`/`\ref`-family key renders what it labels
//!   (`semantic::label_context` at the definition site, resolved cross-file like
//!   citations) plus the number the last compile assigned, read from the build's
//!   `.aux` ([`tex_ls_analysis::project::aux`]) — `Figure 3: A chart`, `Section 1.2 (Intro)`.
//!   Degrades to the numberless preview when the project was never compiled.
//!

use std::fmt::Write as _;

use super::*;
use crate::document_link::comma_spans;
use lsp_types::{Contents, Hover, MarkupContent, MarkupKind};
use tex_ls_analysis::bib::ast as bib_ast;
pub use tex_ls_analysis::hover::{Provenance, lookup_command, lookup_environment};
use tex_ls_analysis::hover::{TargetKind, signature_target_at};
use tex_ls_parser::ast::{command_name, nth_group, nth_group_inner};
use tex_ls_parser::semantic::LabelContext;
use tex_ls_parser::semantic::completion::{PackageMeta, package_metadata};
use tex_ls_parser::semantic::pkgmeta::{NeedsFormatDecl, OptionDecl, ProvidesDecl, provides_kind};
use tex_ls_parser::semantic::signature::{ArgKind, CommandSig, EnvironmentSig, OutlineKind};
use tex_ls_parser::syntax::SyntaxKind;

/// Describe a construct using the source and semantic scope in one snapshot.
pub fn compute_hover(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    position: Position,
) -> Option<Hover> {
    let file = snapshot.lookup_file(path)?;
    let idx = snapshot.file_line_index(file, encoding);
    let offset = idx.offset_at(position.line, position.character);
    if file_kind_for(path) == FileKind::Bib {
        let root = snapshot.parsed_bib_tree(file);
        let (range, value) = if let Some((key, range)) =
            crate::bib_strings::target(snapshot.bib_semantic_model(file), offset)
        {
            let value =
                tex_ls_analysis::bib::render::text(&crate::bib_strings::expanded(&root, &key));
            (
                range,
                format!(
                    "`{key}` = {}",
                    tex_ls_analysis::bib::render::markdown_text(&value)
                ),
            )
        } else {
            tex_ls_analysis::bib::render::documentation(&root, offset)?
        };
        return Some(markup_hover(value, range, &idx));
    }
    build_hover(
        snapshot,
        &snapshot.parsed_tree(file),
        snapshot.semantic_model(file),
        snapshot.scope_signatures(file),
        snapshot.file_path(file),
        offset,
        &idx,
    )
}

/// The shared body: try a command/environment signature, then a citation entry,
/// then a label preview.
#[allow(clippy::too_many_arguments)]
fn build_hover(
    snapshot: &Analysis,
    root: &SyntaxNode,
    model: &SemanticModel,
    scope: &SignatureDb,
    lint_path: &Path,
    offset: usize,
    idx: &LineIndex,
) -> Option<Hover> {
    if let Some((value, range)) = declaration_hover(model, root, offset) {
        return Some(markup_hover(value, range, idx));
    }

    if let Some(target) = rename_target_under_cursor(model, offset)
        && let CursorTarget::Citations(names) = &target.target
        && snapshot
            .citation_occurrences(lint_path, names)
            .iter()
            .any(|item| item.definition && file_kind_for(&item.path) != FileKind::Bib)
    {
        return Some(markup_hover(
            crate::source_cards::manual(snapshot, lint_path, &target.placeholder)
                .unwrap_or_else(|| format!("Manual bibliography item `{}`", target.placeholder)),
            target.span,
            idx,
        ));
    }
    if let Some(target) = signature_target_at(root, offset) {
        let value = match target.kind {
            TargetKind::Command => {
                let (sig, provenance) = lookup_command(scope, &target.name)?;
                render_command(&target.name, sig, &provenance)
            }
            TargetKind::Environment => {
                let (sig, provenance) = lookup_environment(scope, &target.name)?;
                render_environment(&target.name, sig, &provenance)
            }
        };
        return Some(markup_hover(value, target.range, idx));
    }

    if let Some(target) = package_target_at(root, offset) {
        let meta = package_metadata(&target.name)?;
        let value = render_package(&target.name, target.is_class, meta);
        return Some(markup_hover(value, target.range, idx));
    }

    if let Some((key, range)) = crate::glossary::target(model, offset)
        && crate::glossary::occurrences(snapshot, lint_path, &key)
            .iter()
            .any(|(_, _, definition)| *definition)
    {
        return Some(markup_hover(
            crate::source_cards::glossary(snapshot, lint_path, &key)
                .unwrap_or_else(|| format!("Glossary/acronym entry `{key}`")),
            range,
            idx,
        ));
    }
    if let Some((name, key_range)) = citation_at(model, offset) {
        let citations = snapshot.resolve_citations();
        let value = render_citation(snapshot, citations, lint_path, &name)?;
        return Some(markup_hover(value, key_range, idx));
    }

    if let Some((name, key_range)) = label_target_at(model, offset) {
        let resolution = snapshot.resolve_labels();
        let value = render_label(snapshot, resolution, lint_path, root, model, &name)?;
        return Some(markup_hover(value, key_range, idx));
    }

    None
}

/// Wrap rendered markdown in a [`Hover`], anchoring its range to `range` for the
/// client's highlight.
fn markup_hover(value: String, range: TextRange, idx: &LineIndex) -> Hover {
    Hover {
        contents: Contents::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(byte_range_to_lsp(
            idx,
            usize::from(range.start()),
            usize::from(range.end()),
        )),
    }
}

// --- Command / environment signature ------------------------------------------

/// The provenance fact line: `command` / `user-defined command` /
/// ``command defined by package `mypkg` `` (same for `environment`).
pub fn provenance_label(provenance: &Provenance, word: &str) -> String {
    match provenance {
        Provenance::Base => word.to_string(),
        Provenance::Document => format!("user-defined {word}"),
        Provenance::Package(pkg) => format!("{word} defined by package `{pkg}`"),
    }
}

/// A command signature, scope-first then built-in then CWL, with the scope hit's
/// provenance (document vs loaded package) preserved for the rendered label.
pub fn arg_slot(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::Brace => "{}",
        ArgKind::Bracket => "[]",
    }
}

// --- Package / class name (CTAN metadata) -------------------------------------

/// A `\usepackage`/`\documentclass` name the cursor sits on: the stem to look up in
/// the CTAN metadata DB, its byte range (for the client highlight), and whether it
/// came from a class loader (for the rendered label).
struct PackageTarget {
    name: String,
    range: TextRange,
    is_class: bool,
}

/// Recognized package/class loaders whose brace `{name}` argument gets a CTAN
/// metadata hover. Mirrors `document_link::classify`'s `usepackage`/`documentclass`
/// arms and `completion::package_arg`.
fn package_loader_is_class(name: &str) -> Option<bool> {
    use tex_ls_parser::semantic::roles::{FileRoleKind, file_role};
    match file_role(name)?.kind {
        FileRoleKind::Package => Some(false),
        FileRoleKind::Class => Some(true),
        _ => None,
    }
}

/// The package/class name token the cursor sits on, if any: a name inside the first
/// brace `{…}` argument of a `\usepackage`/`\documentclass`-family command, resolved
/// to the single comma-separated segment covering the offset (so `\usepackage{a,b|}`
/// hovers `b`). Reuses `document_link::comma_spans` for the per-name spans.
fn package_target_at(root: &SyntaxNode, offset: usize) -> Option<PackageTarget> {
    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    let (left, right) = match root.token_at_offset(at) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => (Some(t.clone()), Some(t)),
        rowan::TokenAtOffset::Between(l, r) => (Some(l), Some(r)),
    };

    for token in [left, right].into_iter().flatten() {
        let Some(group) = token
            .parent_ancestors()
            .find(|n| n.kind() == SyntaxKind::GROUP)
        else {
            continue;
        };
        let Some(command) = group.parent() else {
            continue;
        };
        if command.kind() != SyntaxKind::COMMAND {
            continue;
        }
        let Some(name) = command_name(&command) else {
            continue;
        };
        let Some(is_class) = package_loader_is_class(&name) else {
            continue;
        };
        // Only the first brace group is the `{name}` list (a `[options]` bracket
        // group is not a `GROUP` child, so group 0 is always the names).
        if nth_group(&command, 0).as_ref() != Some(&group) {
            continue;
        }
        let Some((inner_range, inner)) = nth_group_inner(&command, 0) else {
            continue;
        };
        if let Some((seg, range)) = comma_spans(&inner, inner_range)
            .into_iter()
            .find(|(_, r)| r.contains_inclusive(at))
        {
            return Some(PackageTarget {
                name: seg.to_string(),
                range,
                is_class,
            });
        }
    }
    None
}

/// Render a CTAN metadata hover: a bold `name` with a package/class tag, the one-line
/// description when known, and a link row — a texdoc documentation link (LaTeX
/// Workshop's "View documentation", which shells out to `texdoc`; the web-serve
/// equivalent is `texdoc.org`, which resolves by *package name* and returns the
/// documentation PDF) and the CTAN catalogue page when a catalogue id is known.
fn render_package(name: &str, is_class: bool, meta: &PackageMeta) -> String {
    let tag = if is_class { "class" } else { "package" };
    let mut out = format!("**`{name}`** — {tag}");
    if let Some(desc) = meta.desc {
        let _ = write!(out, "\n\n{desc}");
    }
    // texdoc resolves by TeX package name, not the CTAN catalogue id, so the doc
    // link is keyed on `name`; the CTAN link uses the catalogue id.
    let mut links = format!("[Documentation](https://texdoc.org/pkg/{name})");
    if let Some(url) = meta.ctan_url() {
        let _ = write!(links, " · [CTAN]({url})");
    }
    let _ = write!(out, "\n\n{links}");
    out
}

// --- Package authoring declarations (recognize, never execute) -----------------

/// Hover for a package/class authoring command the cursor sits on — the metadata a
/// `.sty`/`.cls` declares about itself (`\ProvidesPackage`, `\NeedsTeXFormat`,
/// `\DeclareOption`, …). The identity/option facts are read from the pre-extracted
/// [`SemanticModel`], matched to the command by its control-word range; the
/// option-*processing* commands (`\ProcessOptions`/`\ExecuteOptions`) get a static
/// note. Tried before the generic signature hover, which would otherwise render these
/// as plain command prototypes. Returns the markdown and the range to highlight.
fn declaration_hover(
    model: &SemanticModel,
    root: &SyntaxNode,
    offset: usize,
) -> Option<(String, TextRange)> {
    let (name, range) = declaration_command_at(root, offset)?;

    if provides_kind(&name).is_some() {
        let decl = model.provides().filter(|d| d.range == range)?;
        return Some((render_provides(decl), range));
    }
    if name == "NeedsTeXFormat" {
        let decl = model.needs_format().filter(|d| d.range == range)?;
        return Some((render_needs_format(decl), range));
    }
    if name == "DeclareOption" {
        let decl = model.options().iter().find(|d| d.range == range)?;
        return Some((render_option(decl), range));
    }
    let note = match name.as_str() {
        "ProcessOptions" => "**Processes package options** — recognized, never executed by tex-ls",
        "ExecuteOptions" => "**Executes default options** — recognized, never executed by tex-ls",
        _ => return None,
    };
    Some((note.to_string(), range))
}

/// The name and control-word range of the `COMMAND` whose control word the cursor sits
/// on. Mirrors [`signature_target_at`]'s command branch, but keeps the raw name (the
/// caller decides whether it is a package-authoring declaration).
fn declaration_command_at(root: &SyntaxNode, offset: usize) -> Option<(String, TextRange)> {
    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    let (left, right) = match root.token_at_offset(at) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => (Some(t.clone()), Some(t)),
        rowan::TokenAtOffset::Between(l, r) => (Some(l), Some(r)),
    };
    for token in [left, right].into_iter().flatten() {
        if token.kind() == SyntaxKind::CONTROL_WORD
            && let Some(parent) = token.parent()
            && parent.kind() == SyntaxKind::COMMAND
        {
            return Some((
                token.text().trim_start_matches('\\').to_string(),
                token.text_range(),
            ));
        }
    }
    None
}

/// Render a `\Provides…` self-identification: the namespace + name, an inline
/// version/date, then the free-text description on its own line. The version's `v`
/// prefix (LaTeX2e's `v1.2`) is dropped so it reads uniformly with the expl3 form.
fn render_provides(decl: &ProvidesDecl) -> String {
    let mut out = format!("**Provides {}** `{}`", decl.kind.noun(), decl.name);
    let version = decl
        .version
        .as_deref()
        .map(|v| v.trim_start_matches(['v', 'V']));
    match (version, decl.date.as_deref()) {
        (Some(v), Some(d)) => {
            let _ = write!(out, " — version {v} ({d})");
        }
        (Some(v), None) => {
            let _ = write!(out, " — version {v}");
        }
        (None, Some(d)) => {
            let _ = write!(out, " — {d}");
        }
        (None, None) => {}
    }
    if let Some(desc) = provides_description(decl) {
        let _ = write!(out, "\n\n{desc}");
    }
    out
}

/// The human description of a `\Provides…`: the raw `info` with the date and version
/// tokens (already surfaced inline) dropped. For the expl3 form `info` is the
/// description group verbatim, so this is a no-op there. `None` when nothing remains.
fn provides_description(decl: &ProvidesDecl) -> Option<String> {
    let info = decl.info.as_deref()?;
    let rest: Vec<&str> = info
        .split_whitespace()
        .filter(|t| Some(*t) != decl.date.as_deref() && Some(*t) != decl.version.as_deref())
        .collect();
    (!rest.is_empty()).then(|| rest.join(" "))
}

/// Render a `\NeedsTeXFormat{format}[date]`.
fn render_needs_format(decl: &NeedsFormatDecl) -> String {
    let mut out = format!("**Requires format** `{}`", decl.format);
    if let Some(date) = decl.date.as_deref() {
        let _ = write!(out, " ({date})");
    }
    out
}

/// Render a `\DeclareOption{name}` or the starred default handler `\DeclareOption*`.
fn render_option(decl: &OptionDecl) -> String {
    match decl.name.as_deref() {
        Some(name) => format!("**Declares option** `{name}`"),
        None => "**Default option handler** (`\\DeclareOption*`)".to_string(),
    }
}

/// A human summary of an argument list: e.g. `2 required, 1 optional`. Empty when the
/// construct takes no arguments.
pub fn arg_summary(args: &[tex_ls_parser::semantic::signature::ArgSpec]) -> Option<String> {
    let req = args.iter().filter(|a| a.required).count();
    let opt = args.len() - req;
    let mut parts = Vec::new();
    if req > 0 {
        parts.push(format!("{req} required"));
    }
    if opt > 0 {
        parts.push(format!("{opt} optional"));
    }
    (!parts.is_empty()).then(|| format!("{} argument{}", parts.join(", "), plural(args.len())))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `\name{}{}` prototype + a `·`-joined facts line.
pub fn render_command(name: &str, sig: &CommandSig, provenance: &Provenance) -> String {
    let mut out = String::new();
    let _ = write!(out, "```latex\n\\{name}");
    for arg in sig.args.iter() {
        out.push_str(arg_slot(arg.kind));
    }
    out.push_str("\n```\n");

    if matches!(provenance, Provenance::Base)
        && let Some(glyph) = tex_ls_parser::semantic::math::command_glyph(name)
    {
        let _ = write!(out, "\nSymbol: {glyph}\n\n");
    }
    let mut facts = vec![provenance_label(provenance, "command")];
    if let Some(level) = sig.sectioning {
        facts.push(format!("sectioning level {level}"));
    }
    if sig.verbatim {
        facts.push("verbatim argument".to_string());
    }
    if let Some(summary) = arg_summary(&sig.args) {
        facts.push(summary);
    }
    out.push_str(&facts.join(" · "));
    if matches!(provenance, Provenance::Base) {
        for (index, doc) in crate::source_cards::argument_docs(name).iter().enumerate() {
            let _ = write!(out, "\n\nArgument {}: {doc}", index + 1);
        }
    }
    out
}

/// `\begin{name} … \end{name}` prototype + a `·`-joined facts line.
pub fn render_environment(name: &str, sig: &EnvironmentSig, provenance: &Provenance) -> String {
    let mut out = String::new();
    let _ = write!(out, "```latex\n\\begin{{{name}}}");
    for arg in sig.args.iter() {
        out.push_str(arg_slot(arg.kind));
    }
    let _ = write!(out, " … \\end{{{name}}}\n```\n");

    let mut facts = vec![provenance_label(provenance, "environment")];
    match sig.outline {
        Some(OutlineKind::Float) => facts.push("float".to_string()),
        Some(OutlineKind::Theorem) => facts.push("theorem-like".to_string()),
        Some(OutlineKind::Frame) => facts.push("Beamer frame".to_string()),
        None => {}
    }
    if sig.math {
        facts.push("math".to_string());
    }
    if sig.align {
        facts.push("alignment".to_string());
    }
    if sig.list {
        facts.push("list".to_string());
    }
    if sig.verbatim_body {
        facts.push("verbatim body".to_string());
    } else if sig.code {
        facts.push("code body".to_string());
    }
    if let Some(summary) = arg_summary(&sig.args) {
        facts.push(summary);
    }
    out.push_str(&facts.join(" · "));
    out
}

// --- Citation → bib entry -----------------------------------------------------

/// The cite key whose *key* range covers `offset`, with that range. Uses `key_range`
/// (not the whole-command range), so a multi-key `\cite{a,b}` resolves the one key
/// under the cursor — the same per-key precision rename relies on.
fn citation_at(model: &SemanticModel, offset: usize) -> Option<(SmolStr, TextRange)> {
    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    model
        .citations()
        .iter()
        .find(|c| c.key_range.contains_inclusive(at))
        .map(|c| (c.name.clone(), c.key_range))
}

/// Render the `@entry` a cite key resolves to: its type + key, then a few canonical
/// fields. `None` when the key resolves to no entry (no useful card to show). Mirrors
/// [`super::resolve_citation_locations`]'s namespace walk.
fn render_citation(
    snapshot: &Analysis,
    _citations: &ResolvedCitations,
    lint_path: &Path,
    name: &SmolStr,
) -> Option<String> {
    let (file, entry) = snapshot.citation_definition(lint_path, name)?;
    let node = bib_ast::entry_at_range(&snapshot.parsed_bib_tree(file), entry.range)?;
    Some(render_entry(&entry.entry_type, &entry.key, &node))
}

pub use tex_ls_analysis::bib::render::entry as render_entry;

// --- Label preview --------------------------------------------------------------

/// The label key whose *key* range covers `offset` — a `\label` definition key or
/// a `\ref`-family use key. Per-key like [`citation_at`], so a multi-key
/// `\cref{a,b}` resolves the one key under the cursor.
fn label_target_at(model: &SemanticModel, offset: usize) -> Option<(SmolStr, TextRange)> {
    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    model
        .labels()
        .iter()
        .find(|l| l.key_range.contains_inclusive(at))
        .map(|l| (l.name.clone(), l.key_range))
        .or_else(|| {
            model
                .refs()
                .iter()
                .find(|r| r.key_range.contains_inclusive(at))
                .map(|r| (r.name.clone(), r.key_range))
        })
}

/// Render the label preview: what the key labels ([`Analysis::label_context`] at its
/// definition site, resolved cross-file like go-to-definition) plus the number
/// the last compile assigned (from the `.aux`, when one exists). `None` when
/// there is neither a classifiable definition site nor a compiled number.
fn render_label(
    snapshot: &Analysis,
    resolution: &ResolvedLabels,
    lint_path: &Path,
    _root: &SyntaxNode,
    _model: &SemanticModel,
    name: &SmolStr,
) -> Option<String> {
    let context = snapshot.label_context(lint_path, name);

    let number = label_number(snapshot, resolution, lint_path, name);
    render_label_markdown(context.as_ref(), number.as_deref())
}

/// The number the last compile assigned to `name`, read from the namespace's
/// `.aux` files ([`super::document_aux`]).
pub(crate) fn label_number(
    snapshot: &Analysis,
    resolution: &ResolvedLabels,
    lint_path: &Path,
    name: &SmolStr,
) -> Option<String> {
    super::document_aux(snapshot, resolution, lint_path)?
        .labels
        .get(name.as_str())
        .cloned()
}

/// The preview line, texlab-style: `Section 1.2 (Intro)`, `Figure 3: A chart`,
/// `Theorem 4 (Euler)`, `Equation (1.5)`, `Item 2`. Number and context each
/// degrade independently; with neither there is nothing to say.
pub(crate) fn render_label_markdown(
    context: Option<&LabelContext>,
    number: Option<&str>,
) -> Option<String> {
    let mut out = String::new();
    match context {
        Some(LabelContext::Section { title }) => {
            out.push_str("Section");
            if let Some(n) = number {
                let _ = write!(out, " {n}");
            }
            if !title.is_empty() {
                let _ = write!(out, " ({title})");
            }
        }
        Some(LabelContext::Float { env, caption }) => {
            out.push_str(&capitalize(env));
            if let Some(n) = number {
                let _ = write!(out, " {n}");
            }
            if let Some(c) = caption {
                let _ = write!(out, ": {c}");
            }
        }
        Some(LabelContext::Theorem { env, description }) => {
            out.push_str(&capitalize(env));
            if let Some(n) = number {
                let _ = write!(out, " {n}");
            }
            if let Some(d) = description {
                let _ = write!(out, " ({d})");
            }
        }
        Some(LabelContext::Equation) => {
            out.push_str("Equation");
            if let Some(n) = number {
                let _ = write!(out, " ({n})");
            }
        }
        Some(LabelContext::Item) => {
            out.push_str("Item");
            if let Some(n) = number {
                let _ = write!(out, " {n}");
            }
        }
        // Unclassifiable definition (or none found): the compiled number alone
        // still tells the reader what the reference resolves to.
        None => {
            let n = number?;
            let _ = write!(out, "Label {n}");
        }
    }
    Some(out)
}

/// Uppercase the first character (`figure` → `Figure`) for the preview's kind word.
fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    /// Render hover at the first byte of `needle` in `src`, returning its markdown.
    fn hover_md(src: &str, needle: &str) -> Option<String> {
        let path = Path::new(fixture_path!("/p/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.to_string());
        let offset = src.find(needle).expect("needle present");
        markdown_at(&mut db, path, src, offset)
    }

    /// Render the hover markdown at `offset` against a database snapshot.
    fn markdown_at(
        db: &mut IncrementalDatabase,
        path: &Path,
        src: &str,
        offset: usize,
    ) -> Option<String> {
        use tex_ls_analysis::external::*;
        let artifacts = db
            .tracked_files()
            .into_iter()
            .filter_map(|(path, _)| {
                let path = path.with_extension("aux");
                let text = std::fs::read_to_string(&path).ok()?;
                Some((
                    path.clone(),
                    Observation::Present(CompilerArtifact {
                        identity: path.to_string_lossy().into_owned(),
                        content_fingerprint: 0,
                        built_from: None,
                        messages: Vec::new(),
                        recorder: None,
                        parsed: tex_ls_analysis::project::aux::parse_aux(&text),
                    }),
                ))
            })
            .collect();
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
            .unwrap();
        db.apply_external_inputs(token, ExternalInputs::Compiler(artifacts))
            .unwrap();
        let snapshot = db.snapshot();
        let position = byte_to_position(src, offset);
        let hover = compute_hover(&snapshot, path, PositionEncoding::Utf16, position)?;
        match hover.contents {
            Contents::MarkupContent(m) => Some(m.value),
            other => panic!("expected markup, got {other:?}"),
        }
    }

    fn byte_to_position(src: &str, offset: usize) -> Position {
        let idx = LineIndex::new(src);
        let (line, character) = idx.position(offset);
        Position { line, character }
    }

    #[test]
    fn command_signature_shows_sectioning_level() {
        let md = hover_md("\\section{Intro}\n", "section").expect("hover for \\section");
        assert!(md.contains("\\section"), "prototype: {md}");
        assert!(md.contains("sectioning level"), "facts: {md}");
        assert!(md.contains("command"), "kind: {md}");
    }

    #[test]
    fn environment_signature_shows_math_flag() {
        let src = "\\begin{align}\nx &= y\n\\end{align}\n";
        let md = hover_md(src, "align").expect("hover for align");
        assert!(md.contains("\\begin{align}"), "prototype: {md}");
        assert!(md.contains("math"), "facts: {md}");
    }

    #[test]
    fn user_defined_command_is_marked() {
        let src = "\\newcommand{\\foo}[1]{#1}\n\\foo{bar}\n";
        // Hover the *use* site, not the definition.
        let offset = src.rfind("foo").expect("use site");
        let path = Path::new(fixture_path!("/p/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.to_string());
        let md = markdown_at(&mut db, path, src, offset).expect("hover for \\foo");
        assert!(md.contains("user-defined command"), "provenance: {md}");
        assert!(md.contains("1 required argument"), "arity: {md}");
    }

    #[test]
    fn package_defined_command_names_source_package() {
        let src = "\\usepackage{mypkg}\n\\myfoo{a}\n";
        let mut db = IncrementalDatabase::default();
        db.upsert_file(Path::new(fixture_path!("/p/main.tex")), src.to_string());
        db.upsert_file(
            Path::new(fixture_path!("/p/mypkg.sty")),
            "\\newcommand{\\myfoo}[1]{#1}\n".to_string(),
        );
        let offset = src.rfind("myfoo").expect("use site");
        let md = markdown_at(
            &mut db,
            Path::new(fixture_path!("/p/main.tex")),
            src,
            offset,
        )
        .expect("hover for \\myfoo");
        assert!(
            md.contains("command defined by package `mypkg`"),
            "provenance: {md}"
        );
        assert!(!md.contains("user-defined"), "provenance: {md}");
    }

    #[test]
    fn package_defined_environment_names_source_package() {
        let src = "\\usepackage{mypkg}\n\\begin{myenv}\nx\n\\end{myenv}\n";
        let mut db = IncrementalDatabase::default();
        db.upsert_file(Path::new(fixture_path!("/p/main.tex")), src.to_string());
        db.upsert_file(
            Path::new(fixture_path!("/p/mypkg.sty")),
            "\\newenvironment{myenv}{}{}\n".to_string(),
        );
        let offset = src.find("myenv").expect("begin site");
        let md = markdown_at(
            &mut db,
            Path::new(fixture_path!("/p/main.tex")),
            src,
            offset,
        )
        .expect("hover for myenv");
        assert!(
            md.contains("environment defined by package `mypkg`"),
            "provenance: {md}"
        );
    }

    #[test]
    fn document_redefinition_over_package_is_user_defined() {
        // The document's own \renewcommand shadows the package definition, so the
        // hover reads "user-defined" again (the merge clears the package origin).
        let src = "\\usepackage{mypkg}\n\\renewcommand{\\myfoo}[2]{#1#2}\n\\myfoo{a}{b}\n";
        let mut db = IncrementalDatabase::default();
        db.upsert_file(Path::new(fixture_path!("/p/main.tex")), src.to_string());
        db.upsert_file(
            Path::new(fixture_path!("/p/mypkg.sty")),
            "\\newcommand{\\myfoo}[1]{#1}\n".to_string(),
        );
        let offset = src.rfind("myfoo").expect("use site");
        let md = markdown_at(
            &mut db,
            Path::new(fixture_path!("/p/main.tex")),
            src,
            offset,
        )
        .expect("hover for \\myfoo");
        assert!(md.contains("user-defined command"), "provenance: {md}");
        assert!(!md.contains("defined by package"), "provenance: {md}");
        assert!(md.contains("2 required arguments"), "arity: {md}");
    }

    #[test]
    fn provides_package_hover_shows_identity() {
        let src = "\\ProvidesPackage{mypkg}[2024/01/01 v1.2 My package]\n";
        let md = hover_md(src, "ProvidesPackage").expect("hover for \\ProvidesPackage");
        assert!(md.contains("Provides package"), "kind: {md}");
        assert!(md.contains("`mypkg`"), "name: {md}");
        assert!(md.contains("version 1.2"), "version (v stripped): {md}");
        assert!(md.contains("2024/01/01"), "date: {md}");
        assert!(md.contains("My package"), "description: {md}");
    }

    #[test]
    fn provides_expl_package_hover() {
        let src = "\\ProvidesExplPackage{mypkg}{2024/01/01}{1.2}{My package}\n";
        let md = hover_md(src, "ProvidesExplPackage").expect("hover for expl3 provides");
        assert!(md.contains("Provides package"), "kind: {md}");
        assert!(md.contains("version 1.2"), "version: {md}");
        assert!(md.contains("My package"), "description: {md}");
    }

    #[test]
    fn needs_tex_format_hover() {
        let src = "\\NeedsTeXFormat{LaTeX2e}[2020/10/01]\n";
        let md = hover_md(src, "NeedsTeXFormat").expect("hover for \\NeedsTeXFormat");
        assert!(md.contains("Requires format"), "label: {md}");
        assert!(md.contains("`LaTeX2e`"), "format: {md}");
        assert!(md.contains("2020/10/01"), "date: {md}");
    }

    #[test]
    fn declare_option_hover_named_and_star() {
        let named = "\\DeclareOption{draft}{\\@drafttrue}\n";
        let md = hover_md(named, "DeclareOption").expect("hover for \\DeclareOption");
        assert!(md.contains("Declares option"), "label: {md}");
        assert!(md.contains("`draft`"), "option name: {md}");

        let star = "\\DeclareOption*{\\PackageWarning{p}{unknown}}\n";
        let md = hover_md(star, "DeclareOption").expect("hover for \\DeclareOption*");
        assert!(md.contains("Default option handler"), "star form: {md}");
    }

    #[test]
    fn process_options_hover_is_static_note() {
        let src = "\\ProcessOptions\\relax\n";
        let md = hover_md(src, "ProcessOptions").expect("hover for \\ProcessOptions");
        assert!(md.contains("Processes package options"), "note: {md}");
        assert!(md.contains("never executed"), "hermetic note: {md}");
    }

    #[test]
    fn citation_resolves_to_bib_entry() {
        let tex = "\\addbibresource{refs.bib}\n\\cite{knuth1984}\n";
        let bib = "@book{knuth1984,\n  author = {Knuth, Donald E.},\n  title = {The TeXbook},\n  year = {1984},\n}\n";
        let tex_path = Path::new(fixture_path!("/p/main.tex"));
        let bib_path = Path::new(fixture_path!("/p/refs.bib"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(tex_path, tex.to_string());
        db.upsert_file(bib_path, bib.to_string());

        let offset = tex.find("knuth1984").expect("cite key");
        let md = markdown_at(&mut db, tex_path, tex, offset).expect("hover for \\cite key");
        assert!(md.contains("@book"), "type: {md}");
        assert!(md.contains("knuth1984"), "key: {md}");
        assert!(md.contains("The TeXbook"), "title: {md}");
        assert!(md.contains("Knuth"), "author: {md}");
    }

    #[test]
    fn no_hover_on_plain_prose() {
        assert!(hover_md("Just some words here.\n", "words").is_none());
    }

    #[test]
    fn label_ref_without_aux_shows_kind_and_context() {
        let src = "\\section{Intro}\n\\label{sec:a}\nSee \\ref{sec:a}.\n";
        let offset = src.rfind("sec:a").expect("ref key");
        let path = Path::new(fixture_path!("/p/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.to_string());
        let md = markdown_at(&mut db, path, src, offset).expect("hover for \\ref key");
        assert_eq!(md, "Section (Intro)");
    }

    #[test]
    fn label_definition_site_hovers_too() {
        let src = "\\begin{figure}\n\\caption{A chart}\n\\label{fig:x}\n\\end{figure}\n";
        let offset = src.find("fig:x").expect("label key");
        let path = Path::new(fixture_path!("/p/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.to_string());
        let md = markdown_at(&mut db, path, src, offset).expect("hover for \\label key");
        assert_eq!(md, "Figure: A chart");
    }

    #[test]
    fn undefined_ref_without_aux_has_no_hover() {
        let src = "See \\ref{nowhere}.\n";
        let offset = src.find("nowhere").expect("ref key");
        let path = Path::new(fixture_path!("/p/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, src.to_string());
        assert!(markdown_at(&mut db, path, src, offset).is_none());
    }

    #[test]
    fn label_ref_with_aux_shows_number() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.tex");
        let src = "\\documentclass{article}\n\\begin{document}\n\\section{Intro}\n\\label{sec:a}\nSee \\ref{sec:a}.\n\\begin{equation}\nx\\label{eq:x}\n\\end{equation}\n\\eqref{eq:x}\n\\end{document}\n";
        std::fs::write(&path, src).unwrap();
        std::fs::write(
            dir.path().join("main.aux"),
            "\\newlabel{sec:a}{{1.2}{1}{Intro}{section.1.2}{}}\n\\newlabel{eq:x}{{3}{1}}\n",
        )
        .unwrap();
        let mut db = IncrementalDatabase::default();
        db.upsert_file(&path, src.to_string());

        let offset = src.rfind("sec:a").expect("ref key");
        let md = markdown_at(&mut db, &path, src, offset).expect("hover for \\ref key");
        assert_eq!(md, "Section 1.2 (Intro)");

        let offset = src.rfind("eq:x").expect("eqref key");
        let md = markdown_at(&mut db, &path, src, offset).expect("hover for \\eqref key");
        assert_eq!(md, "Equation (3)");
    }

    #[test]
    fn cross_file_label_context_and_number_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        let part = dir.path().join("part.tex");
        let main_src = "\\documentclass{article}\n\\begin{document}\n\\input{part}\nSee \\ref{thm:euler}.\n\\end{document}\n";
        let part_src = "\\begin{theorem}[Euler]\nx \\label{thm:euler}\n\\end{theorem}\n";
        std::fs::write(&main, main_src).unwrap();
        std::fs::write(&part, part_src).unwrap();
        std::fs::write(
            dir.path().join("main.aux"),
            "\\newlabel{thm:euler}{{4}{1}}\n",
        )
        .unwrap();
        let mut db = IncrementalDatabase::default();
        db.upsert_file(&main, main_src.to_string());
        db.upsert_file(&part, part_src.to_string());

        let offset = main_src.rfind("thm:euler").expect("ref key");
        let md = markdown_at(&mut db, &main, main_src, offset).expect("hover for \\ref key");
        assert_eq!(md, "Theorem 4 (Euler)");
    }

    #[test]
    fn package_name_hover_shows_ctan_metadata() {
        let md = hover_md("\\usepackage{amsmath}\n", "amsmath").expect("hover for amsmath");
        assert!(md.contains("package"), "kind: {md}");
        assert!(md.contains("AMS mathematical facilities"), "desc: {md}");
        assert!(
            md.contains("https://ctan.org/pkg/latex-amsmath"),
            "ctan link: {md}"
        );
    }

    #[test]
    fn package_hover_links_documentation_via_texdoc() {
        // texdoc resolves by TeX package name (`amsmath`), not the CTAN catalogue id
        // (`latex-amsmath`), so the doc link is keyed on the name the user wrote.
        let md = hover_md("\\usepackage{amsmath}\n", "amsmath").expect("hover for amsmath");
        assert!(
            md.contains("[Documentation](https://texdoc.org/pkg/amsmath)"),
            "texdoc link: {md}"
        );
    }

    #[test]
    fn documentclass_name_hover_marks_class() {
        let md = hover_md("\\documentclass{article}\n", "article").expect("hover for article");
        assert!(md.contains("class"), "kind: {md}");
        assert!(md.contains("https://ctan.org/pkg/"), "ctan link: {md}");
    }

    #[test]
    fn package_hover_picks_the_comma_segment_under_cursor() {
        // The needle resolves to the second name; its hover must be booktabs', not amsmath's.
        let md =
            hover_md("\\usepackage{amsmath, booktabs}\n", "booktabs").expect("hover for booktabs");
        assert!(md.contains("Publication quality tables"), "desc: {md}");
    }

    #[test]
    fn no_package_hover_on_the_command_word() {
        // Hovering the `\usepackage` control word is a signature/none case, not the
        // CTAN metadata hover (which only fires on the argument name).
        let md = hover_md("\\usepackage{amsmath}\n", "usepackage");
        let is_ctan = md.as_deref().is_some_and(|m| m.contains("ctan.org"));
        assert!(!is_ctan, "command word should not show CTAN hover: {md:?}");
    }
}
