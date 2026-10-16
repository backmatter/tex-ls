//! Exact identities of pinned upstream package sources.
//!
//! Recognition proves bytes only. Callers separately establish installed
//! selection, absence of earlier local candidates, and absence of overlays.
//! Profiles cover the ordinary current LaTeX format without latexrelease,
//! package/class options, or compatibility mode. Font files loaded on later
//! font use are outside this preamble loader profile. Unknown bytes fail closed.
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstalledSourceId {
    Article,
    Report,
    Book,
    Size10,
    Bk10,
    Amsmath,
    Amstext,
    Amsbsy,
    Amsopn,
    Amsgen,
    Amssymb,
    Amsfonts,
    Booktabs,
    Array,
}

#[derive(Debug, PartialEq, Eq)]
pub struct InstalledSourceProfile {
    pub name: &'static str,
    pub required: &'static [&'static str],
    pub optional: &'static [&'static str],
}

pub fn profile(identity: InstalledSourceId) -> &'static InstalledSourceProfile {
    match identity {
        InstalledSourceId::Article => &InstalledSourceProfile {
            name: "article.cls",
            required: &["size10.clo"],
            optional: &[],
        },
        InstalledSourceId::Report => &InstalledSourceProfile {
            name: "report.cls",
            required: &["size10.clo"],
            optional: &[],
        },
        InstalledSourceId::Book => &InstalledSourceProfile {
            name: "book.cls",
            required: &["bk10.clo"],
            optional: &[],
        },
        InstalledSourceId::Size10 => &InstalledSourceProfile {
            name: "size10.clo",
            required: &[],
            optional: &[],
        },
        InstalledSourceId::Bk10 => &InstalledSourceProfile {
            name: "bk10.clo",
            required: &[],
            optional: &[],
        },
        InstalledSourceId::Amsmath => &InstalledSourceProfile {
            name: "amsmath.sty",
            required: &["amstext.sty", "amsbsy.sty", "amsopn.sty"],
            optional: &[],
        },
        InstalledSourceId::Amstext => &InstalledSourceProfile {
            name: "amstext.sty",
            required: &["amsgen.sty"],
            optional: &[],
        },
        InstalledSourceId::Amsbsy => &InstalledSourceProfile {
            name: "amsbsy.sty",
            required: &["amsgen.sty"],
            optional: &[],
        },
        InstalledSourceId::Amsopn => &InstalledSourceProfile {
            name: "amsopn.sty",
            required: &["amsgen.sty"],
            optional: &[],
        },
        InstalledSourceId::Amsgen => &InstalledSourceProfile {
            name: "amsgen.sty",
            required: &[],
            optional: &[],
        },
        InstalledSourceId::Amssymb => &InstalledSourceProfile {
            name: "amssymb.sty",
            required: &["amsfonts.sty"],
            optional: &[],
        },
        InstalledSourceId::Amsfonts => &InstalledSourceProfile {
            name: "amsfonts.sty",
            required: &[],
            optional: &[],
        },
        InstalledSourceId::Booktabs => &InstalledSourceProfile {
            name: "booktabs.sty",
            required: &[],
            optional: &[],
        },
        InstalledSourceId::Array => &InstalledSourceProfile {
            name: "array.sty",
            required: &[],
            optional: &[],
        },
    }
}

struct KnownSource {
    identity: InstalledSourceId,
    bytes: usize,
    sha256: [u8; 32],
}

