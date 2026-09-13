#!/usr/bin/env python3
"""Generate package/class names and CTAN metadata from a pinned TeX Live tlpdb.

Completion uses .sty/.cls file stems, which can differ from CTAN package names.
Namesake and common stems appear before the --- separator; Rust preserves this
order as completion rank. Other stems follow in alphabetical order.
Metadata maps each stem to its package's short description and catalogue ID.
The source is the frozen tlnet-final snapshot selected by TL_YEAR.

Usage:
    python3 scripts/gen_package_names.py                 # check generated files
    python3 scripts/gen_package_names.py --write         # regenerate
    python3 scripts/gen_package_names.py --source FILE   # local tlpdb or .xz
    python3 scripts/gen_package_names.py --selftest       # offline checks
"""

from __future__ import annotations

import argparse
import hashlib
import json
import lzma
import sys
import urllib.request
from pathlib import Path

DATA_DIR = Path(__file__).resolve().parent.parent / "crates" / "tex-ls-parser" / "data"
PACKAGE_FILE = DATA_DIR / "package_names.txt"
CLASS_FILE = DATA_DIR / "class_names.txt"
METADATA_FILE = DATA_DIR / "package_metadata.json"

# Pinned source: the frozen historic tlnet-final tlpdb for this TeX Live release.
# Bump deliberately (then `--write`); the year is echoed into the file headers.
TL_YEAR = "2025"
TLPDB_URL = (
    "https://ftp.tu-chemnitz.de/pub/tug/historic/systems/texlive/"
    f"{TL_YEAR}/tlnet-final/tlpkg/texlive.tlpdb.xz"
)

TLPDB_SHA256 = "74b5744d2ff1386138d5361d3af49179f921dbbc953e449324d99448af092f41"

# The separator line between the primary (ranked-first) and secondary name blocks.
SEP = "---"

# Common names whose stem differs from their package name (or are so ubiquitous they
# should rank first regardless). Namesake stems (stem == package) are promoted
# automatically; this set covers the rest. Expand freely.
COMMON_PACKAGES = frozenset(
    (
        "tikz pgf pgfplots inputenc fontenc amssymb amsfonts amsthm amsopn"
        " graphicx graphics xcolor color babel microtype fontspec csquotes"
        " geometry fancyhdr hyperref url cleveref nameref varioref booktabs"
        " array tabularx longtable multirow multicol makecell colortbl enumitem"
        " caption subcaption float wrapfig rotating listings minted fancyvrb"
        " verbatim tcolorbox mdframed framed siunitx natbib biblatex todonotes"
        " soul ulem etoolbox xparse xspace ifthen calc environ setspace parskip"
        " titlesec titletoc appendix glossaries acronym nomencl makeidx imakeidx"
        " pdfpages standalone import subfiles lipsum blindtext mhchem chemfig"
        " datetime2 hyperref bm mathtools esint mathrsfs cancel physics amsmath"
    ).split()
)

COMMON_CLASSES = frozenset(
    (
        "article report book letter slides proc minimal beamer memoir standalone"
        " extarticle extreport extbook amsart amsbook scrartcl scrreprt scrbook"
        " scrlttr2 tufte-handout tufte-book moderncv acmart IEEEtran elsarticle"
        " revtex4-2 llncs subfiles"
    ).split()
)


# --- tlpdb parsing ------------------------------------------------------------


def parse_tlpdb(
    text: str,
) -> tuple[dict[str, str], dict[str, str], dict[str, dict[str, str]]]:
    """Parse ``texlive.tlpdb`` text into ``(packages, classes, meta)``. ``packages``
    and ``classes`` map ``stem -> owning-package-name``. ``meta`` maps
    ``package-name -> {"shortdesc": ..., "catalogue": ...}`` (either key omitted when
    absent). Each blank-line-separated block starts with a ``name <pkg>`` line;
    ``runfiles`` lists follow a ``runfiles size=…`` line as space-indented paths, from
    which we collect the ``.sty`` / ``.cls`` basenames. ``shortdesc`` and
    ``catalogue`` are top-level ``key value`` lines."""
    packages: dict[str, str] = {}
    classes: dict[str, str] = {}
    meta: dict[str, dict[str, str]] = {}
    pkg = None
    in_runfiles = False

    for line in text.splitlines():
        if not line:
            pkg = None
            in_runfiles = False
            continue
        if line.startswith(" "):
            # A continuation entry: a file path when we're inside runfiles.
            if in_runfiles and pkg is not None:
                path = line.strip()
                base = path.rsplit("/", 1)[-1]
                if base.endswith(".sty"):
                    packages.setdefault(base[:-4], pkg)
                elif base.endswith(".cls"):
                    classes.setdefault(base[:-4], pkg)
            continue
        # A top-level ``key value`` line.
        key, _, value = line.partition(" ")
        if key == "name":
            pkg = value.strip()
            in_runfiles = False
        elif key == "runfiles":
            in_runfiles = True
        else:
            in_runfiles = False
            if pkg is not None and value.strip():
                if key == "shortdesc":
                    meta.setdefault(pkg, {})["shortdesc"] = value.strip()
                elif key == "catalogue":
                    meta.setdefault(pkg, {})["catalogue"] = value.strip()

    return packages, classes, meta


