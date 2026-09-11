//! Native AUX discovery and cache.
use meaning_analysis::project::aux::*;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};
/// One parsed aux file in the server-owned cache, valid while the on-disk
/// `(mtime, len)` still match — the same freshness idea as the TEXMF index's
/// fingerprint, but per file, so a recompile is picked up on the next request
/// without any watcher.
struct CachedFile {
    mtime: SystemTime,
    len: u64,
    parsed: Arc<ParsedAux>,
}

/// Disposable per-server cache of parsed compiler output.
#[derive(Default)]
pub struct AuxCache {
    files: Mutex<HashMap<PathBuf, CachedFile>>,
}
impl AuxCache {
    /// Read and parse `path`, through the cache. `None` when the file is missing,
    /// unreadable, or not a regular file. Non-UTF-8 bytes (legacy inputenc) are
    /// replaced lossily — keys and numbers are ASCII in practice.
    fn read_aux(&self, path: &Path) -> Option<Arc<ParsedAux>> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let mtime = meta.modified().ok()?;
        let len = meta.len();
        if let Ok(map) = self.files.lock()
            && let Some(cached) = map.get(path)
            && cached.mtime == mtime
            && cached.len == len
        {
            return Some(Arc::clone(&cached.parsed));
        }
        let bytes = std::fs::read(path).ok()?;
        let parsed = Arc::new(parse_aux(&String::from_utf8_lossy(&bytes)));
        if let Ok(mut map) = self.files.lock() {
            map.insert(
                path.to_owned(),
                CachedFile {
                    mtime,
                    len,
                    parsed: Arc::clone(&parsed),
                },
            );
        }
        Some(parsed)
    }

    /// The merged aux facts for a document's label namespace, or `None` when no
    /// `.aux` exists (an uncompiled project — features degrade to numberless).
    ///
    /// For each namespace member `foo.tex` the candidates are, first hit wins:
    /// its sibling `foo.aux`; with `aux_dir` configured (resolved against
    /// `root_dir`, the root document's directory, when relative), the member's
    /// root-relative path under it (latexmk `-auxdir` layout), then flat
    /// `{aux_dir}/foo.aux`. Each found file's `\@input` chain is followed
    /// (relative to that aux file's directory), so `\include`'s per-chapter aux
    /// files surface even when the namespace is incomplete. Label conflicts keep
    /// the first number seen; toc entries concatenate in traversal order.
    pub fn data_for(
        &self,
        namespace: &[&Path],
        root_dir: &Path,
        aux_dir: Option<&Path>,
    ) -> Option<AuxData> {
        let base = aux_dir.map(|dir| {
            if dir.is_absolute() {
                dir.to_owned()
            } else {
                root_dir.join(dir)
            }
        });

        let mut merged = AuxData::default();
        let mut visited: HashSet<PathBuf> = HashSet::new();
        for member in namespace {
            if member.extension().and_then(|e| e.to_str()) != Some("tex") {
                continue;
            }
            let mut candidates = vec![member.with_extension("aux")];
            if let Some(base) = &base {
                if let Ok(rel) = member.strip_prefix(root_dir) {
                    candidates.push(base.join(rel).with_extension("aux"));
                }
                if let Some(name) = member.file_name() {
                    candidates.push(base.join(name).with_extension("aux"));
                }
            }
            if let Some(found) = candidates.iter().find(|c| c.is_file()) {
                self.merge_chain(found, &mut merged, &mut visited);
            }
        }
        (!merged.labels.is_empty() || !merged.toc.is_empty()).then_some(merged)
    }

    /// Merge AUX input graphs in source order, visiting each real file once.
    fn merge_chain(&self, path: &Path, out: &mut AuxData, visited: &mut HashSet<PathBuf>) {
        let mut pending = vec![path.to_owned()];
        while let Some(path) = pending.pop() {
            let Ok(path) = path.canonicalize() else {
                continue;
            };
            if !visited.insert(path.clone()) {
                continue;
            }
            let Some(parsed) = self.read_aux(&path) else {
                continue;
            };
            for (key, number) in &parsed.data.labels {
                out.labels
                    .entry(key.clone())
                    .or_insert_with(|| number.clone());
            }
            out.toc.extend(parsed.data.toc.iter().cloned());
            let dir = path.parent().unwrap_or(Path::new(""));
            pending.extend(parsed.inputs.iter().rev().map(|target| dir.join(target)));
        }
    }
}

#[cfg(test)]
use smol_str::SmolStr;

#[cfg(test)]
mod tests {
    use super::*;

    fn labels_of(text: &str) -> HashMap<SmolStr, String> {
        parse_aux(text).data.labels
    }

