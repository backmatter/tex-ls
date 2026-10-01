//! Editor documentation only. These descriptions never affect parsing or arity.
//! Reference manuals: <https://www.latex-project.org/help/documentation/> and
//! <https://tug.ctan.org/systems/doc/etex/etex_ref.html>. Wording is original.
use super::*;
use tex_ls_analysis::name_refs::{NameKind, NameTarget};

const LATEX: &str = "https://www.latex-project.org/help/documentation/";
const ETEX: &str = "https://tug.ctan.org/systems/doc/etex/etex_ref.html";
const TEX: &str = "https://www.tug.org/utilities/plain/cseq.html";

pub struct Entry {
    pub summary: &'static str,
    pub manual: &'static str,
    pub primitive: bool,
}

pub fn entry(name: &str, environment: bool) -> Option<Entry> {
    let summary = if environment {
        match name.trim_end_matches('*') {
            "document" => "Contains the document body, after the preamble.",
            "itemize" => "A bulleted list. Start each entry with \\item.",
            "enumerate" => "A numbered list. Start each entry with \\item.",
            "description" => {
                "A list with descriptive labels supplied as optional arguments to \\item."
            }
            "equation" => "Displays a single equation; the starred form omits its number.",
            "align" => {
                "Aligns equations at & markers. Separate rows with \\\\; the starred form omits numbers."
            }
            "gather" => "Displays centered equations on separate rows.",
            "figure" | "table" => {
                "A floating container. Placement options express preferences, not a fixed position."
            }
            "tabular" => {
                "Typesets rows and columns using a column specification, & separators, and \\\\ row endings."
            }
            "minipage" => "Typesets a separate text block with a specified width.",
            "verbatim" => "Prints the body literally, preserving spaces and line breaks.",
            "frame" => {
                "A Beamer presentation frame, which may produce several slides through overlays."
            }
            "center" => "Centers the enclosed material and adds vertical space around it.",
            "quote" | "quotation" => "Sets off a quotation with indented margins.",
            _ => return None,
        }
    } else {
        match name {
            "documentclass" => "Selects the document class and its options.",
            "usepackage" | "RequirePackage" => "Loads packages with the supplied options.",
            "begin" => "Starts the named environment.",
            "end" => "Ends the named environment.",
            "item" => "Starts a list entry; an optional label overrides the default marker.",
            "title" | "author" | "date" => {
                "Sets document title metadata used by \\maketitle or a class-specific title page."
            }
            "maketitle" | "titlepage" => {
                "Typesets the title information according to the document class."
            }
            "part" | "chapter" | "section" | "subsection" | "subsubsection" | "paragraph"
            | "subparagraph" => {
                "Creates a section heading. The starred form suppresses numbering; an optional short title is used where supported."
            }
            "label" => "Records a cross-reference key using the current reference value.",
            "ref" => {
                "Prints the reference value associated with a label. Compilation resolves the value."
            }
            "pageref" => "Prints the page number associated with a label.",
            "cite" => {
                "Cites bibliography entries by their keys. Appearance depends on the bibliography setup."
            }
            "textbf" => "Typesets its argument in boldface.",
            "textit" => "Typesets its argument in an italic font.",
            "texttt" => "Typesets its argument in a typewriter font.",
            "emph" => "Emphasizes text, with the result depending on the surrounding font.",
            "frac" | "dfrac" | "tfrac" => "Typesets a numerator over a denominator as a fraction.",
            "sqrt" => "Typesets a root; an optional index changes the square root to another root.",
            "includegraphics" => {
                "Includes an image, with optional sizing, cropping, and transformation settings."
            }
            "input" => "Reads another source file at this point in the document.",
            "include" => {
                "Includes a document part with page boundaries and a separate auxiliary file."
            }
            "InputIfFileExists" => {
                "Checks for a source file; if found, runs the success code and inputs it, otherwise runs the failure code."
            }
            "href" => "Creates a hyperlink with explicit display text.",
            "url" => "Displays a URL with suitable line-breaking behavior.",
            "footnote" => "Adds a footnote containing the supplied text.",
            "caption" => {
                "Creates a caption and normally advances the associated figure or table counter."
            }
            "newcommand" | "renewcommand" | "providecommand" | "DeclareRobustCommand" => {
                "Declares a LaTeX command with an argument count and an optional first-argument default. The declaration form controls replacement of existing definitions."
            }
            "NewDocumentCommand"
            | "RenewDocumentCommand"
            | "ProvideDocumentCommand"
            | "DeclareDocumentCommand"
            | "NewExpandableDocumentCommand"
            | "RenewExpandableDocumentCommand"
            | "ProvideExpandableDocumentCommand"
            | "DeclareExpandableDocumentCommand" => {
                "Declares a document command using an argument specification such as m for mandatory or O{default} for optional input."
            }
            "NewCommandCopy" | "RenewCommandCopy" | "DeclareCommandCopy" => {
                "Copies an existing command's current definition using LaTeX's command-copy interface."
            }
            "newenvironment" | "renewenvironment" => {
                "Declares an environment with begin and end code."
            }
            "newtheorem" => {
                "Declares a theorem-like environment with a heading and optional counter relationships."
            }
            "newif" => {
                "Declares a conditional and its true/false setters, for example \\ifdraft, \\drafttrue, and \\draftfalse."
            }
            "[" | "]" => "Starts or ends an unnumbered display-math formula.",
            "(" | ")" => "Starts or ends an inline-math formula.",
            "verb" => {
                "Prints literal text between matching delimiters. The starred form displays spaces."
            }
            "%" | "$" | "#" | "&" | "_" | "{" | "}" => {
                "Prints the escaped character instead of giving it its usual TeX syntax role."
            }
            "\\" => {
                "Ends a line or alignment row in contexts that support it; an optional argument adds vertical space."
            }
            _ => return primitive(name),
        }
    };
    Some(Entry {
        summary,
        manual: match (environment, name.trim_end_matches('*')) {
            (true, "align" | "gather") | (false, "dfrac" | "tfrac") => {
                "https://texdoc.org/pkg/amsmath"
            }
            (true, "frame") => "https://texdoc.org/pkg/beamer",
            (false, "includegraphics") => "https://texdoc.org/pkg/graphicx",
            (false, "href") => "https://texdoc.org/pkg/hyperref",
            (false, "url") => "https://texdoc.org/pkg/url",
            _ => LATEX,
        },
        primitive: false,
    })
}

