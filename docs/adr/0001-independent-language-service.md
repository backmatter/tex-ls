# Independent language-service ownership

Meaning is developed as an independent LaTeX language service. Upstream merge compatibility and the former root-crate API are no longer design constraints. Attribution and licenses remain intact.

On 2026-09-11, Stef accepted a refinement of the initial extraction after code investigation and comparison with rust-analyzer, gopls, and clangd. The canonical responsibility model, alternatives, evidence, and validation requirements are in [Decide the language service's ownership boundaries](../../.scratch/architecture-follow-up/issues/01-ownership.md#answer). This supersedes the initial assignment of all single-file semantics to parser, aggregate configuration to analysis, and external-fact callbacks to protocol.

The decision is accepted; implementation remains pending. Preserve the architecture checkpoint before changing internals, then follow the implementation order established by the [architecture follow-up map](../../.scratch/architecture-follow-up/map.md). Resolving the ownership ticket does not establish test results or completion of the architecture follow-up.
