use meaning_parser::{bib, parser::Edit};

#[test]
fn field_edits_preserve_errors_and_unicode_offsets() {
    for source in [
        "@article{a,ti}\r\n@article{b,title={😀}}",
        "@article{a,title={A word},year=2025}",
        "@string{journal = other}\n@article{b,title=journal}",
        "free text\n@article{a, title = \"text\"}\n",
        "@article{a,ti\n@article{b,au}",
    ] {
        let base = bib::parse(source);
        let mut landed = 0;
        for (at, ch) in source.char_indices() {
            for insert in ["x", "😀", "123", "", "\n", "@", "{", "\"", "%", "title"] {
                let edit = Edit {
                    range: at..at + ch.len_utf8(),
                    insert: insert.into(),
                };
                let text = edit.apply(source);
                if let Some(reparsed) = bib::reparse::reparse(source, &base, &edit, &text) {
                    let full = bib::parse(&text);
                    assert_eq!(reparsed.green, full.green, "{source:?} {edit:?}");
                    assert_eq!(reparsed.errors, full.errors, "{source:?} {edit:?}");
                    assert_eq!(reparsed.syntax().to_string(), text);
                    landed += 1;
                }
            }
        }
        assert!(landed > 10, "{source:?}: {landed}");
    }
}

#[test]
fn entry_type_and_stale_transform_decline() {
    let source = "@article{probe,ti}";
    let base = bib::parse(source);
    let edit = Edit {
        range: 1..8,
        insert: "comment".into(),
    };
    assert!(bib::reparse::reparse(source, &base, &edit, &edit.apply(source)).is_none());
    let edit = Edit {
        range: 15..17,
        insert: "title".into(),
    };
    assert!(bib::reparse::reparse(source, &base, &edit, "unrelated").is_none());
}

#[test]
fn large_field_splice_matches_full_parse() {
    let source =
        "@article{key,title={Research},year=2025}\n".repeat(20000) + "@article{probe,ti}\n";
    let base = bib::parse(&source);
    let at = source.rfind("ti}").unwrap();
    let edit = Edit {
        range: at..at + 2,
        insert: "author".into(),
    };
    let text = edit.apply(&source);
    let result = bib::reparse::reparse(&source, &base, &edit, &text).expect("leaf tier");
    let full = bib::parse(&text);
    assert_eq!(result.green, full.green);
    assert_eq!(result.errors, full.errors);
}

#[test]
fn seeded_bibliography_corpus_matches_full_parse() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bib_corpus");
    let mut paths = std::fs::read_dir(directory)
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "bib"))
        .collect::<Vec<_>>();
    paths.sort();
    let mut tally = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed = bib::parse(&text);
        let boundaries = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .collect::<Vec<_>>();
        let mut seed = 0x19283947u64;
        let mut spliced = 0;
        for i in 0..200 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let index = seed as usize % (boundaries.len() - 1);
            let insert = ["x", "word", "123", "😀", "@", "{", "}", "\n", "", "\""][i % 10];
            let edit = Edit {
                range: boundaries[index]..boundaries[index + usize::from(i % 3 != 0)],
                insert: insert.into(),
            };
            let next = edit.apply(&text);
            if let Some(result) = bib::reparse::reparse(&text, &parsed, &edit, &next) {
                let full = bib::parse(&next);
                assert_eq!(result.green, full.green, "{path:?} {edit:?}");
                assert_eq!(result.errors, full.errors, "{path:?} {edit:?}");
                spliced += 1;
            }
        }
        assert!(
            spliced >= 10,
            "{} only {spliced}/200 splices",
            path.display()
        );
        tally.push(format!(
            "{} {spliced}/200",
            path.file_name().unwrap().to_string_lossy()
        ));
    }
    let actual = tally.join("\n") + "\n";
    let baseline =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bib_reparse_baseline.txt");
    if std::env::var_os("MEANING_RECORD_BIB_REPARSE").is_some() {
        std::fs::write(&baseline, &actual).unwrap();
    }
    assert_eq!(actual, std::fs::read_to_string(baseline).unwrap());
}