fn primitive(name: &str) -> Option<Entry> {
    let (summary, manual) = match name {
        "relax" => (
            "Performs no operation; it can also terminate scanning of a number or dimension.",
            TEX,
        ),
        "def" | "gdef" => (
            "Defines a macro using parameter text and replacement text. The gdef form makes the definition global.",
            TEX,
        ),
        "edef" | "xdef" => (
            "Defines a macro after expanding its replacement text. The xdef form makes the definition global.",
            TEX,
        ),
        "let" => (
            "Assigns a control sequence the current meaning of another token.",
            TEX,
        ),
        "futurelet" => (
            "Copies the meaning of a lookahead token without consuming that token for subsequent processing.",
            TEX,
        ),
        "expandafter" => (
            "Expands the second following token before processing the first.",
            TEX,
        ),
        "noexpand" => (
            "Suppresses expansion of the next token for the current expansion step.",
            TEX,
        ),
        "csname" => (
            "Builds a control-sequence name from expanded characters up to endcsname.",
            TEX,
        ),
        "endcsname" => ("Terminates a control-sequence name begun by csname.", TEX),
        "string" => (
            "Converts the next token to character tokens without expanding it.",
            TEX,
        ),
        "meaning" => (
            "Produces character tokens describing the meaning of the next token.",
            TEX,
        ),
        "the" => ("Expands a register or internal quantity to its value.", TEX),
        "number" | "romannumeral" => (
            "Converts an integer to decimal digits or lowercase Roman numerals.",
            TEX,
        ),
        "if" => ("Compares character codes after expansion.", TEX),
        "ifx" => (
            "Compares the meanings of two tokens without expanding them.",
            TEX,
        ),
        "ifnum" | "ifdim" => ("Compares two numbers or dimensions using <, =, or >.", TEX),
        "iftrue" | "iffalse" => (
            "Starts a conditional whose test is always true or always false.",
            TEX,
        ),
        "else" => (
            "Starts the alternative branch of the current conditional.",
            TEX,
        ),
        "fi" => ("Ends the current conditional.", TEX),
        "begingroup" | "endgroup" => (
            "Begins or ends a group that limits the scope of local assignments.",
            TEX,
        ),
        "global" => ("Makes the following assignment global.", TEX),
        "long" => (
            "Allows a macro's arguments to contain paragraph tokens.",
            TEX,
        ),
        "outer" => (
            "Restricts a macro's use in contexts where TeX is scanning another construct.",
            TEX,
        ),
        "catcode" => (
            "Reads or assigns the category code of a character, which controls subsequent tokenization.",
            TEX,
        ),
        "count" | "dimen" | "skip" | "muskip" | "toks" => (
            "Accesses a numbered integer, dimension, glue, math-glue, or token-list register respectively.",
            TEX,
        ),
        "advance" | "multiply" | "divide" => {
            ("Performs arithmetic on a register using a by operand.", TEX)
        }
        "hbox" | "vbox" | "vtop" => (
            "Constructs a horizontal or vertical box from its contents.",
            TEX,
        ),
        "setbox" => ("Assigns a box to a numbered box register.", TEX),
        "par" => ("Ends the current paragraph.", TEX),
        "kern" => ("Inserts a fixed amount of space.", TEX),
        "penalty" => (
            "Adds a numeric preference or prohibition for a break at this point.",
            TEX,
        ),
        "show" | "showthe" | "showbox" => (
            "Reports a token meaning, internal value, or box contents to TeX's diagnostic output.",
            TEX,
        ),
        "write" => (
            "Writes expanded text to an output stream, normally when the containing page is shipped out.",
            TEX,
        ),
        "immediate" => (
            "Makes a following openout, write, or closeout operation happen immediately.",
            TEX,
        ),
        "numexpr" | "dimexpr" | "glueexpr" | "muexpr" => (
            "Evaluates an integer, dimension, glue, or math-glue expression; a relax token can terminate the expression.",
            "https://texdoc.org/pkg/etex",
        ),
        "hskip" | "vskip" => (
            "Inserts horizontal or vertical glue, which may stretch or shrink.",
            TEX,
        ),
        "hrule" | "vrule" => (
            "Constructs a horizontal or vertical rule, with optional width, height, and depth.",
            TEX,
        ),
        "ifcase" => (
            "Selects a numbered conditional branch separated by or tokens, with an optional else fallback.",
            TEX,
        ),
        "or" => ("Separates branches of an ifcase conditional.", TEX),
        "ifdefined" => (
            "Tests whether the next token has a definition, without expanding it.",
            ETEX,
        ),
        "ifcsname" => (
            "Tests whether a constructed control-sequence name is defined, without creating it.",
            ETEX,
        ),
        "detokenize" => ("Converts a token list to character tokens.", ETEX),
        "unexpanded" => (
            "Preserves a token list through the current expansion-only operation.",
            ETEX,
        ),
        "protected" => (
            "Marks a macro so expansion-only operations leave it unexpanded.",
            ETEX,
        ),
        "scantokens" => (
            "Converts tokens to text and reads that text again using current category codes.",
            ETEX,
        ),
        "unless" => ("Reverses the following conditional test.", ETEX),
        _ => return None,
    };
    Some(Entry {
        summary,
        manual,
        primitive: true,
    })
}

