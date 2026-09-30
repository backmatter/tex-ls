//! Curated argument semantics shared by extraction and editing providers.
use super::label::{ColorDefKind, GlossaryDefKind, RefCommand};

#[derive(Clone, Copy)]
pub enum ArgumentRole {
    BibliographyItem,
    Label,
    Reference(RefCommand),
    Citation(CiteCommand),
    GlossaryDefinition(GlossaryDefKind),
    GlossaryUse,
    ColorDefinition(ColorDefKind),
}

/// Required (brace) argument index, independent of optional argument presence.
pub fn key_argument_role(name: &str, index: usize) -> Option<ArgumentRole> {
    if name == "bibitem" && index == 0 {
        return Some(ArgumentRole::BibliographyItem);
    }
    if matches!(name, "label" | "zlabel") && index == 0 {
        return Some(ArgumentRole::Label);
    }
    if let Some(kind) = ref_command(name) {
        let count = if matches!(
            name,
            "crefrange" | "Crefrange" | "cpagerefrange" | "Cpagerefrange"
        ) {
            2
        } else {
            1
        };
        return (index < count).then_some(ArgumentRole::Reference(kind));
    }
    if let Some(kind) = cite_command(name) {
        let volume = matches!(
            name,
            "volcite"
                | "Volcite"
                | "pvolcite"
                | "Pvolcite"
                | "fvolcite"
                | "Fvolcite"
                | "tvolcite"
                | "Tvolcite"
                | "avolcite"
                | "Avolcite"
                | "svolcite"
                | "Svolcite"
                | "volcites"
                | "Volcites"
                | "pvolcites"
                | "Pvolcites"
                | "fvolcites"
                | "Fvolcites"
                | "tvolcites"
                | "Tvolcites"
                | "avolcites"
                | "Avolcites"
                | "svolcites"
                | "Svolcites"
        );
        let repeated = name == "Cites" || name.ends_with("cites");
        let key = if volume {
            index % 2 == 1 && (repeated || index == 1)
        } else {
            index == 0 || repeated
        };
        return key.then_some(ArgumentRole::Citation(kind));
    }
    if index != 0 {
        return None;
    }
    if let Some(kind) = glossary_definer(name) {
        return Some(ArgumentRole::GlossaryDefinition(kind));
    }
    if is_glossary_ref_command(name) {
        return Some(ArgumentRole::GlossaryUse);
    }
    color_definer(name).map(ArgumentRole::ColorDefinition)
}

/// The behavior of a curated citation command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiteCommand {
    /// A command whose first braced argument is a comma-separated citation-key list.
    Cite,
    /// `\nocite`, whose key list additionally accepts the `*` wildcard.
    Nocite,
}

/// The recognized citation command for a control-word name, or `None`.
///
/// This is a closed table because a `cite` prefix does not establish argument
/// semantics: `\citestyle` and `\citetext`, for example, do not take citation
/// keys. `key_argument_role` supplies the exact repeated/shifted argument indices
/// for multicite and volume-cite commands.
pub fn cite_command(name: &str) -> Option<CiteCommand> {
    Some(match name {
        "nocite" => CiteCommand::Nocite,
        "cites" | "Cites" | "parencites" | "Parencites" | "textcites" | "Textcites"
        | "autocites" | "Autocites" | "footcites" | "Footcites" | "smartcites" | "Smartcites"
        | "supercites" | "volcite" | "Volcite" | "pvolcite" | "Pvolcite" | "fvolcite"
        | "Fvolcite" | "tvolcite" | "Tvolcite" | "avolcite" | "Avolcite" | "svolcite"
        | "Svolcite" | "volcites" | "Volcites" | "pvolcites" | "Pvolcites" | "fvolcites"
        | "Fvolcites" | "tvolcites" | "Tvolcites" | "avolcites" | "Avolcites" | "svolcites"
        | "Svolcites" => CiteCommand::Cite,
        "cite" | "Cite" | "citep" | "Citep" | "citet" | "Citet" | "citealt" | "Citealt"
        | "citealp" | "Citealp" | "citenum" | "citeauthor" | "Citeauthor" | "citefullauthor"
        | "Citefullauthor" | "citeyear" | "citeyearpar" | "citetalias" | "citepalias"
        | "parencite" | "Parencite" | "footcite" | "Footcite" | "footcitetext" | "Footcitetext"
        | "textcite" | "Textcite" | "smartcite" | "Smartcite" | "autocite" | "Autocite"
        | "supercite" | "fullcite" | "footfullcite" | "citetitle" | "Citetitle" | "citedate"
        | "citeurl" | "notecite" | "Notecite" | "pnotecite" | "Pnotecite" | "fnotecite"
        | "Fnotecite" | "citename" | "citelist" | "citefield" => CiteCommand::Cite,
        _ => return None,
    })
}

