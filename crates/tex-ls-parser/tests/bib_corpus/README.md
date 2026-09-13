# BibTeX test corpus

Every `.bib` file is discovered by these parser tests:

- `bib_roundtrip` checks lossless reconstruction.
- `bib_parse_oracle` checks a minimum entry-recognition rate against texlab.
- `bib_parse_compat` compares parse structure. Run it with
  `cargo test -p tex-ls-parser --test bib_parse_compat -- --ignored --nocapture`.

Most fixtures test individual constructs. `biblatex-examples.bib` is an unmodified
example database from biblatex 3.21, containing about 92 entries across 15 types.
It covers entry sets, cross-references, protected casing, multiline values, commands,
and string concatenation.

The source is `bibtex/bib/biblatex/biblatex/biblatex-examples.bib` in biblatex 3.21,
originally copied from the Nix store package `biblatex-3.21-tex`.
It is distributed under LPPL 1.3c, the LaTeX Project Public License.
Authors are Philipp Lehman, Joseph Wright, Audrey Boruvka, Philip Kime, and contributors.