    #[test]
    fn simple_newlabel() {
        let labels = labels_of("\\newlabel{sec:foo}{{1}{1}}\n");
        assert_eq!(labels[&SmolStr::new("sec:foo")], "1");
    }

    #[test]
    fn newlabel_with_caption_and_anchor_groups() {
        let labels = labels_of("\\newlabel{thm:foo}{{1}{1}{Foo}{lemma.1}{}}\n");
        assert_eq!(labels[&SmolStr::new("thm:foo")], "1");
    }

    #[test]
    fn ntheorem_nested_braces_flatten() {
        let labels = labels_of("\\newlabel{thm:test}{{1.{1}}{1}}\n");
        assert_eq!(labels[&SmolStr::new("thm:test")], "1.1");
    }

    #[test]
    fn caption_xref_group_is_skipped_for_the_page_number() {
        // The first group holds a command, not text; the next textual group wins
        // (texlab behavior).
        let labels =
            labels_of("\\newlabel{fig:qux}{{\\caption@xref {fig:qux}{ on input line 15}}{1}}\n");
        assert_eq!(labels[&SmolStr::new("fig:qux")], "1");
    }

    #[test]
    fn empty_groups_are_skipped() {
        let labels = labels_of("\\newlabel{x}{{}{2}}\n");
        assert_eq!(labels[&SmolStr::new("x")], "2");
    }

    #[test]
    fn newlabel_without_number_is_absent() {
        let labels = labels_of("\\newlabel{x}{{}{}}\n\\newlabel{y}{{\\relax}{}}\n");
        assert!(labels.is_empty());
    }

