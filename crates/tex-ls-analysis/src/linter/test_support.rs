//! Default source contexts for isolated rule tests. Each test controls dispatch.
use super::rules::RuleContext;
use crate::{parser::parse, semantic::SemanticModel, syntax::SyntaxNode};

pub(crate) fn with_context<T>(
    source: &str,
    run: impl FnOnce(&SyntaxNode, &RuleContext<'_>) -> T,
) -> T {
    let root = SyntaxNode::new_root(parse(source).green);
    let model = SemanticModel::build(&root);
    let context = RuleContext::new(
        std::path::Path::new("x.tex"),
        &root,
        &model,
        None,
        None,
        None,
    );
    run(&root, &context)
}