const KNOWN: &[KnownSource] = &[
    KnownSource {
        identity: InstalledSourceId::Article,
        bytes: 20144,
        sha256: [
            0xf9, 0xc8, 0x77, 0x0a, 0x90, 0x9e, 0x5b, 0x65, 0x26, 0xeb, 0x72, 0x28, 0x4e, 0x45,
            0x7c, 0xd3, 0x79, 0xb9, 0xbe, 0xdb, 0xf3, 0xb5, 0x08, 0xa9, 0x6b, 0xd7, 0xf9, 0x22,
            0xd4, 0xcb, 0x24, 0xec,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Report,
        bytes: 23203,
        sha256: [
            0x7a, 0xad, 0x08, 0xc5, 0x1e, 0x33, 0xc6, 0x7a, 0x75, 0xc4, 0xb7, 0x57, 0x5f, 0xe3,
            0xe8, 0x8f, 0x3f, 0x60, 0x40, 0xa5, 0x9e, 0x79, 0x4d, 0xe8, 0xc7, 0xf6, 0xed, 0x7f,
            0x75, 0x70, 0x61, 0x10,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Book,
        bytes: 23268,
        sha256: [
            0xb7, 0xd1, 0x7b, 0x9b, 0x72, 0xb7, 0x5b, 0xbb, 0x79, 0x7c, 0x8e, 0x46, 0x87, 0x1d,
            0xab, 0xca, 0x15, 0x64, 0x92, 0xed, 0x6e, 0x45, 0x15, 0xc7, 0xbb, 0x86, 0xe5, 0x73,
            0xf1, 0x20, 0x8a, 0x91,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Size10,
        bytes: 8448,
        sha256: [
            0x28, 0x96, 0x16, 0x2d, 0xd5, 0xed, 0x89, 0x46, 0x89, 0xca, 0x1f, 0x33, 0x6f, 0xe2,
            0xe0, 0x84, 0x00, 0x71, 0x14, 0xdc, 0x07, 0xf8, 0xab, 0x17, 0x20, 0x32, 0x81, 0x91,
            0x14, 0x79, 0x54, 0x8f,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Bk10,
        bytes: 8288,
        sha256: [
            0x48, 0xf2, 0xef, 0xca, 0x47, 0xa5, 0xbc, 0x27, 0xa5, 0x25, 0x29, 0x9b, 0x43, 0x93,
            0x2f, 0x7e, 0xfe, 0xc6, 0xa6, 0x12, 0x14, 0x24, 0x15, 0xfe, 0xe0, 0xf4, 0xe6, 0x31,
            0x2a, 0xe9, 0x31, 0x4d,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amsmath,
        bytes: 88471,
        sha256: [
            0x4c, 0x0b, 0xfd, 0x7d, 0x67, 0xea, 0x18, 0x21, 0x48, 0x10, 0xa2, 0x12, 0x44, 0xf8,
            0x42, 0x6d, 0xa4, 0x0d, 0x04, 0xaf, 0x5b, 0xb9, 0x0e, 0xd2, 0x01, 0xb8, 0x5c, 0xc5,
            0xe2, 0x11, 0xd3, 0x21,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amstext,
        bytes: 2444,
        sha256: [
            0x7a, 0x6d, 0x33, 0xe9, 0x5e, 0x16, 0x8f, 0x90, 0xce, 0xc6, 0xd4, 0x17, 0x2a, 0xea,
            0xe7, 0xda, 0x88, 0x59, 0x24, 0xa8, 0xf5, 0x87, 0xcc, 0xc1, 0x18, 0x17, 0xe1, 0xda,
            0xb4, 0xd5, 0x0b, 0xa9,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amsbsy,
        bytes: 2222,
        sha256: [
            0xc0, 0x9b, 0x1b, 0x14, 0xdf, 0xe8, 0xa8, 0x1b, 0x67, 0x91, 0x67, 0x3b, 0x3b, 0xc5,
            0xa7, 0xfd, 0x8b, 0x79, 0x4f, 0xb0, 0x63, 0xb6, 0xe8, 0xb5, 0xce, 0x79, 0xfc, 0x28,
            0xc7, 0xe5, 0x9c, 0x8c,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amsopn,
        bytes: 4474,
        sha256: [
            0xc5, 0x02, 0x39, 0xa5, 0xac, 0xc1, 0x03, 0x7f, 0xf4, 0xdf, 0x48, 0x28, 0x78, 0xe5,
            0x54, 0x6b, 0xe3, 0xcd, 0x9b, 0x8a, 0xc7, 0x25, 0x15, 0x1b, 0xc1, 0x47, 0x48, 0xa1,
            0x03, 0xd8, 0x00, 0xa8,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amsgen,
        bytes: 4173,
        sha256: [
            0xe7, 0xef, 0x67, 0x7a, 0x56, 0xe2, 0x2d, 0x8d, 0xa9, 0x15, 0xe6, 0x13, 0xc6, 0x5f,
            0xe2, 0xd4, 0xc2, 0xfe, 0x87, 0x44, 0xff, 0xb2, 0x83, 0xc2, 0xcc, 0xfd, 0xfc, 0x06,
            0x6e, 0x45, 0xe1, 0x58,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Array,
        bytes: 15651,
        sha256: [
            0x53, 0x70, 0x13, 0xde, 0x60, 0xc6, 0x60, 0x6c, 0xae, 0xcf, 0xc2, 0xee, 0xe9, 0xb8,
            0x1c, 0xd9, 0x5f, 0x09, 0x3e, 0xbb, 0x0f, 0x99, 0xb4, 0x32, 0x77, 0x77, 0x50, 0x73,
            0xf0, 0x0d, 0x91, 0x60,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Article,
        bytes: 20144,
        sha256: [
            0xc0, 0x9f, 0x59, 0x82, 0x89, 0x3f, 0x8a, 0x09, 0x2e, 0x69, 0x43, 0x5c, 0xaa, 0x15,
            0x1e, 0xef, 0x0d, 0x3c, 0xb4, 0xd7, 0x49, 0x5a, 0xbf, 0x23, 0xf0, 0x8b, 0x64, 0x01,
            0x91, 0xcf, 0x81, 0xf6,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Report,
        bytes: 23203,
        sha256: [
            0x15, 0xe8, 0x27, 0x45, 0x4a, 0xd6, 0x31, 0x6c, 0xf9, 0xa7, 0xec, 0xee, 0xa4, 0xcf,
            0xdb, 0x7c, 0x28, 0xc4, 0x42, 0x8b, 0x69, 0x07, 0xf0, 0x1c, 0x74, 0x2f, 0x5d, 0xd8,
            0x6f, 0xa3, 0x27, 0x90,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Book,
        bytes: 23268,
        sha256: [
            0xff, 0x9d, 0xcf, 0xb5, 0x36, 0x55, 0x4e, 0x5a, 0xfd, 0x36, 0xba, 0x4d, 0xd1, 0xfb,
            0x8f, 0x15, 0xac, 0x63, 0x4c, 0x4e, 0xf1, 0x08, 0xb0, 0x53, 0x14, 0x75, 0x52, 0x3f,
            0x0d, 0x5d, 0x67, 0x62,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Size10,
        bytes: 8448,
        sha256: [
            0x7a, 0x5a, 0xb9, 0xaa, 0x17, 0x94, 0xb8, 0x86, 0x4e, 0x29, 0x63, 0xdb, 0x21, 0x1c,
            0xa0, 0x3b, 0x48, 0xa4, 0x55, 0xc7, 0x06, 0x19, 0x0c, 0x7f, 0x85, 0x12, 0x4c, 0x6c,
            0xa9, 0x71, 0x41, 0x83,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Bk10,
        bytes: 8288,
        sha256: [
            0xc4, 0x4c, 0xa4, 0xb6, 0x7c, 0xc1, 0xff, 0xe7, 0xd1, 0x5b, 0x3e, 0x3d, 0x83, 0x8f,
            0x56, 0x8c, 0x64, 0xbd, 0xb8, 0xe9, 0xd7, 0x83, 0x76, 0x7c, 0x10, 0x99, 0x65, 0x3c,
            0x1e, 0xae, 0xbb, 0xe3,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amsmath,
        bytes: 88826,
        sha256: [
            0x8b, 0xf3, 0x51, 0x30, 0xbe, 0x39, 0xe1, 0x23, 0xcb, 0xd0, 0xe4, 0xf0, 0xc0, 0x9d,
            0x38, 0x06, 0xd7, 0x45, 0xd4, 0xc0, 0x8e, 0xe3, 0x91, 0xa5, 0x43, 0xa8, 0xb9, 0x67,
            0x51, 0x91, 0xe6, 0xa7,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amstext,
        bytes: 2444,
        sha256: [
            0xaa, 0x44, 0x58, 0xf9, 0xb4, 0xa5, 0xe6, 0xc1, 0xa7, 0x91, 0x72, 0xfd, 0x67, 0x0b,
            0xce, 0xfb, 0xe7, 0x17, 0xf3, 0xaf, 0x0d, 0xfc, 0x9c, 0x18, 0xb9, 0x5b, 0xc3, 0x4b,
            0x60, 0xaf, 0xce, 0x7b,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amsbsy,
        bytes: 2222,
        sha256: [
            0x93, 0x8d, 0x91, 0x49, 0xd3, 0x11, 0xb3, 0xa8, 0x88, 0x36, 0x22, 0x0b, 0xc7, 0x6b,
            0x05, 0x77, 0x39, 0x95, 0x87, 0x04, 0xa6, 0x54, 0x22, 0xbe, 0xe0, 0x52, 0x69, 0x63,
            0x29, 0xfd, 0x41, 0xf8,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amsopn,
        bytes: 4474,
        sha256: [
            0x2d, 0x8d, 0xfd, 0x81, 0xcd, 0xb5, 0xee, 0xff, 0xc4, 0xf0, 0x3b, 0x64, 0xf5, 0x75,
            0x48, 0x08, 0xc8, 0x09, 0xfb, 0x72, 0x9d, 0x88, 0x3f, 0xd4, 0xa9, 0x4f, 0xac, 0xad,
            0xa0, 0xd6, 0x37, 0x65,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amsgen,
        bytes: 4173,
        sha256: [
            0xf7, 0x8f, 0xa5, 0x73, 0xb1, 0x59, 0x6e, 0xa6, 0x58, 0x95, 0x70, 0xf3, 0xc3, 0x57,
            0xb6, 0x8a, 0x51, 0x1f, 0xb8, 0x05, 0x06, 0x07, 0xa3, 0xc8, 0x98, 0xb2, 0x5b, 0xc7,
            0x28, 0x57, 0x1a, 0x9e,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Array,
        bytes: 17611,
        sha256: [
            0xed, 0x9d, 0x9f, 0x02, 0x35, 0xc3, 0xdb, 0x06, 0xb5, 0x0c, 0x4b, 0xa7, 0x88, 0xf8,
            0x7b, 0xbc, 0x05, 0x22, 0x9f, 0x59, 0x09, 0x62, 0x62, 0x41, 0xc0, 0xee, 0x49, 0xd9,
            0x48, 0x16, 0x77, 0xbc,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Booktabs,
        bytes: 6078,
        sha256: [
            0x3f, 0xe6, 0x94, 0xa5, 0x40, 0x6f, 0x84, 0x84, 0x71, 0x43, 0xe5, 0x6b, 0xa1, 0x84,
            0x13, 0x85, 0x36, 0x42, 0x77, 0xcf, 0xe7, 0x69, 0x4a, 0x9f, 0x4b, 0xc0, 0x07, 0x2c,
            0xa8, 0x65, 0x6a, 0xbc,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amsfonts,
        bytes: 5949,
        sha256: [
            0xda, 0xb8, 0xb0, 0xe6, 0x21, 0x26, 0x7a, 0xcf, 0xd8, 0x97, 0x36, 0xcf, 0x97, 0xbd,
            0xcc, 0x02, 0x64, 0x7c, 0x54, 0x5c, 0x07, 0x19, 0xc4, 0x99, 0x7b, 0xe8, 0xa7, 0xd1,
            0xbf, 0x98, 0xf9, 0xc3,
        ],
    },
    KnownSource {
        identity: InstalledSourceId::Amssymb,
        bytes: 13829,
        sha256: [
            0x70, 0x83, 0x8b, 0x06, 0x1b, 0x56, 0x56, 0x9d, 0xd3, 0xed, 0x9f, 0x33, 0x9b, 0x1b,
            0xdd, 0x1c, 0x78, 0xba, 0x18, 0x5d, 0xe4, 0x9f, 0x27, 0xce, 0xae, 0x33, 0x1c, 0x97,
            0xf4, 0x8b, 0x59, 0x86,
        ],
    },
];

/// Whether the byte catalog can possibly prove this exact loader filename.
pub fn has_profile_for_name(name: &str) -> bool {
    KNOWN
        .iter()
        .any(|known| profile(known.identity).name == name)
}

/// Recognize exact pinned UTF-8 source bytes. File length cheaply rejects
/// unrelated documents before hashing; no parser or filesystem is consulted.
pub fn identify_source(text: &str) -> Option<InstalledSourceId> {
    if !KNOWN.iter().any(|known| known.bytes == text.len()) {
        return None;
    }
    let digest: [u8; 32] = Sha256::digest(text.as_bytes()).into();
    KNOWN
        .iter()
        .find(|known| known.bytes == text.len() && known.sha256 == digest)
        .map(|known| known.identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::{IncrementalDatabase, QueryKind};
    use std::path::Path;

    const FIXTURES: &[(&str, InstalledSourceId)] = &[
        (
            include_str!("../../tests/fixtures/installed-sources/2025/article.cls"),
            InstalledSourceId::Article,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2025/report.cls"),
            InstalledSourceId::Report,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2025/book.cls"),
            InstalledSourceId::Book,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2025/size10.clo"),
            InstalledSourceId::Size10,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2025/bk10.clo"),
            InstalledSourceId::Bk10,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2025/amsmath.sty"),
            InstalledSourceId::Amsmath,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2025/amstext.sty"),
            InstalledSourceId::Amstext,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2025/amsbsy.sty"),
            InstalledSourceId::Amsbsy,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2025/amsopn.sty"),
            InstalledSourceId::Amsopn,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2025/amsgen.sty"),
            InstalledSourceId::Amsgen,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2025/array.sty"),
            InstalledSourceId::Array,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/article.cls"),
            InstalledSourceId::Article,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/report.cls"),
            InstalledSourceId::Report,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/book.cls"),
            InstalledSourceId::Book,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/size10.clo"),
            InstalledSourceId::Size10,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/bk10.clo"),
            InstalledSourceId::Bk10,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/amsmath.sty"),
            InstalledSourceId::Amsmath,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/amstext.sty"),
            InstalledSourceId::Amstext,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/amsbsy.sty"),
            InstalledSourceId::Amsbsy,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/amsopn.sty"),
            InstalledSourceId::Amsopn,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/amsgen.sty"),
            InstalledSourceId::Amsgen,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/2026/array.sty"),
            InstalledSourceId::Array,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/common/booktabs.sty"),
            InstalledSourceId::Booktabs,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/common/amsfonts.sty"),
            InstalledSourceId::Amsfonts,
        ),
        (
            include_str!("../../tests/fixtures/installed-sources/common/amssymb.sty"),
            InstalledSourceId::Amssymb,
        ),
    ];

    #[test]
    fn pinned_sources_recognize_exact_bytes_and_reject_same_length_edits() {
        for &(source, expected) in FIXTURES {
            assert_eq!(identify_source(source), Some(expected));
            let mut changed = source.to_owned();
            let at = changed.find("Provides").unwrap();
            changed.replace_range(at..at + 1, "X");
            assert_eq!(changed.len(), source.len());
            assert_eq!(identify_source(&changed), None);
            assert_eq!(identify_source(&format!("{source}\n")), None);
        }
        assert_eq!(
            identify_source(r"\ProvidesClass{article}[2025/01/22 v1.4n]"),
            None
        );
    }

    #[test]
    fn identities_are_cached_per_source_and_do_not_establish_overlay_provenance() {
        let source = include_str!("../../tests/fixtures/installed-sources/2025/article.cls");
        let path = Path::new("/arbitrary-name/source.tex");
        let mut db = IncrementalDatabase::default();
        let file = db.apply_change(path, source, None);
        db.clear_query_log();
        for _ in 0..8 {
            assert_eq!(
                db.installed_source_identity(file),
                Some(InstalledSourceId::Article)
            );
            assert_eq!(
                db.standard_package_source(file).unwrap().name,
                "article.cls"
            );
        }
        assert_eq!(
            db.query_log()
                .iter()
                .filter(|entry| entry.kind == QueryKind::InstalledSourceIdentity)
                .count(),
            1
        );
        db.clear_query_log();
        let mut changed = source.to_owned();
        let at = changed.find("Provides").unwrap();
        changed.replace_range(at..at + 1, "X");
        db.apply_change(path, changed, None);
        for _ in 0..8 {
            assert!(db.installed_source_identity(file).is_none());
        }
        assert_eq!(
            db.query_log()
                .iter()
                .filter(|entry| entry.kind == QueryKind::InstalledSourceIdentity)
                .count(),
            1
        );
        let project = db.project_id();
        db.apply_change(path, source, None);
        assert!(!db.source_has_overlay(file));
        db.replace_overlay(project, path, source).unwrap();
        assert!(db.source_has_overlay(file));
        assert_eq!(
            db.installed_source_identity(file),
            Some(InstalledSourceId::Article)
        );
        db.close_overlay(project, path).unwrap();
        assert!(!db.source_has_overlay(file));
    }
}
