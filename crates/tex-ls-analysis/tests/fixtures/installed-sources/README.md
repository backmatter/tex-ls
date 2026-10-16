# Installed source identity fixtures

These files retain the exact upstream bytes and their copyright and license
notices. They exercise a content identity catalog, not a filename or header
heuristic. The runtime catalog contains SHA-256 digests and lengths; full source
bytes are compiled only into tests. `catalog.json` records every fixture digest,
archive digest, source URL, and upstream pin.

## Sources

- `2025/` comes from the official [LaTeX2e release-2025-11-01](https://github.com/latex3/latex2e/tree/22d4c5ec30bba33928a6e80dd26543daf6b438fc), commit `22d4c5ec30bba33928a6e80dd26543daf6b438fc`. The source archive has SHA-256 `6809addb1b56779436fa3f0a4e16e5b854a75d4849f1d80b5585d7a3e8e9ce53`.
- `2026/` contains generated files from the official CTAN TDS archives for [latex-base](https://ctan.org/pkg/latex-base), [latex-amsmath](https://ctan.org/pkg/amsmath), and [latex-tools](https://ctan.org/pkg/latex-tools), downloaded on 2026-10-13. Archive hashes are pinned in `catalog.json`. The archive bytes take precedence over package-page version summaries. The AMS archive contains `amsmath` 2.18d, while the older official release contains 2.17z.
- `common/amsfonts.sty` and `common/amssymb.sty` are from the official [AMSFonts TDS archive](https://ctan.org/pkg/amsfonts), archive SHA-256 `dd763ca89b712d096e952ca43c3b60409bfca174f04c5743da68468d063fe7b6`.
- `common/booktabs.sty` is the generated file in the official TeX Live `booktabs` archive, revision 77682, downloaded from `https://mirrors.ctan.org/systems/texlive/tlnet/archive/booktabs.tar.xz`. Archive SHA-256 is `a5a9ed5c865f6aad4cd197f189f796f87b272bf9e0ebd2851debca2eef5fb613`. Its source is the maintained [CTAN booktabs package](https://ctan.org/pkg/booktabs).

CTAN archive URLs can change their contents. Reproduction must first compare the
downloaded archive hash with the pinned value. A mismatch requires a new source
audit and catalog entry. It must never silently update an existing identity.

## Reproducing the 2025 files

Extract the pinned source archive into a disposable directory. Run the original
docstrip installation scripts with native TeX and docstrip v2.6c, dated
2024-12-23. Disable overwrite prompts in the disposable `.ins` copies by placing
`\askforoverwritefalse` immediately after their `\input docstrip` line. This
changes the extraction interaction, not the emitted preamble or source code.

```sh
cd base
tex -interaction=batchmode classes.ins
cd ../required/amsmath
tex -interaction=batchmode amsmath.ins
tex -interaction=batchmode amstext.ins
tex -interaction=batchmode amsbsy.ins
tex -interaction=batchmode amsopn.ins
tex -interaction=batchmode amsgen.ins
cd ../tools
tex -interaction=batchmode tools.ins
```

The tools script emits `array.sty` before processing its other files. Compare
each selected generated file's complete SHA-256 digest with `catalog.json`.
Source locations within the archive are `base/`, `required/amsmath/`, and
`required/tools/`. The five class files, AMS sources, and `array.sty` generated
from this official pin were independently compared with the installed files;
all raw digests match. Local installed bytes did not supply the identities.

## Profile boundaries and licenses

The profiles assume the ordinary current LaTeX format, without `latexrelease`,
compatibility mode, or class/package options. Default article/report size input
is `size10.clo`; book uses `bk10.clo`. `amsmath` requires `amstext`, `amsbsy`, and
`amsopn`, each of which requires `amsgen`. `amssymb` requires `amsfonts`.
`array` and `booktabs` have no direct preamble source dependencies under these
conditions. Present profile optional-source lists are empty. Font selection and
later font-use files remain part of the declared standard-format assumption.

Exact byte recognition establishes no installed provenance. Action callers must
also prove resolution precedence, installed selection, absence of aliases and
editor overlays, and the required dependency closure. Unknown versions,
one-byte mutations, and newline normalization fail closed.

The LaTeX base, AMS math, tools, and booktabs source notices specify LPPL 1.3c or
later. The AMSFonts package source notices retain their own AMS distribution
terms. `LPPL-notice.txt` preserves the official release's license notice.
Consult the corresponding full upstream distributions and source notices when
redistributing generated files.