/// The recognized reference command for a control-word name, or `None`. A small
/// explicit table — the analog of `project::include::include_kind`. Shared with
/// the completion classifier (`crate::completion`) so the ref-family name set has
/// a single source of truth.
pub fn ref_command(name: &str) -> Option<RefCommand> {
    Some(match name {
        "ref" => RefCommand::Ref,
        "pageref" => RefCommand::PageRef,
        "eqref" => RefCommand::EqRef,
        "autoref" => RefCommand::AutoRef,
        "nameref" => RefCommand::NameRef,
        "cref" => RefCommand::Cref,
        "Cref" => RefCommand::CrefUpper,
        "vref" => RefCommand::Vref,
        "Vref" => RefCommand::VrefUpper,
        "cpageref" | "Cpageref" | "crefrange" | "Crefrange" | "cpagerefrange" | "Cpagerefrange" => {
            RefCommand::CpageRef
        }
        "zref" | "zpageref" | "zcref" | "zCref" | "labelcref" | "labelcpageref" | "namecref"
        | "nameCref" | "lcnamecref" => RefCommand::Ref,
        _ => return None,
    })
}

/// The recognized glossary/acronym *definer* command for a control-word name, or
/// `None`. The definition-side analog of [`ref_command`]; the key is always the
/// first `{…}` group.
pub(crate) fn glossary_definer(name: &str) -> Option<GlossaryDefKind> {
    Some(match name {
        "newglossaryentry"
        | "longnewglossaryentry"
        | "provideglossaryentry"
        | "longprovideglossaryentry" => GlossaryDefKind::Entry,
        "newacronym" | "DeclareAcronym" | "newacro" | "acrodef" => GlossaryDefKind::Acronym,
        "newabbreviation" => GlossaryDefKind::Abbreviation,
        _ => return None,
    })
}

/// The recognized color *definer* command for a control-word name, or `None`.
/// The definition-side analog of [`glossary_definer`]: the newly defined color
/// name is always the first `{…}` group (`\definecolor{name}{model}{spec}`,
/// `\colorlet{name}{base}`).
pub(crate) fn color_definer(name: &str) -> Option<ColorDefKind> {
    Some(match name {
        "definecolor" => ColorDefKind::DefineColor,
        "providecolor" => ColorDefKind::ProvideColor,
        "colorlet" => ColorDefKind::Colorlet,
        _ => return None,
    })
}