def rank(stems: dict[str, str], common: frozenset[str]) -> tuple[list[str], list[str]]:
    """Split ``stem -> package`` into ``(primary, secondary)`` sorted name lists. A
    stem is primary if it is its package's namesake or in ``common``."""
    primary = sorted(s for s, p in stems.items() if s == p or s in common)
    primary_set = set(primary)
    secondary = sorted(s for s in stems if s not in primary_set)
    return primary, secondary


def render(kind: str, primary: list[str], secondary: list[str]) -> str:
    """Render a name-list file: a ``#`` header, the primary block, a ``---`` line,
    then the secondary block. Lines starting with ``#`` and the ``---`` separator are
    skipped by the Rust loader; the blank/separator boundary carries the rank."""
    header = (
        f"# GENERATED by scripts/gen_package_names.py from TeX Live {TL_YEAR} "
        f"(tlnet-final texlive.tlpdb) -- do not edit by hand.\n"
        f"# {kind} name stems for \\usepackage / \\documentclass completion. Common "
        f"and namesake names first, then `{SEP}`, then the long tail.\n"
        f"# Run `python3 scripts/gen_package_names.py --write` to regenerate.\n"
    )
    body = "\n".join(primary) + f"\n{SEP}\n" + "\n".join(secondary) + "\n"
    return header + body


def render_metadata(
    packages: dict[str, str],
    classes: dict[str, str],
    meta: dict[str, dict[str, str]],
) -> str:
    """Render ``data/package_metadata.json``: a ``stem -> {desc, ctan}`` map for every
    ``.sty``/``.cls`` stem whose owning package carries a ``shortdesc`` and/or CTAN
    ``catalogue`` id. ``desc`` is the one-line description (omitted when absent);
    ``ctan`` is the CTAN catalogue id (the Rust side builds ``ctan.org/pkg/<id>``),
    defaulting to the package name. A stem with neither is skipped. Packages win over
    classes on a shared stem (they resolve to the same CTAN package anyway)."""
    entries: dict[str, dict[str, str]] = {}
    # Classes first so packages overwrite on a shared stem.
    for stem, pkg in list(classes.items()) + list(packages.items()):
        info = meta.get(pkg, {})
        desc = info.get("shortdesc")
        ctan = info.get("catalogue") or pkg
        if not desc and not info.get("catalogue"):
            # No description and no real CTAN entry: nothing worth showing.
            continue
        entry: dict[str, str] = {"ctan": ctan}
        if desc:
            entry["desc"] = desc
        entries[stem] = entry

    note = (
        f"GENERATED by scripts/gen_package_names.py from TeX Live {TL_YEAR} "
        f"(tlnet-final texlive.tlpdb) -- do not edit by hand. "
        f"Run `python3 scripts/gen_package_names.py --write` to regenerate."
    )
    doc = {"note": note, "entries": entries}
    return json.dumps(doc, indent=1, sort_keys=True) + "\n"


# --- source fetching ----------------------------------------------------------


def fetch_tlpdb(source: str | None) -> str:
    """Return the tlpdb text: from ``--source`` (a ``.tlpdb`` or ``.xz`` file) or the
    pinned frozen tlnet-final snapshot."""
    if source is not None:
        raw = Path(source).read_bytes()
    else:
        try:
            with urllib.request.urlopen(TLPDB_URL, timeout=120) as resp:
                raw = resp.read()
        except OSError as e:
            sys.exit(f"error: failed to fetch {TLPDB_URL}: {e}")
    if raw[:6] == b"\xfd7zXZ\x00":  # xz magic
        raw = lzma.decompress(raw)
    if hashlib.sha256(raw).hexdigest() != TLPDB_SHA256:
        sys.exit("error: input does not match the pinned TeX Live 2025 package database")
    return raw.decode("utf-8")


# --- self-test ----------------------------------------------------------------


