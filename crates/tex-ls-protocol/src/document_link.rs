//! Protocol tests for analysis-owned literal file targets.
pub use tex_ls_analysis::external::links::{LinkTarget, comma_spans, document_links};
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tex_ls_analysis::incremental::Analysis;
    use tex_ls_parser::parser::parse;
    use tex_ls_parser::syntax::SyntaxNode;

    /// Parse `src` and collect links resolved against `base_dir` (no TEXMF tier).
    fn links(src: &str, base_dir: &Path) -> Vec<LinkTarget> {
        links_with_texmf(
            src,
            base_dir,
            &crate::test_host::TestHost::for_directory(base_dir),
        )
    }

    /// [`links`] with an explicit installed-tree index for the system-package tier.
    fn links_with_texmf(src: &str, base_dir: &Path, texmf: &Analysis) -> Vec<LinkTarget> {
        let root = SyntaxNode::new_root(parse(src).green);
        let facts = crate::test_host::TestHost::with_directory(base_dir, texmf.texmf().clone());
        document_links(&root, Some(base_dir), &facts)
    }

    /// The source substring a link underlines.
    fn underlined<'a>(src: &'a str, link: &LinkTarget) -> &'a str {
        &src[link.range]
    }

    #[test]
    fn input_links_only_when_the_tex_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("chap1.tex"), "").unwrap();

        let src = "\\input{chap1}\n\\input{missing}\n";
        let got = links(src, dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(underlined(src, &got[0]), "chap1");
        assert_eq!(got[0].target, dir.path().join("chap1.tex"));
    }

    #[test]
    fn explicit_extension_is_kept_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.ltx"), "").unwrap();

        let src = "\\include{notes.ltx}\n";
        let got = links(src, dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].target, dir.path().join("notes.ltx"));
    }

    #[test]
    fn usepackage_list_links_each_local_sty_separately() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mypkg.sty"), "").unwrap();
        // `amsmath` is a system package with no local file: no link.

        let src = "\\usepackage{mypkg,amsmath}\n";
        let got = links(src, dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(underlined(src, &got[0]), "mypkg");
        assert_eq!(got[0].target, dir.path().join("mypkg.sty"));
    }

    #[test]
    fn documentclass_falls_back_to_dtx() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("myclass.dtx"), "").unwrap();

        let src = "\\documentclass{myclass}\n";
        let got = links(src, dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].target, dir.path().join("myclass.dtx"));
    }

    #[test]
    fn subfiles_class_option_links_the_parent_document() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tex"), "").unwrap();

        let src = "\\documentclass[main.tex]{subfiles}\n";
        let got = links(src, dir.path());
        // The class itself has no local `subfiles.cls`, so the parent is the
        // only link.
        assert_eq!(got.len(), 1);
        assert_eq!(underlined(src, &got[0]), "main.tex");
        assert_eq!(got[0].target, dir.path().join("main.tex"));
    }

    #[test]
    fn ordinary_class_options_are_never_links() {
        let dir = tempfile::tempdir().unwrap();
        // A file named after an option would be the trap the class-name gate exists
        // to avoid.
        std::fs::write(dir.path().join("a4paper.tex"), "").unwrap();

        let got = links("\\documentclass[a4paper]{article}\n", dir.path());
        assert!(got.is_empty(), "got links: {got:?}");
    }

    #[test]
    fn subfileinclude_links_like_subfile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.tex"), "").unwrap();

        let got = links("\\subfileinclude{one}\n", dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].target, dir.path().join("one.tex"));
    }

    #[test]
    fn bibliography_defaults_bib_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("refs.bib"), "").unwrap();

        let src = "\\bibliography{refs}\n\\addbibresource{refs.bib}\n";
        let got = links(src, dir.path());
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|l| l.target == dir.path().join("refs.bib")));
    }

    #[test]
    fn includegraphics_guesses_the_first_existing_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fig.png"), "").unwrap();

        let src = "\\includegraphics{fig}\n";
        let got = links(src, dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].target, dir.path().join("fig.png"));
    }

    #[test]
    fn import_joins_dir_and_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/part.tex"), "").unwrap();

        let src = "\\import{sub}{part}\n";
        let got = links(src, dir.path());
        assert_eq!(got.len(), 1);
        // The link underlines only the `{file}` argument.
        assert_eq!(underlined(src, &got[0]), "part");
        assert_eq!(got[0].target, dir.path().join("sub/part.tex"));
    }

    #[test]
    fn nested_macro_argument_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let src = "\\input{\\foo}\n";
        assert!(links(src, dir.path()).is_empty());
    }

    #[test]
    fn system_package_resolves_through_the_texmf_index() {
        // No local `amsmath.sty`, but the installed tree has one: the load links to it.
        let base = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        let installed = tree.path().join("tex/latex/amsmath/amsmath.sty");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "").unwrap();
        let texmf = crate::test_host::TestHost::from_roots(&[tree.path().to_path_buf()]);

        let src = "\\usepackage{amsmath}\n";
        // Local-only (empty index): no link, as before.
        assert!(links(src, base.path()).is_empty());
        // With the index: the system package resolves to its installed source.
        let got = links_with_texmf(src, base.path(), &texmf);
        assert_eq!(got.len(), 1);
        assert_eq!(underlined(src, &got[0]), "amsmath");
        assert_eq!(got[0].target, installed);
    }

    #[test]
    fn local_package_wins_over_the_texmf_index() {
        // A project-local `mypkg.sty` resolves locally even when the tree also has one;
        // the `base_dir` hit is returned before the TEXMF fallback is consulted.
        let base = tempfile::tempdir().unwrap();
        std::fs::write(base.path().join("mypkg.sty"), "").unwrap();
        let tree = tempfile::tempdir().unwrap();
        let installed = tree.path().join("tex/latex/mypkg/mypkg.sty");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "").unwrap();
        let texmf = crate::test_host::TestHost::from_roots(&[tree.path().to_path_buf()]);

        let got = links_with_texmf("\\usepackage{mypkg}\n", base.path(), &texmf);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].target, base.path().join("mypkg.sty"));
    }
}
