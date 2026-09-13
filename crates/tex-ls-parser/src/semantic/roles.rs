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
    let (kind, argument, directory, list) = match name {
        "input" => (Source(Input), 0, None, false),
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
