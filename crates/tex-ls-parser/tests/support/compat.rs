//! Corpus inputs and line similarity for the two parse compatibility reports.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Length of the longest common subsequence of two line slices. Used by
/// the differential parse gauge.
pub fn lcs_len(a: &[String], b: &[String]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    for line_a in a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, line_b) in b.iter().enumerate() {
            cur[j + 1] = if line_a == line_b {
                prev[j] + 1
            } else {
                cur[j].max(prev[j + 1])
            };
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Dice coefficient over skeleton lines: `2·LCS / (|a| + |b|)`, in `0.0..=1.0`.
pub fn dice(a: &[String], b: &[String]) -> f64 {
    let denom = a.len() + b.len();
    if denom == 0 {
        return 1.0;
    }
    2.0 * lcs_len(a, b) as f64 / denom as f64
}

/// Enumerate immediate corpus entries with the exact extension, sorted by key and path.
pub fn collect_corpus(dir: &Path, extension: &str) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some(extension) {
                let key = path.file_name().unwrap().to_string_lossy().into_owned();
                files.push((key, path));
            }
        }
    }
    files.sort();
    files
}

/// Read the reports' simple key/reason allowlist syntax. Last matching key wins.
pub fn load_allowlist(path: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"').to_string();
        let value = value.trim().trim_matches('"').to_string();
        map.insert(key, value);
    }
    map
}
