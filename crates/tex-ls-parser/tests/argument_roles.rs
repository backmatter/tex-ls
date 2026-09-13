use tex_ls_parser::{
    parser::parse,
    semantic::SemanticModel,
    syntax::{SyntaxKind, SyntaxNode},
};

#[test]
fn uppercase_multicite_keeps_every_key_argument() {
    let source = r"\Cites{alpha}[see]{beta,gamma}";
    let parsed = parse(source);
    let root = SyntaxNode::new_root(parsed.green);
    assert_eq!(root.text().to_string(), source);
    let model = SemanticModel::build(&root);
    assert_eq!(
        model
            .citations()
            .iter()
            .map(|site| site.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta", "gamma"]
    );
}

#[test]
fn range_and_volume_roles_select_only_identifier_arguments() {
    let source = r"\zlabel{one}\crefrange{one}{two}\cites[see]{alpha}[also]{beta,gamma}\volcites{7}{delta}{9}[p.2]{epsilon}";
    let parsed = parse(source);
    let root = SyntaxNode::new_root(parsed.green);
    assert_eq!(root.text().to_string(), source);
    let model = SemanticModel::build(&root);
    assert_eq!(model.labels()[0].name, "one");
    assert_eq!(
        model
            .refs()
            .iter()
            .map(|site| site.name.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(
        model
            .citations()
            .iter()
            .map(|site| site.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta", "gamma", "delta", "epsilon"]
    );
    for site in model.citations() {
        assert_eq!(&source[site.key_range], site.name);
    }
}

#[test]
fn acronym_package_families_feed_definitions_and_uses() {
    let source = r"\DeclareAcronym{cpu}{short=CPU,long=processor}\newacro{ram}[RAM]{memory}\acrodef{rom}[ROM]{read-only memory}\acrshort{cpu}\ac{ram}\acf{rom}";
    let root = SyntaxNode::new_root(parse(source).green);
    let model = SemanticModel::build(&root);
    assert_eq!(
        model
            .glossary_defs()
            .iter()
            .map(|site| site.key.as_str())
            .collect::<Vec<_>>(),
        ["cpu", "ram", "rom"]
    );
    assert_eq!(
        model
            .glossary_uses()
            .iter()
            .map(|site| site.key.as_str())
            .collect::<Vec<_>>(),
        ["cpu", "ram", "rom"]
    );
}

#[test]
fn bare_inputs_are_lossless_and_dynamic_suffixes_are_not_paths() {
    for source in [
        "\\input chapters/a_b.tex\n",
        "\\input\nchapters/a.tex\n",
        "\\input foo\\suffix\n",
        "\\input \n",
        "\\input{broken",
    ] {
        let root = SyntaxNode::new_root(parse(source).green);
        assert_eq!(root.text().to_string(), source);
        if source.contains("suffix") {
            let command = root
                .descendants()
                .find(|node| node.kind() == SyntaxKind::COMMAND)
                .unwrap();
            assert!(tex_ls_parser::ast::nth_group_inner(&command, 0).is_none());
        }
    }
}

#[test]
fn includeonly_tracks_exclusion_without_losing_source_names() {
    let root = SyntaxNode::new_root(parse(r"\includeonly{a,c.tex}\include{a}\include{b}").green);
    let model = SemanticModel::build(&root);
    assert_eq!(model.include_only().participates("a"), Some(true));
    assert_eq!(model.include_only().participates("b"), Some(false));
    assert_eq!(model.include_only().participates("c"), Some(true));
    let root = SyntaxNode::new_root(parse(r"\includeonly{\dynamic}").green);
    assert_eq!(
        SemanticModel::build(&root).include_only().participates("a"),
        None
    );
}