/// Whether `name` is a glossary/acronym *reference* command whose first `{…}`
/// group is an entry key (`\gls`, `\acrshort`, `\glsxtrfull`, …). Shared with the
/// completion classifier (`crate::completion`), like [`ref_command`] and
/// [`cite_command`], so the name set has a single source of truth. Unlike
/// citations, every command here takes exactly **one** key per group (no comma
/// list).
pub fn is_glossary_ref_command(name: &str) -> bool {
    if matches!(
        name,
        "ac" | "Ac"
            | "AC"
            | "acs"
            | "acl"
            | "acf"
            | "acp"
            | "acsp"
            | "aclp"
            | "acfp"
            | "iac"
            | "Iac"
    ) {
        return true;
    }
    // The `\gls` core set: base name + first-letter-uppercase + all-caps
    // sentence-start variants, each with an optional plural `pl`.
    const GLS: &[&str] = &[
        "gls",
        "Gls",
        "GLS",
        "glspl",
        "Glspl",
        "GLSpl",
        // Text-form accessors (`\glstext{key}` prints the entry text without
        // triggering first-use).
        "glstext",
        "Glstext",
        "glsfirst",
        "Glsfirst",
        "glsplural",
        "Glsplural",
        "glsfirstplural",
        "Glsfirstplural",
        "glsdesc",
        "Glsdesc",
        "glsname",
        "Glsname",
        "glssymbol",
        "Glssymbol",
        // Key-first commands with further groups (`\glslink{key}{text}`).
        "glslink",
        "glsdisp",
        "glsadd",
        // glossaries-extra short/long/full accessors.
        "glsxtrshort",
        "Glsxtrshort",
        "glsxtrlong",
        "Glsxtrlong",
        "glsxtrfull",
        "Glsxtrfull",
    ];
    if GLS.contains(&name) {
        return true;
    }
    // The acronym set: `\acrshort`/`\acrlong`/`\acrfull`, plural `pl`, in
    // `acr`/`Acr`/`ACR` casing.
    for stem in ["acr", "Acr", "ACR"] {
        if let Some(rest) = name.strip_prefix(stem) {
            return matches!(
                rest,
                "short" | "shortpl" | "long" | "longpl" | "full" | "fullpl"
            );
        }
    }
    false
}

/// How a literal file argument contributes to a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRole {
    Input,
    Include,
    Import,
    SubImport,
    SubFile,
    SubFileInclude,
    Glossary,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRoleKind {
    Source(SourceRole),
    Bibliography,
    Package,
    /// A module name translated to a filename by a known loader.
    NamedFile {
        prefix: &'static str,
        suffix: &'static str,
    },
    Class,
    Graphics,
    Svg,
    Inkscape,
    Raw,
}
#[derive(Debug, Clone, Copy)]
pub struct FileRole {
    pub kind: FileRoleKind,
    pub argument: usize,
    pub directory: Option<usize>,
    pub list: bool,
}

/// Shared by dependency extraction, discovery, links, navigation and completion.
pub fn file_role(name: &str) -> Option<FileRole> {
    use FileRoleKind::*;
    use SourceRole::*;
    let named = |prefix, suffix| NamedFile { prefix, suffix };
    let (kind, argument, directory, list) = match name {
        "input" | "InputIfFileExists" => (Source(Input), 0, None, false),
        "include" => (Source(Include), 0, None, false),
        "import" | "inputfrom" | "includefrom" => (Source(Import), 1, Some(0), false),
        "subimport" | "subinputfrom" | "subincludefrom" => (Source(SubImport), 1, Some(0), false),
        "subfile" => (Source(SubFile), 0, None, false),
        "subfileinclude" => (Source(SubFileInclude), 0, None, false),
        "loadglsentries" => (Source(Glossary), 0, None, false),
        "bibliography" => (Bibliography, 0, None, true),
        "addbibresource" => (Bibliography, 0, None, false),
        "usepackage" | "RequirePackage" | "RequirePackageWithOptions" => (Package, 0, None, true),
        "documentclass" | "LoadClass" | "LoadClassWithOptions" => (Class, 0, None, false),
        "RequireBibliographyStyle" => (named("", ".bbx"), 0, None, false),
        "RequireCitationStyle" => (named("", ".cbx"), 0, None, false),
        "DeclareLanguageMapping" => (named("", ".lbx"), 1, None, false),
        "bibliographystyle" => (named("", ".bst"), 0, None, false),
        "usetikzlibrary" => (named("tikzlibrary", ".code.tex"), 0, None, true),
        "usepgflibrary" => (named("pgflibrary", ".code.tex"), 0, None, true),
        "usetheme" => (named("beamertheme", ".sty"), 0, None, true),
        "usecolortheme" => (named("beamercolortheme", ".sty"), 0, None, true),
        "usefonttheme" => (named("beamerfonttheme", ".sty"), 0, None, true),
        "useinnertheme" => (named("beamerinnertheme", ".sty"), 0, None, true),
        "useoutertheme" => (named("beameroutertheme", ".sty"), 0, None, true),
        "includegraphics" => (Graphics, 0, None, false),
        "includesvg" => (Svg, 0, None, false),
        "includeinkscape" => (Inkscape, 0, None, false),
        "verbatiminput" | "VerbatimInput" | "lstinputlisting" => (Raw, 0, None, false),
        "inputminted" => (Raw, 1, None, false),
        _ => return None,
    };
    Some(FileRole {
        kind,
        argument,
        directory,
        list,
    })
}

