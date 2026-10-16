//! Default source contexts for isolated BibTeX rule tests.
use super::{rules::BibRuleContext, suppression::BibSuppressionMap};
use crate::bib::{parse, semantic::Model, syntax::SyntaxNode};

pub(crate) fn with_context<T>(
    source: &str,
    run: impl FnOnce(&SyntaxNode, &BibRuleContext<'_>) -> T,
) -> T {
    let root = parse(source).syntax();
    let model = Model::build(&root);
    let suppressions = BibSuppressionMap::build(&root);
    let context = BibRuleContext {
        project: None,
        path: std::path::Path::new("x.bib"),
        root: &root,
        source,
        model: &model,
        db: crate::bib::semantic::builtin(),
        suppressions: &suppressions,
    };
    run(&root, &context)
}
