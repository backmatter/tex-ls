//! Shared corpus and edit sites for `keystroke.rs` and `reparse.rs`.
//!
//! Both benchmarks edit the same construct at the same offset so their results
//! can be compared. Corpus sizes and syntax kinds are checked before timing.
//!
//! Each benchmark uses a subset of this module, hence the `dead_code` allowance.
//! `autobenches = false` in `Cargo.toml` prevents Cargo from treating this module
//! as a standalone benchmark.

#![allow(dead_code)]

use std::env;
use std::fs;
use std::path::Path;

use tex_ls_parser::declarations::ResolvedDeclarations;
use tex_ls_parser::parser::{LatexFlavor, LexConfig, parse_with_declarations_resolved};
use tex_ls_parser::syntax::SyntaxKind;

/// Where the corpus lives, relative to the workspace root `cargo bench` runs from.
pub const DOCUMENTS_DIR: &str = "benches/documents";

/// A corpus document and the size it has at the revision `download.sh` pins.
///
/// The size is asserted by [`check_corpus`], not decoration. Speedup floors are a
/// function of document size, so a document that grew silently moves every floor
/// that reads it. `download.sh` pins release tags; this check pins the bytes
/// those tags produce.
pub struct Document {
    pub name: &'static str,
    pub bytes: usize,
}

/// The size gradient both benches walk. `small.tex` is committed; the rest are
/// gitignored and come from `bash benches/documents/download.sh`.
pub const DOCUMENTS: [Document; 4] = [
    Document {
        name: "small.tex",
        bytes: 1233,
    },
    Document {
        name: "cv.tex",
        bytes: 6273,
    },
    Document {
        name: "masters_dissertation.tex",
        bytes: 95383,
    },
    Document {
        name: "phd_dissertation.tex",
        bytes: 730369,
    },
];

pub fn load_document(name: &str) -> Option<String> {
    fs::read_to_string(Path::new(DOCUMENTS_DIR).join(name)).ok()
}

/// Every reason the corpus cannot support a gate run, checked before anything is
/// timed.
///
/// A gate that measures whatever happens to be on disk is not a gate: the corpus
/// is gitignored save `small.tex` and both benches skip a missing document with a
/// note, so without this a run on a fresh checkout would pass by not measuring
/// exactly the cases that carry the strictest floors.
pub fn check_corpus() -> Vec<String> {
    let mut problems = Vec::new();
    for document in &DOCUMENTS {
        match fs::metadata(Path::new(DOCUMENTS_DIR).join(document.name)) {
            Err(_) => problems.push(format!("{DOCUMENTS_DIR}/{} is missing", document.name)),
            Ok(meta) if meta.len() as usize != document.bytes => problems.push(format!(
                "{DOCUMENTS_DIR}/{} is {} bytes, expected {} — the pinned upstream \
                 revision moved, and every floor calibrated against it moved with it",
                document.name,
                meta.len(),
                document.bytes,
            )),
            Ok(_) => {}
        }
    }
    problems
}

/// Which keystroke a bench measures.
///
/// A tier's number is only as trustworthy as its edit site, so the site is an
/// explicit choice, it is printed, and it rides the JSON report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Site {
    /// A letter typed into a word of plain prose: the token tier.
    Word,
    /// A line typed inside an `lstlisting` body: the protected-body tier, and the
    /// one keystroke that carries a newline.
    Verbatim,
    /// A backslash typed into that same word of prose: no tier claims it.
    ///
    /// Deliberately the *same offset* as [`Site::Word`], differing only in what is
    /// typed, so the pair isolates the guard rather than confounding it with
    /// position. It is also the decline that costs the most to reach: the cascade
    /// clears the newline ban, the flavor ban, the kind allow-list and the context
    /// scan, then relexes `wo\rd` into two tokens and refuses. A decline that bails
    /// on the first guard would price nothing.
    Decline,
}

impl Site {
    pub fn from_env() -> Self {
        match env::var("TEX_LS_BENCH_SITE").as_deref() {
            Ok("verbatim") => Self::Verbatim,
            Ok("decline") => Self::Decline,
            Ok("word") | Err(_) => Self::Word,
            Ok(other) => {
                panic!("TEX_LS_BENCH_SITE={other:?}: expected `word`, `verbatim`, or `decline`")
            }
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Verbatim => "verbatim",
            Self::Decline => "decline",
        }
    }

