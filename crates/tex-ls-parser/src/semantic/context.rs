//! Token lists that print source without executing its commands.
use rowan::TextRange;

use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Check one cursor token without collecting ranges from the whole document.
/// The parser emits TOKEN_LIST as its owning group's first node child, so group
/// braces have the same inert ownership as the tokens inside that marker.
pub fn in_token_list(token: &SyntaxToken) -> bool {
    token.parent_ancestors().any(|node| {
        node.kind() == SyntaxKind::TOKEN_LIST
            || (node.kind() == SyntaxKind::GROUP
                && node
                    .first_child()
                    .is_some_and(|child| child.kind() == SyntaxKind::TOKEN_LIST))
    })
}

/// Source ranges whose inert ownership the parser recorded. Local overrides
/// decline primitive interpretation before the marker is emitted.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TokenListContexts {
    ranges: Vec<TextRange>,
}

impl TokenListContexts {
    pub fn new(root: &SyntaxNode) -> Self {
        let mut ranges: Vec<_> = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::TOKEN_LIST)
            .filter_map(|node| node.parent())
            .filter(|node| node.kind() == SyntaxKind::GROUP)
            .map(|group| group.text_range())
            .collect();
        ranges.sort_unstable_by_key(|range| range.start());
        // CST ranges are nested or disjoint. Retain only the outer ranges so a
        // binary search can answer containment without scanning every body.
        let mut outer: Vec<TextRange> = Vec::with_capacity(ranges.len());
        for range in ranges {
            if outer.last().is_none_or(|body| !body.contains_range(range)) {
                outer.push(range);
            }
        }
        Self { ranges: outer }
    }

    pub fn contains(&self, range: TextRange) -> bool {
        let next = self
            .ranges
            .partition_point(|body| body.start() <= range.start());
        next > 0 && self.ranges[next - 1].contains_range(range)
    }

    pub fn ranges(&self) -> &[TextRange] {
        &self.ranges
    }
}

/// Control-sequence mentions consumed by a primitive operand reader. Their
/// names remain navigable, but their apparent arguments have no command roles.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PrimitiveOperandContexts {
    heads: Vec<TextRange>,
}

impl PrimitiveOperandContexts {
    pub fn from_definition_sites(root: &SyntaxNode, sites: &[super::DefSite]) -> Self {
        use crate::parser::lexer::{ParseCtx, PrimitiveOperandScan};
        use std::collections::{HashMap, HashSet};
        let mut overrides: Vec<_> = sites
            .iter()
            .filter(|site| site.kind == super::DefSiteKind::Command)
            .map(|site| {
                (
                    site.name_range.start(),
                    site.range.start(),
                    site.name.clone(),
                )
            })
            .collect();
        overrides.sort_unstable_by_key(|(start, _, _)| *start);
        let owner_starts: HashSet<_> = overrides.iter().map(|(_, owner, _)| *owner).collect();
        let mut owner_names = HashMap::new();
        let protected = TokenListContexts::new(root);
        let mut next_override = 0;
        let mut context = ParseCtx::default();
        let mut operands = PrimitiveOperandScan::default();
        let mut heads = Vec::new();
        let mut declaration_overrides = HashSet::new();
        for token in root
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
        {
            // Collect owner spellings in this existing linear token walk. Looking
            // each site up from the root can repeatedly scan long child lists.
            if owner_starts.contains(&token.text_range().start())
                && let Some(name) = token.text().strip_prefix('\\')
            {
                owner_names.insert(token.text_range().start(), name.to_owned());
            }
            while let Some((_, owner, name)) = overrides
                .get(next_override)
                .filter(|(start, _, _)| *start <= token.text_range().start())
            {
                if heads
                    .binary_search_by_key(owner, |head: &TextRange| head.start())
                    .is_err()
                    && owner_names
                        .get(owner)
                        .is_none_or(|owner| !declaration_overrides.contains(owner))
                {
                    // Raw sites include generated declarations whose factory may
                    // have been replaced earlier. Prove their owner before using
                    // them to suppress another source reader.
                    declaration_overrides.insert(name.to_string());
                    context.suppress(name.clone());
                }
                next_override += 1;
            }
            if protected.contains(token.text_range()) {
                operands = PrimitiveOperandScan::default();
                continue;
            }
            if operands.observe(token.kind(), token.text(), &context) {
                heads.push(token.text_range());
            }
        }
        Self { heads }
    }

