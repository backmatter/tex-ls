//! Forward search: reveal the cursor's source position in the compiled PDF.
//!
//! tex-ls never typesets, and it never reads a `.synctex.gz`. It resolves three
//! facts — the cursor's `.tex` file, the root document's PDF, and the 1-based
//! line — substitutes them into a user-configured viewer command, and spawns it.
//! Every SyncTeX-aware viewer (zathura, Okular, SumatraPDF, Skim) links
//! libsynctex and performs the actual mapping itself, which is why none of them
//! accepts a page or a coordinate: they all want a file and a line.
//!
//! The native `tex-ls.forwardSearch` command accepts a source position and reports viewer launch
//! outcomes. Viewer commands use `%f`, `%p`, and `%l` for the source file, PDF,
//! and line. The browser host does not advertise this extension.
//!
//! # The backend seam
//!
//! [`SearchTarget`] in, [`ForwardSearchOutcome`] out. The *locating* half (which
//! file is the root, where is its PDF) lives in `lsp.rs`'s `run_forward_search`;
//! everything past the `SearchTarget` is a backend, and [`spawn_viewer`] is
//! currently the only one. A native `.synctex.gz` reader — worth building only to
//! drive page-only viewers (qpdfview, a browser) or to report honestly that a
//! line produces no output — would add a sibling backend, extra `%pg`/`%x`/`%y`
//! placeholders, and a setting selecting between them. It would change neither
//! the command, nor the result shape, nor [`pdf_path`], nor any `[build]` key.
//!
//! There is deliberately **no** `trait ForwardSearchBackend` yet: a trait with a
//! single implementor is the premature re-modelling `AGENTS.md` pushes back on,
//! and promoting this free function to a trait method later is a one-line change
//! at one call site.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::BuildConfig;
use serde::{Deserialize, Serialize};

/// The sole argument to `tex-ls.forwardSearch` (LSP positions are zero-based).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ForwardSearchParams {
    pub uri: lsp_types::Uri,
    pub position: lsp_types::Position,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub(crate) enum ForwardSearchOutcome {
    Launched {
        source: PathBuf,
        pdf: PathBuf,
        line1: u32,
    },
    Unconfigured,
    MissingPdf {
        path: PathBuf,
    },
    UnsupportedSource,
    AmbiguousRoot {
        candidates: Vec<PathBuf>,
    },
    LaunchFailed {
        message: String,
    },
}

/// Where a forward search should land, in *source* space.
///
/// The backend seam (see the module docs): everything a viewer — or a future
/// native SyncTeX backend — needs, and nothing about how it is delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchTarget {
    /// `%f` — the *cursor's* file. SyncTeX indexes per input file, so this is
    /// the child, not the root.
    pub tex: PathBuf,
    /// `%p` — the *root document's* PDF. A `\input`ed child has no PDF of its
    /// own.
    pub pdf: PathBuf,
    /// `%l` — 1-based, LSP's 0-based line plus one.
    pub line: u32,
}

/// The compiled PDF for the document root `root`.
///
/// `pdf-dir` names the directory (relative → joined onto `root`'s own directory,
/// absolute → used as-is, mirroring [`aux_data_for`]'s rule); `pdf-filename`, or
/// `<root-stem>.pdf`, names the file inside it. A `pdf-filename` carrying no
/// extension gains `.pdf`, so `pdf-filename = "thesis"` does what it looks like.
///
/// Pure path algebra — no filesystem access — so the existence check stays with
/// the caller, where a missing-PDF outcome is decided.
///
/// [`aux_data_for`]: crate::aux::aux_data_for
pub(crate) fn pdf_path(root: &Path, build: &BuildConfig) -> Option<PathBuf> {
    let dir = match build.pdf_dir.as_deref() {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        Some(dir) => root.parent().unwrap_or(Path::new("")).join(dir),
        None => root.parent().unwrap_or(Path::new("")).to_path_buf(),
    };
    let name = if build.pdf_filename.is_none()
        && let Some(job_name) = &build.job_name
    {
        PathBuf::from(format!("{job_name}.pdf"))
    } else {
        match build.pdf_filename.as_deref() {
            // `[build] pdf-filename` is validated to be a bare file name, so this
            // never re-introduces a directory.
            Some(name) if Path::new(name).extension().is_some() => PathBuf::from(name),
            Some(name) => PathBuf::from(name).with_extension("pdf"),
            None => PathBuf::from(root.file_stem()?).with_extension("pdf"),
        }
    };
    Some(dir.join(name))
}