    /// What the site types.
    ///
    /// The `word` site types one letter. The `verbatim` site types a whole line,
    /// since a line terminator is the edit the protected-body tier exists to claim
    /// and the token tier refuses outright. The `decline` site types a backslash,
    /// which splits its word into two tokens and so cannot be a one-leaf splice.
    pub fn typed(self) -> &'static str {
        match self {
            Self::Word => "z",
            Self::Verbatim => "\n    total = 0;",
            Self::Decline => "\\",
        }
    }

    /// The kind of the leaf the pinned offset must land in.
    ///
    /// Check the syntax kind at the edit offset before timing. This detects
    /// changed corpus content and failed fixture injection, including an offset
    /// outside the intended verbatim body.
    pub fn expected_leaf(self) -> SyntaxKind {
        match self {
            Self::Word | Self::Decline => SyntaxKind::WORD,
            Self::Verbatim => SyntaxKind::VERBATIM_BODY,
        }
    }
}

/// One line of the synthetic listing, sized so the block is a few kilobytes — big
/// enough that relexing it is a real cost, small enough to stay a plausible listing.
pub const LISTING_LINE: &str =
    "    for (int i = 0; i < n; i++) { total += weights[i] * values[i]; }\n";
pub const LISTING_LINES: usize = 32;

/// The document the bench actually edits, and the byte offset it edits at.
///
/// No document under `benches/documents/` contains a verbatim environment, a
/// `\verb`, or a `\url`, so the protected-body site has to *inject* its construct.
/// That makes the `verbatim` numbers comparable across runs but not against the
/// `word` ones on a different document — which is why the site is printed and
/// recorded rather than inferred.
pub fn prepare(text: &str, site: Site) -> (String, usize) {
    match site {
        Site::Word | Site::Decline => {
            let at = word_interior_at_or_after(text, text.len() * 4 / 5);
            (text.to_owned(), at)
        }
        Site::Verbatim => {
            let anchor = line_start_at_or_after(text, text.len() * 4 / 5);
            let body: String = LISTING_LINE.repeat(LISTING_LINES);
            let block =
                format!("\n\\begin{{lstlisting}}[language=C]\n{body}\\end{{lstlisting}}\n\n");
            // Halfway down the body, inside a code line rather than at its edge, so
            // the edit is unambiguously interior to the one `VERBATIM_BODY` leaf.
            let into_body = "\n\\begin{lstlisting}[language=C]\n".len()
                + LISTING_LINE.len() * (LISTING_LINES / 2)
                + LISTING_LINE.len() / 2;
            let mut out = String::with_capacity(text.len() + block.len());
            out.push_str(&text[..anchor]);
            out.push_str(&block);
            out.push_str(&text[anchor..]);
            (out, anchor + into_body)
        }
    }
}

/// The first offset at or after `from` that sits strictly inside a word on a line
/// of plain prose — a place where inserting a letter extends one word and changes
/// nothing else about the document.
///
/// "Two adjacent letters" is not enough on its own: that is also the inside of
/// `\lesssim`, and at 80% of a real thesis that is exactly what it finds. So the
/// line has to be free of every character that makes it something other than
/// prose, which is what keeps the bench measuring the keystroke it claims to.
///
/// Falls back to `from` (snapped to a char boundary) when the tail holds no such
/// line, so a pathological document still benches — it just benches a different
/// keystroke, which the site pin then rejects rather than letting it pass as a
/// number.
pub fn word_interior_at_or_after(text: &str, from: usize) -> usize {
    const STRUCTURE: [char; 8] = ['\\', '{', '}', '$', '%', '&', '#', '~'];
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        if offset < from || line.trim().len() < 40 || line.contains(STRUCTURE) {
            continue;
        }
        let bytes = line.as_bytes();
        if let Some(i) = (1..line.len())
            .find(|&i| bytes[i - 1].is_ascii_alphabetic() && bytes[i].is_ascii_alphabetic())
        {
            return line_start + i;
        }
    }
    let mut at = from.min(text.len());
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// The start of the first line at or after `from`.
pub fn line_start_at_or_after(text: &str, from: usize) -> usize {
    match text[from.min(text.len())..].find('\n') {
        Some(rel) => from + rel + 1,
        None => text.len(),
    }
}

/// The parse inputs both benches run under, matching the reparse harness.
pub fn config() -> LexConfig {
    LatexFlavor::Document.into()
}

/// Check that `at` lands in the leaf kind `site` promises, returning a description
/// of the mismatch if it does not. See [`Site::expected_leaf`].
pub fn check_site_pin(document: &str, text: &str, at: usize, site: Site) -> Option<String> {
    let declared = ResolvedDeclarations::default();
    let parse = parse_with_declarations_resolved(text, config(), &declared).0;
    let root = parse.syntax();
    let offset = rowan::TextSize::try_from(at).ok()?;
    let expected = site.expected_leaf();

    let found: Vec<SyntaxKind> = root.token_at_offset(offset).map(|t| t.kind()).collect();
    if found.contains(&expected) {
        return None;
    }
    Some(format!(
        "{document}/{}: byte {at} is in {found:?}, expected a {expected:?} — the site \
         relocated, the injection stopped, or the document drifted",
        site.name(),
    ))
}
