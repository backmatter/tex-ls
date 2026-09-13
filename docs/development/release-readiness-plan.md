# Release readiness

Reviewed 2026-09-13. This file replaces the original 13-item review plan and tracks
the corrected candidate. The release scope is tagged source, matching the existing
installation instructions and `publish = false` manifests. Native executables and
browser bindings can be built from that source; automated artifact distribution
has not been added.

## Completed fixes

- Parser recursion is bounded on native and WASM hosts. Excessively nested input
  retains every byte, reports a diagnostic and leaves the session usable. Tests
  cover balanced/unclosed groups, math, environments, BibTeX and incremental edits.
- Expl3 fallback statements use syntax and preserved comment/blank-line boundaries.
  Single newlines no longer determine their layout. Both argument scanners reset
  newline runs at `~`, so newlines on either side do not become a false blank line.
  The 13 recorded non-convergence cases and ten newly discovered `.def` cases pass.
- Unknown optional arguments preserve glued opening edges. All eleven typesetting
  fixtures pass text and rendered-page comparisons, including keyval colors and
  geometry. Compiler and PDF-tool errors fail the check and retain artifacts.
- Debug formatting emits versioned JSON with typed failure classes. The corpus
  gate validates coverage, counts and exit status without parsing Markdown.
- Formatter lowering is split into private modules for prose, expl3, environments,
  lists, alignment, groups, commands, math and trivia. Worker dispatch and
  acquisition have separate modules. CLI debug commands have their own module;
  redundant response-forwarding functions and repetitive layout comments are gone.
- Fallback watching performs one scan honoring source ignores and checks explicit
  compiler candidates, including missing paths and acquired AUX chains. It no
  longer traverses every ignored build/dependency directory. Artifact-scope updates
  are coalesced and respect the polling interval, fixing repeated scans during
  large diagnostic requests. New roots still scan immediately. Debug logs report
  scope and duration; the RPC benchmark reports process-wide idle CPU/read counters.
- Native allocation benchmarks share one counter. Browser stages, removal/rebuild
  cost and session retention profiles remain separate because they measure
  different operations; their command table now says which.
- BibLaTeX 3.21 and the TeX Live 2025 package database have validated SHA-256 inputs.
  All four generators match their upstream inputs. Three offline selftests run in CI.
- Dependency manifests now give version constraints for path and pinned Git
  dependencies. `cargo deny --all-features check` passes. Production licenses use
  an explicit `include-dev = false`; advisory and source checks include tests.
  The initial license-mismatch suspicion was incorrect: cargo-deny already excluded
  development-only licenses by default. GPL texlab crates remain test-only.
- Both corpus checks verify shared pins and clean checkouts. Reparse coverage now
  includes `.def` and `.lco`. Recording cannot overwrite baselines after a failed
  equivalence assertion or splice floor.
- Earlier review fixes remain: reliable gate exit handling, browser context wrapper
  and examples, unused CLI fix-pass removal, stale-comment cleanup, generator UTF-8
  handling, corpus-fetch safeguards and benchmark process cleanup.

## Reviewed baselines and limitations

The lower reparse counts predate this review's edits. Commit `0b1073d` added two
conservative guards without refreshing the baselines: leaf edits inside
`BARE_ARGUMENT` decline, and edit chains involving `\input` use full parsing.
Those guards are retained because bare filename attachment crosses trivia and can
change ownership. The expanded corpus baselines are recorded only after full-tree
and diagnostic equivalence assertions and the splice floors pass. Lower splice
counts alone are not a latency regression.

Formatter baselines retain malformed and unsupported inputs. The newly discovered
PGF `pgfsys-common-svg.def` changes the catcode of `%` before defining a literal
percent character, which this surface grammar does not execute. Its refusal is
recorded and explained in the formatting guide. No supported-input convergence
failure is accepted as a baseline.

The pinned source sets are unchanged. Performance results are observations of this
Linux machine, not cross-platform guarantees. Extracted text and raster equality
cover the typesetting fixtures, not every possible TeX package or document.

## Final verification

Verified on Linux with Rust 1.98.1, Node 24.13.0, wasm-bindgen 0.2.128 and
cargo-deny 0.20.2. All checks below passed.

