use tex_ls_parser::parser::parse;
use tex_ls_parser::semantic::SemanticModel;

fn assert_fake_reference_is_active(before: &str, listing: &str) {
    let source = format!("{before}{listing}");
    let parsed = parse(&source);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1, "{source}");
    assert_eq!(model.refs()[0].name, "fake", "{source}");
}

#[test]
fn dynamic_listing_settings_cannot_preserve_prior_tex_ownership() {
    for (before, setter, listing) in [
        (
            r"\lstset{language=Python,mathescape=true}",
            r"\lstset",
            "\\begin{lstlisting}\n# x=$\\ref{fake}$\n\\end{lstlisting}",
        ),
        (
            r"\fvset{commandchars=+\[\]}",
            r"\fvset",
            "\\begin{Verbatim}\n+ref[fake]\n\\end{Verbatim}",
        ),
        (
            r"\setminted{mathescape=true}",
            r"\setminted",
            "\\begin{minted}{python}\n# x=$\\ref{fake}$\n\\end{minted}",
        ),
        (
            r"\setminted[python]{mathescape=true}",
            r"\setminted[python]",
            "\\begin{minted}{python}\n# x=$\\ref{fake}$\n\\end{minted}",
        ),
        (
            r"\setmintedinline{mathescape=true}",
            r"\setmintedinline",
            r"\mintinline{python}|# x=$\ref{fake}$|",
        ),
        (
            r"\setmintedinline[python]{mathescape=true}",
            r"\setmintedinline[python]",
            r"\mintinline{python}|# x=$\ref{fake}$|",
        ),
    ] {
        assert_fake_reference_is_active(before, listing);
        for options in [r"{\dynamic=false}", r"{\options}", r"\options", ""] {
            let source = format!("{before}{setter}{options}{listing}\\ref{{real}}");
            let parsed = parse(&source);
            let root = parsed.syntax();
            assert_eq!(root.text().to_string(), source);
            let model = SemanticModel::build(&root);
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "real", "{source}");
        }
    }
}

#[test]
fn dynamic_minted_language_cannot_prove_the_affected_defaults() {
    for (before, setter, listing) in [
        (
            r"\setminted[python]{mathescape=true}",
            r"\setminted[\language]{mathescape=false}",
            "\\begin{minted}{python}\n# x=$\\ref{fake}$\n\\end{minted}",
        ),
        (
            r"\setmintedinline[python]{mathescape=true}",
            r"\setmintedinline[\language]{mathescape=false}",
            r"\mintinline{python}|# x=$\ref{fake}$|",
        ),
    ] {
        assert_fake_reference_is_active(before, listing);
        let source = format!("{before}{setter}{listing}\\ref{{real}}");
        let parsed = parse(&source);
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        let model = SemanticModel::build(&root);
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real", "{source}");
    }
}

#[test]
fn dynamic_listing_uncertainty_restores_with_its_scope() {
    for (before, setter, listing) in [
        (
            r"\lstset{language=Python,mathescape=true}",
            r"\lstset{\options}",
            "\\begin{lstlisting}\n# x=$\\ref{KEY}$\n\\end{lstlisting}",
        ),
        (
            r"\fvset{commandchars=+\[\]}",
            r"\fvset\options",
            "\\begin{Verbatim}\n+ref[KEY]\n\\end{Verbatim}",
        ),
    ] {
        let inner = listing.replace("KEY", "fake");
        let outer = listing.replace("KEY", "real");
        let source = format!("{before}{{{setter}{inner}}}{outer}");
        let parsed = parse(&source);
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        let model = SemanticModel::build(&root);
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real", "{source}");
    }
}

#[test]
fn unknown_static_listing_options_do_not_hide_proved_tex_islands() {
    let source = concat!(
        "\\lstset{language=Python,mathescape=true}",
        "\\lstset{futureoption=\\value,anotheroption=true}",
        "\\begin{lstlisting}\n# x=$\\ref{real}$\n\\end{lstlisting}",
    );
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "real");
}

#[test]
fn unproved_style_redefinitions_cannot_reuse_prior_tex_ownership() {
    for declaration in [
        r"\lstdefinestyle{code}",
        r"\lstdefinestyle{code}\options",
        r"\lstdefinestyle{code}{\options}",
        r"\lstdefinestyle{\name}{mathescape=false}",
        r"\lstdefinestyle\name",
    ] {
        let source = format!(
            "\\lstdefinestyle{{code}}{{language=Python,mathescape=true}}{declaration}\\begin{{lstlisting}}[style=code]\n# x=$\\ref{{fake}}$\n\\end{{lstlisting}}\\ref{{real}}"
        );
        let parsed = parse(&source);
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        let model = SemanticModel::build(&root);
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real", "{source}");
    }
}

#[test]
fn literal_style_redefinition_recovers_after_an_unknown_target() {
    let source = concat!(
        "\\lstdefinestyle{code}{language=Python,mathescape=true}",
        "\\lstdefinestyle{\\name}{mathescape=false}",
        "\\begin{lstlisting}[style=code]\n# x=$\\ref{fake}$\n\\end{lstlisting}",
        "\\lstdefinestyle{code}{language=Python,mathescape=true}",
        "\\begin{lstlisting}[style=code]\n# x=$\\ref{real}$\n\\end{lstlisting}",
    );
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "real");
}

#[test]
fn static_style_and_language_names_allow_spaces_and_unicode() {
    for source in [
        "\\lstdefinestyle{my α style}{language=Python,mathescape=true}\\begin{lstlisting}[style=my α style]\n# x=$\\ref{real}$\n\\end{lstlisting}",
        "\\setminted[python]{mathescape=true}\\setminted[My Language!]{mathescape=false}\\begin{minted}{python}\n# x=$\\ref{real}$\n\\end{minted}",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real", "{source}");
    }
}

#[test]
fn malformed_named_generated_specs_cannot_reuse_prior_tex_ownership() {
    for (before, declaration, listing) in [
        (
            r"\newminted[code]{python}{mathescape=true}",
            r"\newminted[code]{python}\options",
            "\\begin{code}\n# x=$\\ref{fake}$\n\\end{code}",
        ),
        (
            r"\newminted{python}{mathescape=true}",
            r"\newminted{python}\options",
            "\\begin{pythoncode*}{}\n# x=$\\ref{fake}$\n\\end{pythoncode*}",
        ),
        (
            r"\newmintinline[code]{python}{mathescape=true}",
            r"\newmintinline[code]{python}\options",
            r"\code|# x=$\ref{fake}$|",
        ),
        (
            r"\lstnewenvironment{code}{\lstset{language=Python,mathescape=true}}{}",
            r"\lstnewenvironment{code}\options",
            "\\begin{code}\n# x=$\\ref{fake}$\n\\end{code}",
        ),
        (
            r"\DefineVerbatimEnvironment{code}{Verbatim}{commandchars=+\[\]}",
            r"\DefineVerbatimEnvironment{code}{Verbatim}\options",
            "\\begin{code}\n+ref[fake]\n\\end{code}",
        ),
    ] {
        assert_fake_reference_is_active(before, listing);
        let source = format!("{before}{declaration}{listing}\\ref{{real}}");
        let parsed = parse(&source);
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        let model = SemanticModel::build(&root);
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real", "{source}");
    }
}