/// Render source text as code, never as executable Markdown supplied by a file.
fn literal(text: &str) -> String {
    crate::source_cards::excerpt(text)
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn source_details(snapshot: &Analysis, origin: &Path, name: &str, environment: bool) -> String {
    let target = NameTarget {
        kind: if environment {
            NameKind::Environment
        } else {
            NameKind::Command
        },
        name: name.into(),
        span: TextRange::empty(TextSize::new(0)),
    };
    let sites = snapshot.navigation_definitions(origin, &target);
    let mut out = String::new();
    for (file, range) in sites.iter().take(3) {
        let path = snapshot.file_path(*file);
        let Some(uri) = path_to_uri(path) else {
            continue;
        };
        let text = snapshot.file_text(*file);
        let start = usize::from(range.start());
        let line = text[..start].bytes().filter(|b| *b == b'\n').count() + 1;
        let filename = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("source");
        let label = tex_ls_analysis::bib::render::markdown_text(filename);
        out.push_str(&format!(
            "\n\n[Definition in {label}, line {line}]({uri}#L{line})"
        ));
        if matches!(filename, "latex.ltx" | "latex-dev.ltx") {
            if let Some(doc) = entry(name, environment).filter(|doc| !doc.primitive) {
                out.push_str(&format!("\n\n{}", doc.summary));
            }
            out.push_str("\n\n[LaTeX kernel documentation](https://www.latex-project.org/help/documentation/source2e.pdf)");
        } else if let Some(package) = snapshot.semantic_model(*file).provides()
            && (tex_ls_parser::semantic::completion::package_metadata(&package.name).is_some()
                || snapshot
                    .texmf()
                    .by_name
                    .get(filename)
                    .is_some_and(|installed| installed == path))
        {
            out.push_str(&manual_link(&package.name));
        }

        if sites.len() != 1 {
            continue;
        }
        let site = snapshot
            .definition_sites(*file)
            .iter()
            .find(|site| site.name_range == *range && site.name == name);
        // A declaration name may be on a later line than its command head.
        // Leading comment trivia may also belong to the declaration's node.
        let declaration_start = site.map_or(start, |site| {
            let root = snapshot.parsed_tree(*file);
            let mut token = root.token_at_offset(site.range.start()).right_biased();
            while let Some(current) = token {
                if !tex_ls_parser::syntax::is_trivia(current.kind()) {
                    return usize::from(current.text_range().start());
                }
                token = current.next_token();
            }
            usize::from(site.range.start())
        });
        let line_start = text[..declaration_start].rfind('\n').map_or(0, |at| at + 1);
        let mut comments: Vec<_> = text[..line_start]
            .lines()
            .rev()
            .take(16)
            .map(str::trim)
            .take_while(|line| line.starts_with('%'))
            .map(|line| line.trim_start_matches('%').trim_start())
            .collect();
        comments.reverse();
        if let Some(doc) = snapshot.doc_associations(*file).iter().find(|doc| {
            doc.name.trim_start_matches('\\') == name
                && doc.code.iter().any(|code| code.contains_range(*range))
        }) {
            let end = doc.code.iter().map(|range| range.start()).min().unwrap();
            let body = &text[TextRange::new(doc.name_range.end(), end)];
            out.push_str("\n\nSource documentation:\n\n");
            out.push_str(&literal(body));
            comments.clear();
        }
        if !comments.is_empty() {
            out.push_str("\n\nSource comments:\n\n");
            out.push_str(&literal(&comments.join("\n")));
        }
        if let Some(site) = site {
            out.push_str("\n\nDefinition:\n\n");
            out.push_str(&literal(&text[site.range]));
        }
    }
    if sites.len() > 3 {
        out.push_str(&format!(
            "\n\n{} additional definitions. Use Go to Definition to inspect them.",
            sites.len() - 3
        ));
    }
    out
}

/// Both hover and completion resolve use this card, including unknown arity.
pub fn card(snapshot: &Analysis, path: &Path, name: &str, environment: bool) -> Option<String> {
    let file = snapshot.lookup_file(path)?;
    let symbols = snapshot.editor_symbols(file);
    let source = source_details(snapshot, path, name, environment);
    let known = if environment {
        symbols.environments.contains(name)
    } else {
        symbols.commands.contains(name)
    };
    let mut out = if environment && known && symbols.signatures.environment(name).is_none() {
        format!(
            "    \\begin{{{name}}} … \\end{{{name}}}\n\nDefined environment; argument signature is unknown."
        )
    } else if environment {
        let (sig, provenance) = crate::hover::lookup_environment(&symbols.signatures, name)?;
        crate::hover::render_environment(name, sig, &provenance)
    } else if known && symbols.signatures.command(name).is_none() {
        format!("    \\{name}\n\nDefined command; argument signature is unknown.")
    } else if let Some(doc) =
        entry(name, false).filter(|doc| doc.primitive && !known && source.is_empty())
    {
        format!(
            "    \\{name}\n\nTeX engine primitive.\n\n{}\n\n[Reference manual]({})",
            doc.summary, doc.manual
        )
    } else {
        if let Some((sig, provenance)) = crate::hover::lookup_command(&symbols.signatures, name) {
            crate::hover::render_command(name, sig, &provenance)
        } else if entry(name, false).is_some() {
            format!("    \\{name}\n\nCommand")
        } else {
            return None;
        }
    };
    // Descriptions for source-defined names would be wrong after a local redefinition.
    // Loaded kernel/package definitions retain source documentation instead.
    if source.is_empty()
        && !known
        && let Some(doc) = entry(name, environment)
        && !doc.primitive
    {
        out.push_str(&format!(
            "\n\n{}\n\n[Documentation]({})",
            doc.summary, doc.manual
        ));
    }
    if source.is_empty() && !known && entry(name, environment).is_none() {
        let links = catalogue_links(name, environment);
        if links.is_empty() {
            out.push_str(&format!("\n\n[LaTeX documentation]({LATEX})"));
        } else {
            out.push_str("\n\nPackage reference documentation: ");
            out.push_str(&links.join(", "));
        }
    }
    out.push_str(&source);
    Some(out)
}

/// CWL package provenance is documentation metadata, not evidence that a package
/// is loaded. Resolve component names through the bundled catalogue where possible.
fn catalogue_links(name: &str, environment: bool) -> Vec<String> {
    let packages = if environment {
        tex_ls_parser::semantic::signature::cwl().environment_packages(name)
    } else {
        tex_ls_parser::semantic::signature::cwl().command_packages(name)
    };
    packages
        .iter()
        .filter_map(|package| {
            let meta = tex_ls_parser::semantic::completion::package_metadata(package)?;
            Some(meta.ctan.unwrap_or(package))
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(3)
        .map(|package| {
            let label = tex_ls_analysis::bib::render::markdown_text(package);
            format!("[{label}]({})", manual_url(package))
        })
        .collect()
}

/// Package names are encoded as one URL path segment.
pub fn manual_link(name: &str) -> String {
    format!("\n\n[Package documentation]({})", manual_url(name))
}

fn manual_url(name: &str) -> url::Url {
    let mut url = url::Url::parse("https://texdoc.org/pkg/").unwrap();
    url.path_segments_mut().unwrap().pop_if_empty().push(name);
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;
    fn database(text: &str) -> (IncrementalDatabase, PathBuf) {
        let mut db = IncrementalDatabase::default();
        let path = PathBuf::from(fixture_path!("/hover-docs/main.tex"));
        db.apply_change(&path, text, None);
        (db, path)
    }
    fn hover(
        db: &IncrementalDatabase,
        path: &Path,
        offset: usize,
        encoding: PositionEncoding,
    ) -> lsp_types::Hover {
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(path).unwrap();
        let (line, character) = snapshot.file_line_index(file, encoding).position(offset);
        crate::hover::compute_hover(&snapshot, path, encoding, Position::new(line, character))
            .unwrap()
    }
    fn content(hover: &lsp_types::Hover) -> &str {
        match &hover.contents {
            lsp_types::Contents::MarkupContent(value) => &value.value,
            _ => panic!("expected markdown"),
        }
    }
    #[test]
    fn primitive_cards_do_not_invent_brace_arguments() {
        let (db, path) = database("\\expandafter\\relax");
        let value = card(&db.snapshot(), &path, "expandafter", false).unwrap();
        assert!(value.contains("second following token"));
        assert!(value.contains("Reference manual"));
        assert!(!value.contains("expandafter{}"));
        let item = lsp_types::CompletionItem {
            label: "expandafter".into(),
            data: crate::completion_resolve::CompletionResolveData::Command {
                name: "expandafter".into(),
                file: path,
            }
            .into_value(),
            ..Default::default()
        };
        let resolved = crate::completion_resolve::resolve(&db.snapshot(), item);
        assert_eq!(resolved.detail.as_deref(), Some("\\expandafter"));
        assert!(
            matches!(resolved.documentation, Some(lsp_types::Documentation::MarkupContent(doc)) if doc.value == value)
        );
    }
    #[test]
    fn token_targets_include_symbols_delimiters_and_verb_head() {
        let text = "% 😀\n\\begin{itemize}\n\\item \\% \\\\ \\verb|\\relax|\\end{itemize}";
        let (db, path) = database(text);
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            for (needle, within, expected) in [
                ("\\begin", 2, "Starts the named"),
                ("itemize", 2, "bulleted"),
                ("\\%", 1, "escaped character"),
                ("\\\\", 1, "Ends a line"),
                ("\\verb", 2, "literal text"),
                ("\\end", 2, "Ends the named"),
            ] {
                let result = hover(&db, &path, text.find(needle).unwrap() + within, encoding);
                assert!(
                    content(&result).contains(expected),
                    "{needle}: {}",
                    content(&result)
                );
                assert!(result.range.is_some());
            }
            let snapshot = db.snapshot();
            let file = snapshot.lookup_file(&path).unwrap();
            let (line, character) = snapshot
                .file_line_index(file, encoding)
                .position(text.find("\\relax").unwrap() + 2);
            assert!(
                crate::hover::compute_hover(
                    &snapshot,
                    &path,
                    encoding,
                    Position::new(line, character)
                )
                .is_none()
            );
        }
    }
    #[test]
    fn source_comments_are_literal_and_follow_dependency_changes() {
        let (mut db, path) = database("\\input{shared}\n\\custom{value}");
        let shared = path.with_file_name("shared.tex");
        db.apply_change(
            &shared,
            "% User documentation 😀\n% [unsafe](command:evil)\n\\newcommand{\\custom}[1]{#1}",
            None,
        );
        let value = card(&db.snapshot(), &path, "custom", false).unwrap();
        assert!(value.contains("shared.tex"));
        assert!(value.contains("    [unsafe](command:evil)"));
        assert!(value.contains("    User documentation 😀"));
        db.apply_change(
            &shared,
            "% Replacement docs\n\\newcommand{\\custom}[1]{#1}",
            None,
        );
        let value = card(&db.snapshot(), &path, "custom", false).unwrap();
        assert!(value.contains("Replacement docs"));
        assert!(!value.contains("User documentation"));
    }
    #[test]
    fn multiline_declarations_keep_their_leading_documentation() {
        let source =
            "% Formats a custom value.\n\\newcommand\n  {\\custom}[1]{#1}\n\\custom{value}";
        let (db, path) = database(source);
        let value = card(&db.snapshot(), &path, "custom", false).unwrap();
        assert!(value.contains("    Formats a custom value."), "{value}");
        assert!(value.contains("Source comments:"));
    }
    #[test]
    fn package_commands_and_environments_link_their_reference_manuals() {
        let (db, path) = database("");
        for (name, environment, package) in [
            ("autocite", false, "biblatex"),
            ("minted", true, "minted"),
            ("dfrac", false, "amsmath"),
            ("align", true, "amsmath"),
            ("includegraphics", false, "graphicx"),
            ("href", false, "hyperref"),
            ("frame", true, "beamer"),
        ] {
            let value = card(&db.snapshot(), &path, name, environment).unwrap();
            assert!(
                value.contains(&format!("https://texdoc.org/pkg/{package}")),
                "{name}: {value}"
            );
        }
    }
    #[test]
    fn source_only_environment_still_has_a_card_and_resolve_documentation() {
        let source = "% Custom delimited input.\n\\NewDocumentEnvironment{custom}{r()}{#1}{}\n\\begin{custom}(value)\\end{custom}";
        let (db, path) = database(source);
        let value = card(&db.snapshot(), &path, "custom", true).unwrap();
        assert!(value.contains("Custom delimited input."), "{value}");
        assert!(value.contains("argument signature is unknown"), "{value}");
        let item = lsp_types::CompletionItem {
            label: "custom".into(),
            data: crate::completion_resolve::CompletionResolveData::Environment {
                name: "custom".into(),
                file: path,
            }
            .into_value(),
            ..Default::default()
        };
        let resolved = crate::completion_resolve::resolve(&db.snapshot(), item);
        assert_eq!(resolved.detail.as_deref(), Some("\\begin{custom}"));
        assert!(
            matches!(resolved.documentation, Some(lsp_types::Documentation::MarkupContent(doc)) if doc.value == value)
        );
    }
    #[test]
    fn installed_packages_without_catalogue_metadata_keep_manual_links() {
        use tex_ls_analysis::external::{
            ExternalInputKind, ExternalInputs, InstalledMetadata, Observation,
        };
        let (mut db, path) = database("\\usepackage{newcataloguepackage}\n\\custom");
        let package = path.with_file_name("newcataloguepackage.sty");
        db.apply_change(
            &package,
            "\\ProvidesPackage{newcataloguepackage}\n\\def\\custom{value}",
            None,
        );
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                toolchain: "test".into(),
                index: TexmfIndex::from_files([("newcataloguepackage.sty".into(), package)].into()),
            })),
        )
        .unwrap();
        let value = card(&db.snapshot(), &path, "custom", false).unwrap();
        assert!(
            value.contains("https://texdoc.org/pkg/newcataloguepackage"),
            "{value}"
        );
    }
    #[test]
    fn environment_redefinitions_clear_stale_signatures_and_still_complete() {
        let source = "\\newenvironment{custom}[2]{}{}\n\\RenewDocumentEnvironment{custom}{r()}{#1}{}\n\\begin{cust";
        let (db, path) = database(source);
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(&path).unwrap();
        let symbols = snapshot.editor_symbols(file);
        assert!(symbols.environments.contains("custom"));
        assert!(symbols.signatures.environment("custom").is_none());
        let (line, character) = snapshot
            .file_line_index(file, PositionEncoding::Utf16)
            .position(source.len());
        let items = crate::completion::compute_completion(
            &snapshot,
            &path_to_uri(&path).unwrap(),
            &path,
            PositionEncoding::Utf16,
            Position::new(line, character),
        )
        .items;
        let item = items
            .into_iter()
            .find(|item| item.label == "custom")
            .expect("declared environment candidate");
        let resolved = crate::completion_resolve::resolve(&snapshot, item);
        assert_eq!(resolved.detail.as_deref(), Some("\\begin{custom}"));
    }
    #[test]
    fn unsupported_environment_arguments_never_borrow_builtin_slots() {
        let source =
            "\\RenewDocumentEnvironment{tabular}{r()}{#1}{}\n\\begin{tabular}{value}\\end{tabular}";
        let (db, path) = database(source);
        let snapshot = db.snapshot();
        let value = card(&snapshot, &path, "tabular", true).unwrap();
        assert!(value.contains("argument signature is unknown"), "{value}");
        assert!(!value.contains("column specification"), "{value}");
        let file = snapshot.lookup_file(&path).unwrap();
        let (line, character) = snapshot
            .file_line_index(file, PositionEncoding::Utf16)
            .position(source.find("value").unwrap() + 2);
        assert!(
            crate::signature_help::compute_signature_help(
                &snapshot,
                &path,
                Position::new(line, character),
                PositionEncoding::Utf16,
            )
            .is_none()
        );
    }
    #[test]
    fn local_redefinitions_do_not_inherit_builtin_descriptions() {
        let (db, path) = database("% My own behavior\n\\def\\textbf#1{X}\n\\textbf{value}");
        let value = card(&db.snapshot(), &path, "textbf", false).unwrap();
        assert!(value.contains("My own behavior"));
        assert!(!value.contains("in boldface"));
        assert!(!value.contains("Text to emphasize"));
        assert!(card(&db.snapshot(), &path, "notARealCommand", false).is_none());
    }
    #[test]
    fn unknown_arity_has_source_documentation_in_completion_too() {
        let (db, path) = database("% Delimited macro\n\\def\\custom#1;{#1}\n\\custom x;");
        let value = card(&db.snapshot(), &path, "custom", false).unwrap();
        let item = lsp_types::CompletionItem {
            label: "custom".into(),
            data: crate::completion_resolve::CompletionResolveData::Command {
                name: "custom".into(),
                file: path,
            }
            .into_value(),
            ..Default::default()
        };
        let resolved = crate::completion_resolve::resolve(&db.snapshot(), item);
        assert!(
            matches!(resolved.documentation, Some(lsp_types::Documentation::MarkupContent(doc)) if doc.value == value)
        );
        assert!(value.contains("Delimited macro"));
    }
    #[test]
    fn file_roles_and_local_package_metadata_hover() {
        let text =
            "\\usetheme{local}\n\\usepackage{custompkg}\n\\usepackage[style=numeric]{biblatex}";
        let (mut db, path) = database(text);
        for (name, body) in [
            ("beamerthemelocal.sty", "% theme"),
            (
                "custompkg.sty",
                "\\ProvidesPackage{custompkg}[2026/01/01 v1.0 Local description]",
            ),
            ("numeric.bbx", "% bibliography"),
            ("numeric.cbx", "% citations"),
        ] {
            db.apply_change(&path.with_file_name(name), body, None);
        }
        for (needle, expected) in [
            ("local", "beamerthemelocal.sty"),
            ("custompkg", "Local description"),
            ("numeric", "numeric.cbx"),
        ] {
            let value = hover(
                &db,
                &path,
                text.find(needle).unwrap() + 1,
                PositionEncoding::Utf16,
            );
            assert!(content(&value).contains(expected), "{}", content(&value));
        }
    }
    #[test]
    fn source_excerpts_are_bounded() {
        let (db, path) = database(&format!("\\def\\large{{{}}}\\large", "x".repeat(10_000)));
        let value = card(&db.snapshot(), &path, "large", false).unwrap();
        assert!(value.len() < 2000);
        assert!(value.contains('…'));
    }
    #[test]
    fn dtx_associations_supply_documentation() {
        let mut db = IncrementalDatabase::default();
        let path = PathBuf::from(fixture_path!("/hover-docs/example.dtx"));
        let source = "% \\begin{macro}{\\custom}\n% Describes the custom operation.\n%    \\begin{macrocode}\n\\def\\custom#1{#1}\n%    \\end{macrocode}\n% \\end{macro}\n";
        db.apply_change(&path, source, None);
        let value = card(&db.snapshot(), &path, "custom", false).unwrap();
        assert!(value.contains("Source documentation:"), "{value}");
        assert!(value.contains("Describes the custom operation"));
    }
    #[test]
    fn signature_help_uses_the_same_source_card() {
        let source = "% Custom operation\n\\newcommand{\\custom}[1]{#1}\n\\custom{value}";
        let (db, path) = database(source);
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(&path).unwrap();
        let (line, character) = snapshot
            .file_line_index(file, PositionEncoding::Utf16)
            .position(source.find("value").unwrap() + 1);
        let help = crate::signature_help::compute_signature_help(
            &snapshot,
            &path,
            Position::new(line, character),
            PositionEncoding::Utf16,
        )
        .unwrap();
        assert!(
            matches!(&help.signatures[0].documentation, Some(lsp_types::Documentation::MarkupContent(doc)) if Some(doc.value.clone()) == card(&snapshot, &path, "custom", false))
        );
    }
}
