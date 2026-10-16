//! Parse declarations and rebased reader state. Collection ownership stays here.
use crate::parser::reader_facts::reads_following_text;
use crate::semantic::signature::{ArgSpec, EnvironmentSig, builtin};
use smol_str::SmolStr;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Per-parse facts discovered from definitions or project declarations.
#[derive(Debug, Default, Clone)]
pub struct ParseCtx {
    pub(crate) reparse_guard: crate::parser::embedded::ReparseGuard,
    commands: Arc<HashMap<SmolStr, Vec<ArgSpec>>>,
    environments: Arc<HashMap<SmolStr, Vec<ArgSpec>>>,
    suppressed: Arc<HashSet<SmolStr>>,
    reader_overrides: Arc<HashMap<SmolStr, usize>>,
    reader_override_max: Option<usize>,
    reader_override_min: Option<usize>,
    reader_override_shift: isize,
    pub(crate) verbatim_comment_start: Option<usize>,
    pub(crate) release_header_start: Option<usize>,
    non_verbatim_environments: Arc<HashSet<SmolStr>>,
    non_verbatim_arg_environments: Arc<HashSet<SmolStr>>,
    begin_aliases: Arc<HashMap<SmolStr, SmolStr>>,
    end_aliases: Arc<HashMap<SmolStr, SmolStr>>,
    declared_environments: Arc<HashMap<SmolStr, EnvironmentSig>>,
    environment_name_lengths: Arc<HashSet<usize>>,
}

impl PartialEq for ParseCtx {
    fn eq(&self, other: &Self) -> bool {
        self.reparse_guard == other.reparse_guard
            && self.commands == other.commands
            && self.environments == other.environments
            && self.suppressed == other.suppressed
            && self.reader_override_max == other.reader_override_max
            && self.reader_override_min == other.reader_override_min
            && self.reader_overrides.len() == other.reader_overrides.len()
            && (self.reader_override_shift == other.reader_override_shift
                && self.reader_overrides == other.reader_overrides
                || self.reader_overrides.iter().all(|(name, offset)| {
                    other
                        .reader_overrides
                        .get(name)
                        .is_some_and(|other_offset| {
                            self.effective_reader_offset(*offset)
                                == other.effective_reader_offset(*other_offset)
                        })
                }))
            && self.verbatim_comment_start == other.verbatim_comment_start
            && self.release_header_start == other.release_header_start
            && self.non_verbatim_environments == other.non_verbatim_environments
            && self.non_verbatim_arg_environments == other.non_verbatim_arg_environments
            && self.begin_aliases == other.begin_aliases
            && self.end_aliases == other.end_aliases
            && self.declared_environments == other.declared_environments
            && self.environment_name_lengths == other.environment_name_lengths
    }
}

impl Eq for ParseCtx {}

/// Backward-compatible alias for [`ParseCtx`].
pub type VerbCtx = ParseCtx;

impl ParseCtx {
    pub(super) fn has_environment_name_length(&self, length: usize) -> bool {
        self.environment_name_lengths.contains(&length)
    }

    pub(super) fn is_environment_suppressed(&self, name: &str) -> bool {
        self.non_verbatim_environments.contains(name)
    }