/// A build filter preserves excluded sources as known dependencies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum IncludeOnly {
    #[default]
    Unrestricted,
    Literal(Vec<smol_str::SmolStr>),
    Dynamic,
}
impl IncludeOnly {
    pub fn participates(&self, name: &str) -> Option<bool> {
        let normalize = |name: &str| {
            name.trim()
                .strip_suffix(".tex")
                .unwrap_or(name.trim())
                .to_owned()
        };
        match self {
            Self::Unrestricted => Some(true),
            Self::Literal(names) => Some(
                names
                    .iter()
                    .any(|candidate| normalize(candidate) == normalize(name)),
            ),
            Self::Dynamic => None,
        }
    }
}

pub fn include_only(root: &crate::syntax::SyntaxNode) -> IncludeOnly {
    use crate::ast::{command_name, nth_group_inner};
    use crate::syntax::SyntaxKind;
    let mut filter = IncludeOnly::Unrestricted;
    for command in root
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::COMMAND)
    {
        if command_name(&command).as_deref() != Some("includeonly")
            || command
                .ancestors()
                .skip(1)
                .any(|node| node.kind() == SyntaxKind::COMMAND)
        {
            continue;
        }
        filter = match nth_group_inner(&command, 0) {
            Some((_, names)) => IncludeOnly::Literal(
                names
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(Into::into)
                    .collect(),
            ),
            None => IncludeOnly::Dynamic,
        };
    }
    filter
}

