//! Source classification independent of discovery.
use crate::parser::{LatexFlavor, LexConfig};
use std::path::{Path, PathBuf};
/// Which pipeline a lintable file feeds: the LaTeX layer (`.tex`, plus the
/// package/class sources `.sty`/`.cls`) or the BibTeX layer (`.bib`). `Ord` so a
/// `(PathBuf, FileKind)` list sorts/dedups by path; `Hash` so it can tag a
/// [`crate::project::ProjectMember`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileKind {
    /// A `.tex` document.
    Tex,
    /// A `*.code.tex` package-implementation file. By convention these are
    /// `\input` by a `.sty`/`.cls` under an implicit `\makeatletter` (pgf/TikZ,
    /// pgfplots, …), so — like a package source — they parse with `@` already a
    /// letter and format as code, not prose. The naming convention is the static
    /// signal; the file itself carries no `\makeatletter`.
    CodeTex,
    /// A `.sty` package source.
    Sty,
    /// A `.cls` class source.
    Cls,
    /// A `.dtx` docstrip literate source (interleaved documentation + code).
    Dtx,
    /// A `.ins` docstrip installation script (a driver TeX runs directly).
    Ins,
    /// A `.bib` bibliography database.
    Bib,
}

impl FileKind {
    /// Whether this kind feeds the LaTeX pipeline (`.tex`/`.sty`/`.cls`/`.dtx`/
    /// `.ins`), as opposed to the BibTeX one. The LaTeX kinds share a parser,
    /// formatter, and linter, differing only in
    /// [`latex_flavor`](Self::latex_flavor) and [`lex_config`](Self::lex_config).
    pub fn is_latex(self) -> bool {
        matches!(
            self,
            FileKind::Tex
                | FileKind::CodeTex
                | FileKind::Sty
                | FileKind::Cls
                | FileKind::Dtx
                | FileKind::Ins
        )
    }

    /// The [`LatexFlavor`] to parse this kind with: `.sty`/`.cls` are loaded under
    /// an implicit `\makeatletter` ([`LatexFlavor::Package`]); everything else is a
    /// plain [`LatexFlavor::Document`]. A `.dtx`'s *documentation* layer is
    /// `Document`-flavored — its `macrocode` body switches to the package regime
    /// internally (the docstrip mode, see [`lex_config`](Self::lex_config)).
    pub fn latex_flavor(self) -> LatexFlavor {
        match self {
            // `*.code.tex` is `\input` under an implicit `\makeatletter`, exactly
            // like a package source, so `@` starts as a letter.
            FileKind::Sty | FileKind::Cls | FileKind::CodeTex => LatexFlavor::Package,
            _ => LatexFlavor::Document,
        }
    }

    /// The full [`LexConfig`] to parse this kind with: its
    /// [`latex_flavor`](FileKind::latex_flavor) plus
    /// the `.dtx` docstrip mode for [`Dtx`](FileKind::Dtx).
    pub fn lex_config(self) -> LexConfig {
        LexConfig {
            flavor: self.latex_flavor(),
            dtx: matches!(self, FileKind::Dtx),
        }
    }
}

/// Whether `path`'s file name ends with `.code.tex` (case-insensitive) — the
/// package-implementation convention (`tikz.code.tex`, `pgfcorepoints.code.tex`).
/// Checked on the name, not the extension, since `Path::extension` sees only `tex`.
fn is_code_tex(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".code.tex") && lower.len() > ".code.tex".len()
        })
}

/// The lint [`FileKind`] of `path` by extension (`.tex`/`.bib`), or `None` for any
/// other file.
pub fn lint_file_kind(path: &Path) -> Option<FileKind> {
    let ext = path.extension().and_then(|ext| ext.to_str())?;
    if ["tex", "def", "lco"]
        .iter()
        .any(|alias| ext.eq_ignore_ascii_case(alias))
    {
        // A `*.code.tex` package-implementation file is loaded under an implicit
        // `\makeatletter` (checked on the full name, since the extension is just
        // `tex`); classify it apart from a plain `.tex` document.
        if is_code_tex(path) {
            Some(FileKind::CodeTex)
        } else {
            Some(FileKind::Tex)
        }
    } else if ext.eq_ignore_ascii_case("sty") {
        Some(FileKind::Sty)
    } else if ext.eq_ignore_ascii_case("cls") {
        Some(FileKind::Cls)
    } else if ext.eq_ignore_ascii_case("dtx") {
        Some(FileKind::Dtx)
    } else if ext.eq_ignore_ascii_case("ins") {
        Some(FileKind::Ins)
    } else if ["bib", "bibtex"]
        .iter()
        .any(|alias| ext.eq_ignore_ascii_case(alias))
    {
        Some(FileKind::Bib)
    } else {
        None
    }
}

/// The [`FileKind`] of `path` by extension, defaulting to [`FileKind::Tex`] for any
/// non-`.bib` extension (including none). The permissive resolver used where a
/// pipeline must be picked for content that has no real file on disk — the LSP
/// (buffers named only by URI) and the CLI's `--stdin-filepath`. Contrast
/// `collect_lint_files`, which *rejects* an unsupported explicit path rather than
/// defaulting it.
pub fn file_kind_or_tex(path: &Path) -> FileKind {
    lint_file_kind(path).unwrap_or(FileKind::Tex)
}

/// Normalize logical paths without filesystem access or a process directory.
pub fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            Component::ParentDir if out.has_root() && out.file_name().is_none() => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod normalization_tests {
    use super::*;

    #[test]
    fn lexical_parents_preserve_relative_identity_and_stop_at_root() {
        assert_eq!(
            normalize_path(Path::new("../a/../../b")),
            Path::new("../../b")
        );
        assert_eq!(normalize_path(Path::new("/a/../../b")), Path::new("/b"));
        assert_eq!(normalize_path(Path::new("/a/./b/../c")), Path::new("/a/c"));
    }
}