    pub(crate) fn contains_head_start(&self, start: rowan::TextSize) -> bool {
        self.heads
            .binary_search_by_key(&start, |head| head.start())
            .is_ok()
    }

    pub fn contains_head(&self, command: &SyntaxNode) -> bool {
        command
            .first_token()
            .is_some_and(|token| self.contains_token(token.text_range()))
    }

    pub fn contains_token(&self, range: TextRange) -> bool {
        self.heads
            .binary_search_by_key(&range.start(), |head| head.start())
            .is_ok_and(|index| self.heads[index] == range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use crate::semantic::{SemanticModel, scan_definition_sites, scan_definitions};

    #[test]
    fn replaced_listing_factories_cannot_suppress_primitive_readers() {
        for override_source in [
            r"\renewcommand{\newmintinline}[1]{#1}",
            r"\let\newmintinline\relax",
            r"\RenewCommandCopy{\newmintinline}{\relax}",
            r"\ExplSyntaxOn\cs_set:cpn{newmintinline}#1{#1}\ExplSyntaxOff",
        ] {
            for (reader, invocation, closer) in [
                ("meaning", r"\meaning", ""),
                ("ifx", r"\ifx a", r"\fi"),
                ("ifdefined", r"\ifdefined", r"\fi"),
            ] {
                let source = format!(
                    r"{override_source}\newmintinline[{reader}]{{python}}{{}}{invocation}\newcommand{{\bad}}[1]{{#1}}{closer}\newcommand{{\real}}[1]{{#1}}"
                );
                let parsed = parse(&source);
                assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
                let root = parsed.syntax();
                assert_eq!(root.text().to_string(), source);
                let sites = scan_definition_sites(&root);
                assert!(
                    sites
                        .iter()
                        .all(|site| site.name != reader && site.name != "bad"),
                    "{source}: {sites:?}"
                );
                assert!(sites.iter().any(|site| site.name == "real"), "{source}");
                let operands = PrimitiveOperandContexts::from_definition_sites(&root, &sites);
                let expected_ranges = [r"\newcommand{\bad}", r"\newcommand{\real}"].map(|prefix| {
                    TextRange::at(
                        rowan::TextSize::new(source.find(prefix).unwrap() as u32),
                        rowan::TextSize::new("\\newcommand".len() as u32),
                    )
                });
                let definition_heads = root
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                    .filter(|token| token.text() == "\\newcommand")
                    .map(|token| {
                        (
                            token.text_range(),
                            operands.contains_token(token.text_range()),
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    definition_heads,
                    vec![(expected_ranges[0], true), (expected_ranges[1], false)],
                    "{source}"
                );
                let signatures = scan_definitions(&root);
                assert!(signatures.command(reader).is_none(), "{source}");
                assert!(signatures.command("bad").is_none(), "{source}");
                assert!(signatures.command("real").is_some(), "{source}");
            }
        }
    }

    #[test]
    fn copied_factory_overrides_do_not_hide_real_generated_declarations() {
        for copied_override in [
            r"\meaning\renewcommand{\newmintinline}[1]{#1}",
            r"\meaning\let\newmintinline\relax",
            r"\meaning\RenewCommandCopy{\newmintinline}{\relax}",
            r"\ExplSyntaxOn\meaning\cs_set:cpn{newmintinline}#1{#1}\ExplSyntaxOff",
        ] {
            let source = format!(
                r"{copied_override}\newmintinline[meaning]{{python}}{{}}\newcommand{{\real}}[1]{{#1}}"
            );
            let root = parse(&source).syntax();
            assert_eq!(root.text().to_string(), source);
            let sites = scan_definition_sites(&root);
            assert!(
                sites.iter().any(|site| site.name == "meaning"),
                "{source}: {sites:?}"
            );
            assert!(
                sites.iter().all(|site| site.name != "newmintinline"),
                "{source}: {sites:?}"
            );
            assert!(
                sites.iter().any(|site| site.name == "real"),
                "{source}: {sites:?}"
            );
            assert!(
                scan_definitions(&root).command("meaning").is_some(),
                "{source}"
            );
        }
    }

    #[test]
    fn listing_factory_override_validation_preserves_source_order_and_chains() {
        let source = r"\newmintinline[meaning]{python}{}\renewcommand{\newmintinline}[1]{#1}\newmintinline[ifx]{python}{}\ifx a\newcommand{\bad}[1]{#1}\newcommand{\real}[1]{#1}";
        let root = parse(source).syntax();
        assert_eq!(root.text().to_string(), source);
        let sites = scan_definition_sites(&root);
        assert!(sites.iter().any(|site| site.name == "meaning"));
        assert!(sites.iter().any(|site| site.name == "newmintinline"));
        assert!(
            sites
                .iter()
                .all(|site| site.name != "ifx" && site.name != "bad")
        );
        assert!(sites.iter().any(|site| site.name == "real"));
    }

    #[test]
    fn empty_definition_sites_still_protect_readers_and_preserve_nested_commands() {
        let source = r"\meaning\ref{fake \ref{nested}}\ifx a\ref{compared}\fi\ref{real}";
        let root = parse(source).syntax();
        let sites = scan_definition_sites(&root);
        assert!(sites.is_empty());
        let operands = PrimitiveOperandContexts::from_definition_sites(&root, &sites);
        let heads = root
            .descendants()
            .filter(|node| crate::ast::command_name(node).as_deref() == Some("ref"))
            .map(|node| operands.contains_head(&node))
            .collect::<Vec<_>>();
        assert_eq!(heads, vec![true, false, true, false]);
        let model = SemanticModel::build_with_declarations_and_operands(
            &root,
            &crate::declarations::ResolvedDeclarations::default(),
            &operands,
        );
        assert_eq!(
            model
                .refs()
                .iter()
                .map(|reference| reference.name.as_str())
                .collect::<Vec<_>>(),
            vec!["nested", "real"]
        );
    }

    #[test]
    fn operand_heads_preserve_ifx_spaces_overrides_and_active_leftover_groups() {
        for (source, expected) in [
            (r"\ifx a\input{fake}\fi", vec![true]),
            (r"\ifdefined\input{fake}\fi", vec![true]),
            (r"\ifx a \input{real}\fi", vec![false]),
            (r"\ifx\! \input{real}\fi", vec![false]),
            (r"\ifx\relax \input{fake}\fi", vec![true]),
            ("\\ifx a% comment\n\\input{fake}\\fi", vec![true]),
            ("\\ifx a% comment\n   \\input{fake}\\fi", vec![true]),
            ("\\ifx a% comment\r\n\t\\input{fake}\\fi", vec![true]),
            ("\\ifx a% comment\r   \\input{fake}\\fi", vec![true]),
            (r"\meaning\input{\input{real}}", vec![true, false]),
            (r"\detokenize{\meaning}\input{real}", vec![false]),
            (
                r"\meaning\input{fake}\renewcommand{\meaning}[1]{#1}\meaning\input{real}",
                vec![true, false],
            ),
            (
                r"\ifx a\input{fake}\fi\renewcommand{\ifx}[1]{#1}\ifx a\input{real}",
                vec![true, false],
            ),
        ] {
            let root = parse(source).syntax();
            let operands = PrimitiveOperandContexts::from_definition_sites(
                &root,
                &scan_definition_sites(&root),
            );
            let actual: Vec<_> = root
                .descendants()
                .filter(|node| crate::ast::command_name(node).as_deref() == Some("input"))
                .map(|node| operands.contains_head(&node))
                .collect();
            assert_eq!(actual, expected, "{source}");
            assert_eq!(root.text().to_string(), source);
        }
    }

    #[test]
    fn cursor_ownership_matches_ranges_including_braces_and_malformed_lists() {
        for source in [
            r"\detokenize{} \detokenize{\ref{fake}{nested}} \ref{real}",
            r"\detokenize{\ref{fake}",
            r"\renewcommand{\detokenize}[1]{#1}\detokenize{\ref{real}}",
        ] {
            let root = parse(source).syntax();
            let contexts = TokenListContexts::new(&root);
            for token in root
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
            {
                assert_eq!(
                    in_token_list(&token),
                    contexts.contains(token.text_range()),
                    "{source}: {token:?}"
                );
            }
        }
    }

    #[test]
    fn printed_token_lists_do_not_define_or_reference_names() {
        let source =
            r"\detokenize{\label{fake}\ref{fake}\newcommand{\ghost}{x}} \label{real}\ref{real}";
        let root = parse(source).syntax();
        assert_eq!(root.text().to_string(), source);
        let model = SemanticModel::build(&root);
        assert_eq!(model.labels().len(), 1);
        assert_eq!(model.refs().len(), 1);
        assert!(scan_definition_sites(&root).is_empty());
        assert!(scan_definitions(&root).command("ghost").is_none());
    }

    #[test]
    fn locally_redefined_detokenize_has_no_primitive_assumptions() {
        let root = parse(r"\renewcommand{\detokenize}[1]{#1}\detokenize{\ref{real}}").syntax();
        assert!(TokenListContexts::new(&root).ranges().is_empty());
        assert_eq!(SemanticModel::build(&root).refs().len(), 1);
    }

    #[test]
    fn copied_detokenize_has_no_primitive_assumptions() {
        let source = r"\let\detokenize\relax \detokenize{\ref{real}}";
        let parsed = parse(source);
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(TokenListContexts::new(&root).ranges().is_empty());
        assert_eq!(SemanticModel::build(&root).refs().len(), 1);
    }

    #[test]
    fn primitive_token_list_marker_preserves_group_apis_and_malformed_input() {
        for source in [
            r"\detokenize{\ref{fake}} \ref{real}",
            r"\detokenize{\ref{fake}",
        ] {
            let root = parse(source).syntax();
            assert_eq!(root.text().to_string(), source);
            let command = root
                .descendants()
                .find(|node| {
                    node.kind() == SyntaxKind::COMMAND
                        && crate::ast::command_name(node).as_deref() == Some("detokenize")
                })
                .unwrap();
            let group = crate::ast::nth_group(&command, 0).unwrap();
            assert!(
                group
                    .children()
                    .any(|node| node.kind() == SyntaxKind::TOKEN_LIST)
            );
            assert_eq!(
                TokenListContexts::new(&root).ranges(),
                &[group.text_range()]
            );
            assert!(
                SemanticModel::build(&root)
                    .refs()
                    .iter()
                    .all(|reference| reference.name == "real")
            );
        }
    }

    #[test]
    fn printed_overrides_do_not_suppress_real_listing_declaration_sites() {
        let source = r"\detokenize{\renewcommand{\newminted}[2]{#2}}\newminted[code]{python}{}";
        let root = parse(source).syntax();
        assert!(scan_definitions(&root).environment("code").is_some());
        assert!(
            scan_definition_sites(&root)
                .iter()
                .any(|site| site.name == "code")
        );
        assert!(
            crate::semantic::define::editor::editor_declarations(&root)
                .iter()
                .any(|declaration| declaration.site.name == "code")
        );
    }

    #[test]
    fn explicit_command_characters_keep_primitive_token_list_ownership() {
        let source = "\\begin{Verbatim}[commandchars=+\\[\\]]\n+detokenize[+ref[fake]]\n+meaning+detokenize[+ref[real]]\n\\end{Verbatim}";
        let root = parse(source).syntax();
        assert_eq!(root.text().to_string(), source);
        assert_eq!(TokenListContexts::new(&root).ranges().len(), 1);
        let model = SemanticModel::build(&root);
        assert_eq!(model.refs().len(), 1);
        assert_eq!(model.refs()[0].name, "real");
    }
}