    #[test]
    fn truncated_entry_is_skipped_earlier_entries_survive() {
        let labels = labels_of("\\newlabel{a}{{1}{1}}\n\\newlabel{b}{{2}{1}\n");
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[&SmolStr::new("a")], "1");
    }

    #[test]
    fn similarly_named_commands_do_not_match() {
        let parsed = parse_aux("\\newlabelx{a}{{1}{1}}\n\\@inputonce{f.aux}\n");
        assert!(parsed.data.labels.is_empty());
        assert!(parsed.inputs.is_empty());
    }

    #[test]
    fn at_input_targets_collected_in_order() {
        let parsed = parse_aux("\\@input{ch1.aux}\n\\relax\n\\@input{ch2.aux}\n");
        assert_eq!(parsed.inputs, vec!["ch1.aux", "ch2.aux"]);
    }

    #[test]
    fn toc_contentsline_with_numberline() {
        let toc = parse_aux(
            "\\@writefile{toc}{\\contentsline {section}{\\numberline {1.2}Basics}{3}{section.1.2}}\n",
        )
        .data
        .toc;
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].level, "section");
        assert_eq!(toc[0].number.as_deref(), Some("1.2"));
        assert_eq!(toc[0].title, "Basics");
    }

    #[test]
    fn starred_contentsline_has_no_number() {
        let toc =
            parse_aux("\\@writefile{toc}{\\contentsline {section}{Preface}{1}{section*.1}}\n")
                .data
                .toc;
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].number, None);
        assert_eq!(toc[0].title, "Preface");
    }

    #[test]
    fn title_keeps_macro_source() {
        let toc = parse_aux(
            "\\@writefile{toc}{\\contentsline {section}{\\numberline {1}\\textsc  {Intro}}{1}{section.1}}\n",
        )
        .data
        .toc;
        assert_eq!(toc[0].title, "\\textsc  {Intro}");
    }

    #[test]
    fn non_toc_writefile_streams_are_ignored() {
        let parsed = parse_aux(
            "\\@writefile{lof}{\\contentsline {figure}{\\numberline {1}{\\ignorespaces A chart}}{2}{figure.1}}\n",
        );
        assert!(parsed.data.toc.is_empty());
    }

    #[test]
    fn escaped_braces_do_not_unbalance_groups() {
        let labels = labels_of("\\newlabel{k}{{1}{1}{a \\{b\\} c}{s.1}{}}\n");
        assert_eq!(labels[&SmolStr::new("k")], "1");
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn sibling_aux_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "\\documentclass{article}\n");
        write(&dir.path().join("main.aux"), "\\newlabel{sec:a}{{1}{1}}\n");
        let data = AuxCache::default()
            .data_for(&[&main], dir.path(), None)
            .expect("aux found");
        assert_eq!(data.labels[&SmolStr::new("sec:a")], "1");
    }

    #[test]
    fn missing_aux_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "x\n");
        assert_eq!(
            AuxCache::default().data_for(&[&main], dir.path(), None),
            None
        );
    }

    #[test]
    fn aux_dir_candidates_flat_and_relative() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        let chap = dir.path().join("chapters/one.tex");
        write(&main, "x\n");
        write(&chap, "x\n");
        // latexmk -auxdir layout: root flat, includes under their relative path.
        write(
            &dir.path().join("build/main.aux"),
            "\\newlabel{sec:root}{{1}{1}}\n",
        );
        write(
            &dir.path().join("build/chapters/one.aux"),
            "\\newlabel{sec:one}{{2}{3}}\n",
        );
        let data = AuxCache::default()
            .data_for(&[&main, &chap], dir.path(), Some(Path::new("build")))
            .expect("aux found");
        assert_eq!(data.labels[&SmolStr::new("sec:root")], "1");
        assert_eq!(data.labels[&SmolStr::new("sec:one")], "2");
    }

    #[test]
    fn at_input_chain_is_followed_with_cycles_guarded() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "x\n");
        // main.aux → ch1.aux → main.aux (cycle); ch1 is *not* in the namespace.
        write(
            &dir.path().join("main.aux"),
            "\\newlabel{a}{{1}{1}}\n\\@input{ch1.aux}\n",
        );
        write(
            &dir.path().join("ch1.aux"),
            "\\newlabel{b}{{2}{2}}\n\\@input{main.aux}\n",
        );
        let data = AuxCache::default()
            .data_for(&[&main], dir.path(), None)
            .expect("aux found");
        assert_eq!(data.labels.len(), 2);
        assert_eq!(data.labels[&SmolStr::new("b")], "2");
    }

    #[test]
    fn stale_cache_reparses_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "x\n");
        let aux = dir.path().join("main.aux");
        write(&aux, "\\newlabel{a}{{1}{1}}\n");
        let cache = AuxCache::default();
        let first = cache
            .data_for(&[&main], dir.path(), None)
            .expect("aux found");
        assert_eq!(first.labels[&SmolStr::new("a")], "1");

        // A recompile rewrites the file; ensure the timestamp moves even on
        // coarse-mtime filesystems.
        let text = "\\newlabel{a}{{7}{1}}\n";
        write(&aux, text);
        let old = std::fs::metadata(&aux).unwrap().modified().unwrap();
        std::fs::File::options()
            .append(true)
            .open(&aux)
            .unwrap()
            .set_modified(old + std::time::Duration::from_secs(2))
            .unwrap();

        let second = cache
            .data_for(&[&main], dir.path(), None)
            .expect("aux found");
        assert_eq!(second.labels[&SmolStr::new("a")], "7");
    }

    #[test]
    fn deep_aux_graphs_and_path_alias_cycles_are_complete() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "x");
        write(&dir.path().join("main.aux"), "\\@input{0.aux}\n");
        for n in 0..40 {
            let next = if n == 39 {
                "./main.aux".to_owned()
            } else {
                format!("{}.aux", n + 1)
            };
            write(
                &dir.path().join(format!("{n}.aux")),
                &format!("\\newlabel{{key{n}}}{{{{{n}}}{{1}}}}\n\\@input{{{next}}}\n"),
            );
        }
        let data = AuxCache::default()
            .data_for(&[&main], dir.path(), None)
            .unwrap();
        assert_eq!(data.labels.len(), 40);
        assert_eq!(data.labels[&SmolStr::new("key39")], "39");
    }

    #[test]
    fn non_tex_members_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let bib = dir.path().join("refs.bib");
        write(&bib, "@article{a}\n");
        write(&dir.path().join("refs.aux"), "\\newlabel{x}{{1}{1}}\n");
        assert_eq!(
            AuxCache::default().data_for(&[&bib], dir.path(), None),
            None
        );
    }

    #[test]
    fn realistic_aux_mix() {
        let text = "\\relax \n\
                    \\providecommand\\hyper@newdestlabel[2]{}\n\
                    \\@writefile{toc}{\\contentsline {section}{\\numberline {1}Intro}{1}{section.1}}\n\
                    \\newlabel{sec:intro}{{1}{1}{Intro}{section.1}{}}\n\
                    \\@writefile{lof}{\\contentsline {figure}{\\numberline {1}{\\ignorespaces X}}{2}{figure.1}}\n\
                    \\newlabel{fig:x}{{1}{2}{X}{figure.1}{}}\n\
                    \\@input{ch1.aux}\n\
                    \\citation{knuth1984}\n\
                    \\gdef \\@abspage@last{3}\n";
        let parsed = parse_aux(text);
        assert_eq!(parsed.data.labels.len(), 2);
        assert_eq!(parsed.data.labels[&SmolStr::new("sec:intro")], "1");
        assert_eq!(parsed.data.labels[&SmolStr::new("fig:x")], "1");
        assert_eq!(parsed.data.toc.len(), 1);
        assert_eq!(parsed.inputs, vec!["ch1.aux"]);
    }
}