/// Literal file-valued biblatex options. One `style` selects both style files.
/// Values retain exact spans for links and highlighting. Comment-continued
/// values have one span per fragment; completion must not replace just a fragment.
pub fn biblatex_style_arguments(
    command: &crate::syntax::SyntaxNode,
) -> Vec<(FileRoleKind, rowan::TextRange, String)> {
    use crate::ast::command_name;
    use crate::syntax::SyntaxKind;
    use rowan::{TextRange, TextSize};
    if !command_name(command)
        .is_some_and(|name| matches!(name.as_str(), "usepackage" | "RequirePackage"))
        || !loads_biblatex(command)
    {
        return Vec::new();
    }
    let Some(options) = command
        .children()
        .find(|node| node.kind() == SyntaxKind::OPTIONAL)
    else {
        return Vec::new();
    };
    // Read TeX's literal spelling while retaining each original byte's position.
    // A comment removes its newline and following indentation, but a blank line
    // or ordinary whitespace cannot join two fragments of a filename.
    let mut raw = String::new();
    let mut positions = Vec::new();
    let mut separators = Vec::new();
    let mut depth = 0usize;
    let mut after_comment = false;
    let mut skip_indent = false;
    for token in options
        .descendants_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
    {
        if (token.kind() == SyntaxKind::L_BRACKET
            && token.text_range().start() == options.text_range().start())
            || (token.kind() == SyntaxKind::R_BRACKET
                && token.text_range().end() == options.text_range().end())
        {
            continue;
        }
        match token.kind() {
            SyntaxKind::COMMENT => {
                after_comment = true;
                continue;
            }
            SyntaxKind::NEWLINE if after_comment => {
                after_comment = false;
                skip_indent = true;
                continue;
            }
            SyntaxKind::WHITESPACE if skip_indent => continue,
            _ => skip_indent = false,
        }
        match token.kind() {
            SyntaxKind::L_BRACE => depth += 1,
            SyntaxKind::R_BRACE => depth = depth.saturating_sub(1),
            SyntaxKind::WORD if depth == 0 => {
                let start = raw.len();
                separators.extend(token.text().match_indices(',').map(|(at, _)| start + at));
            }
            _ => {}
        }
        raw.push_str(token.text());
        positions.extend(
            (0..token.text().len()).map(|at| token.text_range().start() + TextSize::new(at as u32)),
        );
    }
    if depth == 0 {
        separators.push(raw.len());
    }
    let mut result = Vec::new();
    let mut start = 0;
    for at in separators {
        let segment = &raw[start..at];
        if let Some((key, value)) = segment.split_once('=') {
            let suffixes: &[&str] = match key.trim() {
                "style" => &[".bbx", ".cbx"],
                "bibstyle" => &[".bbx"],
                "citestyle" => &[".cbx"],
                _ => &[],
            };
            let mut lo = start + key.len() + 1 + value.len() - value.trim_start().len();
            let mut text = value.trim();
            if text.starts_with('{') && text.ends_with('}') {
                text = &text[1..text.len() - 1];
                lo += 1 + text.len() - text.trim_start().len();
                text = text.trim();
            }
            if !text.chars().any(|c| {
                c.is_whitespace()
                    || matches!(c, '\\' | '#' | '{' | '}' | '%' | '=' | ',' | '[' | ']')
            }) {
                let mut ranges: Vec<TextRange> = Vec::new();
                if text.is_empty() {
                    let at = positions.get(lo).copied().unwrap_or_else(|| {
                        options
                            .last_token()
                            .filter(|token| token.kind() == SyntaxKind::R_BRACKET)
                            .map_or(options.text_range().end(), |token| {
                                token.text_range().start()
                            })
                    });
                    ranges.push(TextRange::empty(at));
                }
                for &position in &positions[lo..lo + text.len()] {
                    match ranges.last_mut() {
                        Some(previous) if previous.end() == position => {
                            *previous =
                                TextRange::new(previous.start(), position + TextSize::new(1));
                        }
                        _ => ranges.push(TextRange::at(position, TextSize::new(1))),
                    }
                }
                for &suffix in suffixes {
                    for &range in &ranges {
                        result.push((
                            FileRoleKind::NamedFile { prefix: "", suffix },
                            range,
                            text.to_owned(),
                        ));
                    }
                }
            }
        }
        start = at + 1;
    }
    result
}