    /// Returns whether the context contains no discovered or declared facts.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
            && self.environments.is_empty()
            && self.suppressed.is_empty()
            && self.reader_overrides.is_empty()
            && self.verbatim_comment_start.is_none()
            && self.release_header_start.is_none()
            && self.non_verbatim_environments.is_empty()
            && self.non_verbatim_arg_environments.is_empty()
            && self.begin_aliases.is_empty()
            && self.end_aliases.is_empty()
            && self.declared_environments.is_empty()
    }

    /// Applies project declarations over discovered facts.
    pub fn overlay_declarations(&mut self, declared: &crate::declarations::ResolvedDeclarations) {
        let db = declared.as_db();
        for name in db.environment_names() {
            if let Some(sig) = db.environment(name) {
                Arc::make_mut(&mut self.non_verbatim_environments).remove(name);
                Arc::make_mut(&mut self.non_verbatim_arg_environments).remove(name);
                Arc::make_mut(&mut self.declared_environments)
                    .insert(SmolStr::new(name), sig.clone());
                Arc::make_mut(&mut self.environment_name_lengths).insert(name.len());
            }
        }
        for (name, target) in db.env_begin_aliases() {
            self.insert_begin_alias(SmolStr::new(name), SmolStr::new(target));
        }
        for (name, target) in db.env_end_aliases() {
            self.insert_end_alias(SmolStr::new(name), SmolStr::new(target));
        }
    }

    pub(crate) fn insert(&mut self, name: SmolStr, leading: Vec<ArgSpec>) {
        Arc::make_mut(&mut self.commands).insert(name, leading);
    }

    pub(crate) fn suppress(&mut self, name: SmolStr) {
        Arc::make_mut(&mut self.suppressed).insert(name);
    }

    pub(crate) fn suppress_reader_after(&mut self, name: SmolStr, offset: usize) {
        self.materialize_reader_offsets();
        let previous = self.reader_overrides.get(&name).copied();
        let earliest = previous.map_or(offset, |previous| previous.min(offset));
        if previous == Some(earliest) {
            return;
        }
        Arc::make_mut(&mut self.reader_overrides).insert(name, earliest);
        self.reader_override_min = Some(
            self.reader_override_min
                .map_or(earliest, |minimum| minimum.min(earliest)),
        );
        self.reader_override_max = if previous.is_some_and(|previous| {
            Some(previous) == self.reader_override_max && earliest < previous
        }) {
            // Normal source-ordered construction never reduces an existing
            // earliest offset. Out-of-order insertion still keeps this exact.
            self.reader_overrides.values().copied().max()
        } else {
            Some(
                self.reader_override_max
                    .map_or(earliest, |maximum| maximum.max(earliest)),
            )
        };
    }

    pub(super) fn is_reader_suppressed_at(&self, name: &str, offset: usize) -> bool {
        self.is_suppressed(name)
            || self.reader_overrides.get(name).is_some_and(|start| {
                self.effective_reader_offset(*start)
                    .is_some_and(|start| start <= offset)
            })
    }

    fn effective_reader_offset(&self, offset: usize) -> Option<usize> {
        offset.checked_add_signed(self.reader_override_shift)
    }

    fn materialize_reader_offsets(&mut self) {
        if self.reader_override_shift == 0 {
            return;
        }
        self.reader_overrides = Arc::new(
            self.reader_overrides
                .iter()
                .map(|(name, offset)| {
                    (
                        name.clone(),
                        self.effective_reader_offset(*offset)
                            .expect("validated reader coordinate"),
                    )
                })
                .collect(),
        );
        self.reader_override_shift = 0;
    }

    /// Rebase source positions after a proved leaf edit. All other facts remain
    /// shared. Edits before every binding shift one coordinate bias in O(1).
    pub(crate) fn after_leaf_edit(
        &self,
        start: usize,
        end: usize,
        inserted: usize,
    ) -> Option<Self> {
        if start > end
            || [self.verbatim_comment_start, self.release_header_start]
                .into_iter()
                .flatten()
                .any(|profile| start < profile)
        {
            return None;
        }
        let removed = end - start;
        let delta = isize::try_from(inserted)
            .ok()?
            .checked_sub(isize::try_from(removed).ok()?)?;
        let mut next = self.clone();
        if delta == 0
            || self
                .reader_override_max
                .is_none_or(|maximum| maximum < start)
        {
            return Some(next);
        }
        if self
            .reader_override_min
            .is_some_and(|minimum| end <= minimum)
        {
            next.reader_override_shift = self.reader_override_shift.checked_add(delta)?;
            next.reader_override_min = Some(self.reader_override_min?.checked_add_signed(delta)?);
            next.reader_override_max = Some(self.reader_override_max?.checked_add_signed(delta)?);
            return Some(next);
        }
        let mut offsets = HashMap::with_capacity(self.reader_overrides.len());
        for (name, offset) in self.reader_overrides.iter() {
            let offset = self.effective_reader_offset(*offset)?;
            if start <= offset && offset < end {
                return None;
            }
            let offset = if offset >= end {
                offset.checked_add_signed(delta)?
            } else {
                offset
            };
            offsets.insert(name.clone(), offset);
        }
        next.reader_override_min = offsets.values().copied().min();
        next.reader_override_max = offsets.values().copied().max();
        next.reader_override_shift = 0;
        next.reader_overrides = Arc::new(offsets);
        Some(next)
    }

    pub(crate) fn has_reader_override(&self, name: &str) -> bool {
        self.reader_overrides.contains_key(name)
    }

    pub(crate) fn reader_overrides_after_or_at(&self, offset: usize) -> bool {
        self.reader_override_max
            .is_some_and(|maximum| maximum >= offset)
    }

    pub(crate) fn is_suppressed(&self, name: &str) -> bool {
        self.suppressed.contains(name)
    }

    pub(crate) fn suppress_environment(&mut self, name: SmolStr) {
        Arc::make_mut(&mut self.non_verbatim_environments).insert(name);
    }

    pub(crate) fn suppress_environment_arg(&mut self, name: SmolStr) {
        Arc::make_mut(&mut self.non_verbatim_arg_environments).insert(name);
    }

    pub(super) fn is_verbatim_arg_environment(&self, name: &str) -> bool {
        if let Some(sig) = self.declared_environment(name) {
            return sig.verbatim_arg;
        }
        !self.non_verbatim_arg_environments.contains(name)
            && builtin()
                .environment(name)
                .is_some_and(|sig| sig.verbatim_arg)
    }

    pub(crate) fn insert_environment(&mut self, name: SmolStr, args: Vec<ArgSpec>) {
        Arc::make_mut(&mut self.environment_name_lengths).insert(name.len());
        Arc::make_mut(&mut self.environments).insert(name, args);
    }

    /// A command whose spelling cannot change this parse's lexer state.
    pub(crate) fn inert_control_word(&self, text: &str) -> bool {
        let Some(name) = text.strip_prefix('\\') else {
            return false;
        };
        !name.is_empty()
            && name.bytes().all(|b| b.is_ascii_lowercase())
            && !reads_following_text(text)
            && !matches!(
                name,
                "verb"
                    | "makeatletter"
                    | "makeatother"
                    | "begingroup"
                    | "endgroup"
                    | "bgroup"
                    | "egroup"
            )
            && !crate::parser::embedded::observes_settings_command(name)
            && !self.commands.contains_key(name)
            && !self.begin_aliases.contains_key(name)
            && !self.end_aliases.contains_key(name)
            && builtin()
                .command(name)
                .is_none_or(|sig| !sig.verbatim && sig.args.is_empty())
    }

    /// A known ordinary argument owner for the restricted full-mode token proof.
    pub(crate) fn ordinary_math_command(&self, text: &str) -> bool {
        let Some(name) = text.strip_prefix('\\') else {
            return false;
        };
        !self.commands.contains_key(name)
            && !reads_following_text(text)
            && !crate::parser::embedded::observes_settings_command(name)
            && builtin().command(name).is_some_and(|signature| {
                !signature.verbatim && signature.args.iter().all(|argument| !argument.verbatim)
            })
    }

    /// Closed builtin raw profiles whose body cannot inherit embedded settings.
    pub(crate) fn pure_builtin_raw_command(&self, name: &str, offset: usize) -> bool {
        matches!(name, "url" | "path")
            && !self.commands.contains_key(name)
            && !self.is_reader_suppressed_at(name, offset)
            && builtin()
                .command(name)
                .is_some_and(|signature| signature.verbatim)
    }

    pub(crate) fn pure_builtin_raw_environment(&self, name: &str) -> bool {
        matches!(name, "verbatim" | "verbatim*")
            && !self.environments.contains_key(name)
            && !self.non_verbatim_environments.contains(name)
            && self
                .declared_environment(name)
                .is_none_or(|signature| builtin().environment(name) == Some(signature))
    }

    pub(super) fn leading_args(&self, name: &str) -> Option<&[ArgSpec]> {
        self.commands.get(name).map(Vec::as_slice)
    }

    pub(super) fn verbatim_environment_args(&self, name: &str) -> Option<&[ArgSpec]> {
        if let Some(sig) = self.declared_environment(name) {
            return sig.verbatim_body.then(|| &*sig.args);
        }
        self.environments.get(name).map(Vec::as_slice)
    }

    pub(crate) fn is_literal_alltt_environment(&self, name: &str) -> bool {
        name == "alltt"
            && !self.environments.contains_key(name)
            && self.declared_environment(name).is_none()
            && !self.non_verbatim_environments.contains(name)
    }

    pub(crate) fn is_verbatim_environment(&self, name: &str) -> bool {
        if name == "alltt"
            && !self.environments.contains_key(name)
            && self.declared_environment(name).is_none()
        {
            return false;
        }
        if self.non_verbatim_environments.contains(name) {
            return false;
        }
        match self.declared_environment(name) {
            Some(sig) => sig.verbatim_body,
            None => {
                self.environments.contains_key(name)
                    || builtin()
                        .environment(name)
                        .is_some_and(|env| env.verbatim_body)
            }
        }
    }

    pub(crate) fn insert_begin_alias(&mut self, name: SmolStr, target: SmolStr) {
        Arc::make_mut(&mut self.begin_aliases).insert(name, target);
    }

    pub(crate) fn insert_end_alias(&mut self, name: SmolStr, target: SmolStr) {
        Arc::make_mut(&mut self.end_aliases).insert(name, target);
    }

    pub(crate) fn begin_alias(&self, name: &str) -> Option<&str> {
        self.begin_aliases.get(name).map(SmolStr::as_str)
    }

    pub(crate) fn end_alias(&self, name: &str) -> Option<&str> {
        self.end_aliases.get(name).map(SmolStr::as_str)
    }

    pub(crate) fn begin_alias_targets(&self) -> impl Iterator<Item = &str> {
        self.begin_aliases.values().map(SmolStr::as_str)
    }

    pub(super) fn declared_environment(&self, name: &str) -> Option<&EnvironmentSig> {
        self.declared_environments.get(name)
    }

    pub(crate) fn is_block_environment(&self, name: &str) -> bool {
        match self.declared_environment(name) {
            Some(sig) => sig.block(),
            None => builtin()
                .environment(name)
                .is_some_and(EnvironmentSig::block),
        }
    }

    pub(crate) fn is_math_environment(&self, name: &str) -> bool {
        match self.declared_environment(name) {
            Some(sig) => sig.math,
            None => builtin().environment(name).is_some_and(|env| env.math),
        }
    }

    pub(crate) fn is_statement_environment(&self, name: &str) -> bool {
        match self.declared_environment(name) {
            Some(sig) => sig.statement_body,
            None => builtin()
                .environment(name)
                .is_some_and(|env| env.statement_body),
        }
    }

    pub(crate) fn has_env_aliases(&self) -> bool {
        !self.begin_aliases.is_empty() || !self.end_aliases.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebased_reader_offsets_share_facts_and_match_absolute_map_oracle() {
        let mut ctx = ParseCtx::default();
        for i in 0..8192 {
            ctx.insert(SmolStr::new(format!("raw{i}")), Vec::new());
        }
        for (name, offset) in [("meaning", 7), ("ifx", 51), ("verb", 1024)] {
            ctx.suppress_reader_after(name.into(), offset);
        }
        let retained = ctx.clone();
        let original_offsets = (*ctx.reader_overrides).clone();
        let mut reference = original_offsets.clone();
        for (start, end, inserted, shared_offsets) in [
            (0, 0, 3, true),
            (17, 20, 1, false),
            (0, 2, 0, true),
            (1200, 1200, 1, true),
        ] {
            let previous_offsets = ctx.reader_overrides.clone();
            let next = ctx.after_leaf_edit(start, end, inserted).unwrap();
            let delta = inserted as isize - (end - start) as isize;
            for offset in reference.values_mut() {
                assert!(!(start <= *offset && *offset < end));
                if *offset >= end {
                    *offset = offset.checked_add_signed(delta).unwrap();
                }
            }
            assert!(Arc::ptr_eq(&ctx.commands, &next.commands));
            assert!(Arc::ptr_eq(&ctx.environments, &next.environments));
            assert!(Arc::ptr_eq(
                &ctx.declared_environments,
                &next.declared_environments
            ));
            assert!(Arc::ptr_eq(&ctx.begin_aliases, &next.begin_aliases));
            assert_eq!(
                Arc::ptr_eq(&previous_offsets, &next.reader_overrides),
                shared_offsets
            );
            for at in 0..1030 {
                assert_eq!(
                    next.reader_overrides_after_or_at(at),
                    reference.values().any(|offset| *offset >= at)
                );
                for name in ["meaning", "ifx", "verb", "unknown"] {
                    assert_eq!(
                        next.is_reader_suppressed_at(name, at),
                        reference.get(name).is_some_and(|offset| *offset <= at)
                    );
                }
            }
            let mut absolute = retained.clone();
            absolute.reader_overrides = Arc::new(reference.clone());
            absolute.reader_override_min = reference.values().copied().min();
            absolute.reader_override_max = reference.values().copied().max();
            absolute.reader_override_shift = 0;
            assert_eq!(next, absolute);
            ctx = next;
        }
        assert_eq!(*retained.reader_overrides, original_offsets);
        let at = *reference.get("ifx").unwrap();
        assert!(ctx.after_leaf_edit(at, at + 1, 0).is_none());
        assert!(ctx.after_leaf_edit(1, 0, 0).is_none());
        let mut extreme = ParseCtx::default();
        extreme.suppress_reader_after("meaning".into(), usize::MAX);
        assert!(extreme.after_leaf_edit(0, 0, 1).is_none());
    }

    #[test]
    fn reader_override_boundary_matches_map_after_out_of_order_updates() {
        let mut ctx = ParseCtx::default();
        let updates = [
            ("a", 40),
            ("b", 20),
            ("a", 10),
            ("c", 20),
            ("b", 1),
            ("c", 0),
            ("a", 0),
            ("a", 50),
            ("b", 1),
        ];
        for offset in 0..=50 {
            assert!(!ctx.reader_overrides_after_or_at(offset));
        }
        for (name, start) in updates {
            ctx.suppress_reader_after(name.into(), start);
            let before_queries = ctx.clone();
            for offset in 0..=50 {
                assert_eq!(
                    ctx.reader_overrides_after_or_at(offset),
                    ctx.reader_overrides.values().any(|start| *start >= offset)
                );
            }
            assert_eq!(ctx, before_queries);
        }
        let mut reordered = ParseCtx::default();
        for (name, start) in updates.into_iter().rev() {
            reordered.suppress_reader_after(name.into(), start);
        }
        assert_eq!(ctx, reordered);
        for index in 0..4096 {
            ctx.suppress_reader_after(format!("generated{index}").into(), index);
        }
        assert_eq!(ctx.reader_override_max, Some(4095));
        assert!(!ctx.reader_overrides_after_or_at(4096));
    }
}