def _selftest() -> int:
    def eq(got, want, msg):
        assert got == want, f"{msg}: got {got!r}, want {want!r}"

    sample = (
        "name amsmath\n"
        "category Package\n"
        "shortdesc AMS mathematical facilities\n"
        "catalogue amsmath\n"
        "runfiles size=42\n"
        " texmf-dist/tex/latex/amsmath/amsmath.sty\n"
        " texmf-dist/tex/latex/amsmath/amstext.sty\n"
        " texmf-dist/doc/latex/amsmath/README\n"
        "catalogue-ctan /macros/latex/required/amsmath\n"
        "\n"
        "name koma-script\n"
        "shortdesc A bundle of versatile classes and packages\n"
        "runfiles size=9\n"
        " RELOC/tex/latex/koma-script/scrartcl.cls\n"
        " RELOC/tex/latex/koma-script/scrbook.cls\n"
        "\n"
        "name pgf\n"
        "runfiles size=1\n"
        " texmf-dist/tex/latex/pgf/frontendlayer/tikz.sty\n"
    )
    pkgs, classes, meta = parse_tlpdb(sample)
    eq(pkgs.get("amsmath"), "amsmath", "amsmath stem")
    eq(pkgs.get("amstext"), "amsmath", "amstext owned by amsmath")
    eq(pkgs.get("tikz"), "pgf", "tikz owned by pgf")
    eq("amsmath" in classes, False, "no class from sty")
    eq(classes.get("scrartcl"), "koma-script", "scrartcl class stem")
    eq(meta.get("amsmath", {}).get("shortdesc"), "AMS mathematical facilities", "shortdesc")
    eq(meta.get("amsmath", {}).get("catalogue"), "amsmath", "catalogue id")

    primary, secondary = rank(pkgs, COMMON_PACKAGES)
    eq("amsmath" in primary, True, "namesake amsmath primary")
    eq("tikz" in primary, True, "common tikz primary")
    eq("amstext" in secondary, True, "internal amstext secondary")

    cprimary, _ = rank(classes, COMMON_CLASSES)
    eq("scrartcl" in cprimary, True, "scrartcl common-class primary")

    out = render("Package", primary, secondary)
    eq(out.count(f"\n{SEP}\n"), 1, "one separator")
    eq(out.startswith("# GENERATED"), True, "header present")

    meta_json = render_metadata(pkgs, classes, meta)
    parsed = json.loads(meta_json)
    eq(parsed["entries"]["amsmath"]["desc"], "AMS mathematical facilities", "meta desc")
    eq(parsed["entries"]["amsmath"]["ctan"], "amsmath", "meta ctan id")
    # `tikz` stem is owned by `pgf`, which has no shortdesc/catalogue -> skipped.
    eq("tikz" in parsed["entries"], False, "no meta without desc/catalogue")
    # A class stem inherits its package's shortdesc; ctan falls back to the pkg name.
    eq(parsed["entries"]["scrartcl"]["ctan"], "koma-script", "class ctan fallback")
    print("selftest: ok")
    return 0


# --- main ---------------------------------------------------------------------


def _write_or_check(path: Path, rendered: str, write: bool, label: str) -> int:
    n = rendered.count("\n") - rendered.count("\n#") - 1  # rough name count
    if write:
        path.write_text(rendered, encoding="utf-8")
        print(f"wrote {path.relative_to(Path(__file__).resolve().parent.parent)} (~{n} {label} names)")
        return 0
    current = path.read_text(encoding="utf-8") if path.is_file() else ""
    if current == rendered:
        print(f"{path.name} is in sync (~{n} {label} names)")
        return 0
    print(f"{path.name} is OUT OF SYNC with TeX Live {TL_YEAR}.", file=sys.stderr)
    print("run with --write to regenerate.", file=sys.stderr)
    return 1


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--write", action="store_true", help="regenerate the lists in place")
    ap.add_argument("--source", metavar="FILE", help="local texlive.tlpdb (or .xz)")
    ap.add_argument("--selftest", action="store_true", help="run offline unit checks")
    args = ap.parse_args()

    if args.selftest:
        return _selftest()

    text = fetch_tlpdb(args.source)
    pkgs, classes, meta = parse_tlpdb(text)
    if not pkgs or not classes:
        sys.exit("error: parsed no package/class stems -- is this a full tlpdb?")

    p_primary, p_secondary = rank(pkgs, COMMON_PACKAGES)
    c_primary, c_secondary = rank(classes, COMMON_CLASSES)

    rc = _write_or_check(
        PACKAGE_FILE, render("Package", p_primary, p_secondary), args.write, "package"
    )
    rc |= _write_or_check(
        CLASS_FILE, render("Class", c_primary, c_secondary), args.write, "class"
    )
    rc |= _write_or_check(
        METADATA_FILE, render_metadata(pkgs, classes, meta), args.write, "metadata"
    )
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