/// Substitute the forward-search placeholders into one configured viewer
/// argument.
///
/// texlab-compatible down to the corner cases, deliberately:
///
/// - an argument wrapped **entirely** in `"` passes through with the quotes
///   stripped and *no* substitution — the escape hatch for a literal `%f`;
/// - `%f` → `tex`, `%p` → `pdf`, `%l` → `line`;
/// - `%` followed by anything else emits that character *alone*, so `%%f` → `%f`
///   is the documented escape and `%z` → `z` drops the percent. The dropped
///   percent is surprising, but matching it is what keeps existing viewer
///   recipes portable;
/// - a trailing lone `%` emits `%`.
fn substitute(arg: &str, tex: &str, pdf: &str, line: u32) -> String {
    if arg.len() >= 2 && arg.starts_with('"') && arg.ends_with('"') {
        return arg[1..arg.len() - 1].to_owned();
    }
    let mut out = String::with_capacity(arg.len());
    let mut chars = arg.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('f') => out.push_str(tex),
            Some('p') => out.push_str(pdf),
            Some('l') => out.push_str(&line.to_string()),
            Some(other) => out.push(other),
            None => out.push('%'),
        }
    }
    out
}

/// [`substitute`] over every configured argument.
///
/// Paths go through [`Path::to_string_lossy`] rather than `Display`: a viewer
/// argument is data we hand to another process, so the lossy-but-defined
/// conversion is the right one for a non-UTF-8 path.
pub(crate) fn viewer_args(args: &[String], target: &SearchTarget) -> Vec<String> {
    let tex = target.tex.to_string_lossy();
    let pdf = target.pdf.to_string_lossy();
    args.iter()
        .map(|arg| substitute(arg, &tex, &pdf, target.line))
        .collect()
}