| Check | Result |
| --- | --- |
| Formatting and Clippy, all workspace targets/features | Passed with warnings denied |
| Workspace tests, all features | 2,740 passed; three optional corpus tests ignored |
| Rustdoc and architecture checker | Passed with rustdoc warnings denied |
| All-feature WASM builds of browser, parser and formatter | Passed |
| Native/WASM host parity | Browser bridge, deep nesting/recovery and 35 transcripts passed |
| Formatter corpora | All eight reports match; 176 refusal rows, zero accepted convergence failures |
| Reparse corpora | All four baselines match; 6,384 files including `.def`/`.lco`; equivalence and floors passed |
| Typesetting | All eleven fixtures preserve text and rendered pages |
| Dependency policy | Advisories, versions, licenses and sources passed with all features |
| Metadata generators | All four upstream comparisons and three offline selftests passed |
| Clean source installation | Fresh build, installed CLI checks, native deep expl3 request and recovery passed |
| Structured report validation | Rejects missing coverage/counts, execution errors, inconsistent statuses and unknown schemas/classes |
| Documentation and scripts | Relative Markdown targets, Python compilation and shell syntax checks passed |

The installation started from an unpacked source snapshot with an empty target
directory, then was rebuilt with the final watcher fix. Downloaded dependency
sources were cached. TeX validation used locally downloaded packages without
changing the system installation.

Final timings ran after the repository's builds, corpus and typesetting checks.
On this Ryzen 7 5700U machine, five warm completion edits had median times of
42.2 ms (one file), 50.2 ms (100 files) and 100.8 ms (1,000 files). Each workload
recorded zero warm acquisition reads and compiler probes.

A separate workspace contained 1,001 sources and 20,000 ignored source/artifact
files. Over five idle seconds, the server used 310 ms of process CPU and made
50 read calls totaling 6,745 bytes. Warm edits again caused zero acquisition reads
or compiler probes. These process-wide counters include watching and background
work; they do not count every metadata syscall. OS caches were not flushed, and
these are observations, not performance guarantees or a controlled before/after
comparison. Raw local logs are retained in `target/release-readiness/`.

## Public repository assessment

The project is ready for an early source release once the platform CI gate passes.
The repository is already public. It has an MIT license with upstream attribution,
separate notices for bundled data, installation and editor guides, contribution
instructions, and automated language, architecture, and dependency checks.

The 0.1.0 release review repeated all 2,740 workspace tests on Linux, with three
optional corpus tests ignored. Formatting, Clippy, Rustdoc, WASM builds, host parity,
dependency policy, generator selftests, and all eleven typesetting fixtures passed.
A fresh source archive built and installed successfully; the installed executable
reported `tex-ls 0.1.0` and passed formatting, format-check, and lint smoke checks.
Relative Markdown links resolved. A limited scan of source and Git history found
no private-key, GitHub-token, or AWS-access-key patterns; this is not a security
audit. The review corrected an old crate path in the unicode-math notice and made
the README install the release tag. `CHANGELOG.md` records the release scope.

Fresh platform CI also exposed Windows issues that the earlier Linux review could
not detect. TEXINPUTS rejected short directory names containing `~`; a regression
assertion now covers that case. Exclude matching also handles canonical Windows
roots, short-path watcher events, deleted paths, and files outside the root without
panicking. Native and embedded tests now use absolute paths
with drive letters on Windows, transcript substitutions operate on parsed JSON,
and the BibTeX reparse baseline explicitly retains LF checkout line endings.
CI runs all test targets even after a failure so one platform report includes all
failing suites. Linux and macOS passed before these fixture corrections; the tag
requires the final amended baseline to pass all platform checks.

Follow-up work after 0.1.0:

- Provide prebuilt executables if installation without a Rust toolchain is needed.
  This release intentionally uses tagged source, with `publish = false` crates.
- Establish a private vulnerability-reporting route and document it in
  `SECURITY.md`. GitHub secret scanning and push protection were disabled when
  reviewed; enabling them would help catch accidental credential commits.
- Gather editor and platform feedback. The library and browser APIs remain
  unstable, and TeX macro execution and dynamic catcode changes are outside the
  parser's supported grammar.

The release baseline uses one root commit with the Conventional Commit subject
`feat: release tex-ls 0.1.0`. Earlier commit IDs in this review describe local
review history, retained in local backup branches rather than the public baseline.

## Release steps

- [x] Save the candidate on local branch `release/readiness-2026-09-13`, including
  the new modules and transcripts. The working tree and index on `main` remain
  available; no existing changes are discarded.
- [x] Validate installation from a clean source snapshot of the candidate.
- [ ] Review Linux/macOS/Windows and dependency CI for that commit.
- [ ] Tag the validated source and publish its release notes. These are release
  operations, not prerequisites for reviewing the local fixes.

Suggested release notes: LaTeX/BibTeX language server, formatter and linter with
native stdio and browser sessions; completion, navigation, rename, diagnostics,
semantic highlighting and document structure. Deep nesting recovers with a syntax
diagnostic; formatting refuses unsupported syntax. Library and browser APIs remain
unstable before 1.0. Build tools and editor setup are documented in the README and
guides.