fn loads_biblatex(command: &crate::syntax::SyntaxNode) -> bool {
    use crate::ast::nth_group;
    use crate::syntax::SyntaxKind;
    use rowan::NodeOrToken;

    let Some(group) = nth_group(command, 0) else {
        return false;
    };
    let mut names = String::new();
    let mut after_comment = false;
    let mut skip_indent = false;
    for element in group.children_with_tokens() {
        let NodeOrToken::Token(token) = element else {
            return false;
        };
        match token.kind() {
            SyntaxKind::L_BRACE | SyntaxKind::R_BRACE => {}
            SyntaxKind::COMMENT => after_comment = true,
            SyntaxKind::NEWLINE if after_comment => {
                after_comment = false;
                skip_indent = true;
            }
            SyntaxKind::WHITESPACE if skip_indent => {}
            SyntaxKind::WORD
            | SyntaxKind::UNDERSCORE
            | SyntaxKind::WHITESPACE
            | SyntaxKind::NEWLINE => {
                names.push_str(token.text());
                after_comment = false;
                skip_indent = false;
            }
            _ => return false,
        }
    }
    names.split(',').any(|name| name.trim() == "biblatex")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast::command_name, parser::parse};

    #[test]
    fn biblatex_style_options_ignore_comment_contents() {
        for options in [
            "% a comment\nstyle=numeric",
            "% a comment,style=ghost,\nstyle=numeric",
            "% an unmatched brace {\nstyle=numeric",
            "style=numeric% a trailing comment\n",
            "% 😀,style=ghost,\r\nstyle={numeric}% é\r\n",
        ] {
            let source = format!("\\usepackage[{options}]{{biblatex}}");
            let root = parse(&source).syntax();
            let command = root
                .descendants()
                .find(|node| command_name(node).as_deref() == Some("usepackage"))
                .unwrap();
            let arguments = biblatex_style_arguments(&command);
            assert_eq!(arguments.len(), 2, "{source}: {arguments:?}");
            for (_, range, value) in arguments {
                assert_eq!(value, "numeric", "{source}");
                assert_eq!(&source[range], value, "{source}");
            }
        }
    }

    #[test]
    fn biblatex_style_options_use_token_delimiters() {
        for options in [
            r"foo=\{,style=numeric",
            r"foo=\,style=ghost,style=numeric",
            r"foo={\},style=ghost},style=numeric",
        ] {
            let source = format!("\\usepackage[{options}]{{biblatex}}");
            let root = parse(&source).syntax();
            let command = root
                .descendants()
                .find(|node| command_name(node).as_deref() == Some("usepackage"))
                .unwrap();
            let arguments = biblatex_style_arguments(&command);
            assert_eq!(arguments.len(), 2, "{source}: {arguments:?}");
            for (_, range, value) in arguments {
                assert_eq!(value, "numeric", "{source}");
                assert_eq!(&source[range], value, "{source}");
            }
        }
    }

    #[test]
    fn biblatex_style_options_recognize_comments_in_literal_package_lists() {
        for packages in [
            "biblatex% a comment\n",
            "% a comment\nbiblatex",
            "other,% 😀\r\nbiblatex",
            "bib% a comment\nlatex",
            "bib% a comment\n  latex",
            "bib% a comment\r\n\tlatex",
        ] {
            let source = format!("\\usepackage[style=numeric]{{{packages}}}");
            let root = parse(&source).syntax();
            let command = root
                .descendants()
                .find(|node| command_name(node).as_deref() == Some("usepackage"))
                .unwrap();
            assert_eq!(biblatex_style_arguments(&command).len(), 2, "{source}");
        }
    }

    #[test]
    fn biblatex_style_options_preserve_comment_continued_value_spans() {
        for value in ["num% 😀 comment\n  eric", "{num% comment\r\n\teric}"] {
            let source = format!("\\usepackage[sty% key\nle={value}]{{biblatex}}");
            let root = parse(&source).syntax();
            let command = root
                .descendants()
                .find(|node| command_name(node).as_deref() == Some("usepackage"))
                .unwrap();
            let arguments = biblatex_style_arguments(&command);
            assert_eq!(arguments.len(), 4, "{source}: {arguments:?}");
            assert_eq!(
                arguments
                    .iter()
                    .map(|(_, range, _)| &source[*range])
                    .collect::<Vec<_>>(),
                ["num", "eric", "num", "eric"]
            );
            assert!(arguments.iter().all(|(_, _, name)| name == "numeric"));
        }
        for value in ["num% comment\n\neric", "num eric", "num\neric"] {
            let source = format!("\\usepackage[style={value}]{{biblatex}}");
            let root = parse(&source).syntax();
            let command = root
                .descendants()
                .find(|node| command_name(node).as_deref() == Some("usepackage"))
                .unwrap();
            assert!(biblatex_style_arguments(&command).is_empty(), "{source}");
        }
    }

    #[test]
    fn blank_line_does_not_continue_a_package_name() {
        let source = "\\usepackage[style=numeric]{bib% comment\n\nlatex}";
        let root = parse(source).syntax();
        let command = root
            .descendants()
            .find(|node| command_name(node).as_deref() == Some("usepackage"))
            .unwrap();
        assert!(biblatex_style_arguments(&command).is_empty());
    }
}