/// Launch the configured viewer at `target`.
///
/// Best-effort in the shape of [`kpsewhich_var`]: every failure funnels into one
/// status, and nothing is fed back into the formatter or linter.
///
/// Two deliberate divergences from texlab, which spawns with a blocking
/// `.status()`:
///
/// - we [`spawn`](Command::spawn) and reap on a short detached thread. The read
///   pool can be a single thread wide, so waiting on a viewer that lives for
///   hours would stall diagnostics and formatting behind it — and the client
///   wants to know the *launch* succeeded, not that the user eventually closed
///   the window. Reaping still happens (no zombies; on Windows `wait` is also
///   what closes the process handle);
/// - the exit status is ignored either way. Viewers routinely return non-zero
///   from a remote-control invocation that worked.
///
/// `executable` is spawned directly, never through a shell, so it must be a
/// program name and not a command line.
///
/// [`kpsewhich_var`]: crate::project::texmf
pub(crate) fn spawn_viewer(
    executable: &str,
    args: &[String],
    target: &SearchTarget,
) -> ForwardSearchOutcome {
    let args = viewer_args(args, target);
    match Command::new(executable)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            if std::thread::Builder::new()
                .name("tex-ls-viewer-reap".to_owned())
                .spawn(move || {
                    let _ = child.wait();
                })
                .is_err()
            {
                log::warn!("forward search: could not spawn a reaper for `{executable}`");
            }
            ForwardSearchOutcome::Launched {
                source: target.tex.clone(),
                pdf: target.pdf.clone(),
                line1: target.line,
            }
        }
        Err(err) => {
            log::error!("forward search: failed to launch `{executable}`: {err}");
            ForwardSearchOutcome::LaunchFailed {
                message: err.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SearchTarget {
        SearchTarget {
            tex: PathBuf::from("chapter.tex"),
            pdf: PathBuf::from("main.pdf"),
            line: 42,
        }
    }

    fn subst(arg: &str) -> String {
        substitute(arg, "chapter.tex", "main.pdf", 42)
    }

    #[test]
    fn substitutes_each_placeholder() {
        assert_eq!(subst("%f"), "chapter.tex");
        assert_eq!(subst("%p"), "main.pdf");
        assert_eq!(subst("%l"), "42");
        assert_eq!(
            subst("--synctex-forward=%l:1:%f"),
            "--synctex-forward=42:1:chapter.tex"
        );
        assert_eq!(subst("file:%p#src:%l%f"), "file:main.pdf#src:42chapter.tex");
    }

    #[test]
    fn fully_quoted_argument_is_verbatim() {
        assert_eq!(subst("\"%f\""), "%f");
        assert_eq!(subst("\"\""), "");
    }

    #[test]
    fn partially_quoted_argument_still_substitutes() {
        assert_eq!(subst("a\"%f\"b"), "a\"chapter.tex\"b");
        assert_eq!(subst("\"%f"), "\"chapter.tex");
    }

    #[test]
    fn double_percent_escapes() {
        assert_eq!(subst("%%f"), "%f");
        assert_eq!(subst("100%%"), "100%");
    }

    #[test]
    fn unknown_placeholder_drops_the_percent() {
        // texlab-compatible, deliberately: `%z` -> `z`. Surprising on its own,
        // but it is what makes an existing viewer recipe portable.
        assert_eq!(subst("%z"), "z");
    }

    #[test]
    fn trailing_percent_is_literal() {
        assert_eq!(subst("50%"), "50%");
    }

    #[test]
    fn placeholder_free_argument_is_identity() {
        assert_eq!(subst(""), "");
        assert_eq!(subst("--reuse-instance"), "--reuse-instance");
    }

    #[test]
    fn viewer_args_substitutes_every_argument() {
        let args: Vec<String> = ["--synctex-forward", "%l:1:%f", "%p"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(
            viewer_args(&args, &target()),
            vec!["--synctex-forward", "42:1:chapter.tex", "main.pdf"]
        );
    }

    #[test]
    fn job_name_sets_pdf_stem_and_explicit_pdf_name_wins() {
        let root = Path::new("/project/main.tex");
        let mut build = BuildConfig {
            job_name: Some("thesis.v2".into()),
            pdf_dir: Some(PathBuf::from("out")),
            ..Default::default()
        };
        assert_eq!(
            pdf_path(root, &build),
            Some(PathBuf::from("/project/out/thesis.v2.pdf"))
        );
        build.pdf_filename = Some("review".into());
        assert_eq!(
            pdf_path(root, &build),
            Some(PathBuf::from("/project/out/review.pdf"))
        );
    }

    #[test]
    fn pdf_path_defaults_beside_the_root() {
        let root = Path::new("proj").join("main.tex");
        assert_eq!(
            pdf_path(&root, &BuildConfig::default()),
            Some(Path::new("proj").join("main.pdf"))
        );
    }

    #[test]
    fn pdf_path_handles_a_root_with_no_directory() {
        assert_eq!(
            pdf_path(Path::new("main.tex"), &BuildConfig::default()),
            Some(PathBuf::from("main.pdf"))
        );
    }

    #[test]
    fn pdf_path_honors_relative_and_absolute_pdf_dir() {
        let root = Path::new("proj").join("main.tex");
        let relative = BuildConfig {
            pdf_dir: Some(PathBuf::from("out")),
            ..BuildConfig::default()
        };
        assert_eq!(
            pdf_path(&root, &relative),
            Some(Path::new("proj").join("out").join("main.pdf"))
        );

        let absolute_dir = std::env::current_dir().expect("cwd").join("build");
        let absolute = BuildConfig {
            pdf_dir: Some(absolute_dir.clone()),
            ..BuildConfig::default()
        };
        assert_eq!(
            pdf_path(&root, &absolute),
            Some(absolute_dir.join("main.pdf"))
        );
    }

    #[test]
    fn pdf_path_honors_pdf_filename_and_appends_pdf() {
        let root = Path::new("proj").join("main.tex");
        for name in ["thesis.pdf", "thesis"] {
            let build = BuildConfig {
                pdf_filename: Some(name.to_owned()),
                ..BuildConfig::default()
            };
            assert_eq!(
                pdf_path(&root, &build),
                Some(Path::new("proj").join("thesis.pdf")),
                "unexpected resolution for pdf-filename = `{name}`"
            );
        }
    }

    #[test]
    fn outcomes_have_descriptive_wire_values() {
        assert_eq!(
            serde_json::to_value(ForwardSearchOutcome::Unconfigured).unwrap(),
            serde_json::json!({"outcome":"unconfigured"})
        );
    }
}
