//! Pure, conservative readers for last-build compiler artifacts.
use crate::source::normalize_path;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
    Information,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub path: Option<PathBuf>,
    /// One-based compiler line, absent when the log does not identify one.
    pub line: Option<u32>,
    pub level: Level,
    pub message: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Recorder {
    pub directory: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub outputs: Vec<PathBuf>,
}

pub fn parse_recorder(text: &str, directory: &Path) -> Recorder {
    let mut result = Recorder {
        directory: directory.into(),
        ..Default::default()
    };
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("PWD ") {
            if Path::new(path).is_absolute() {
                result.directory = normalize_path(Path::new(path));
            }
        } else if let Some(path) = line.strip_prefix("INPUT ") {
            result
                .inputs
                .push(normalize_path(&result.directory.join(path)));
        } else if let Some(path) = line.strip_prefix("OUTPUT ") {
            result
                .outputs
                .push(normalize_path(&result.directory.join(path)));
        }
    }
    result.inputs.sort();
    result.inputs.dedup();
    result.outputs.sort();
    result.outputs.dedup();
    result
}

/// Recognize file:line errors, TeX error/line pairs, and explicit warning lines.
/// Unattributed messages retain no source rather than guessing a compilation root.
pub fn parse_log(text: &str, directory: &Path) -> Vec<Message> {
    let mut messages: Vec<Message> = Vec::new();
    let mut files: Vec<Option<PathBuf>> = Vec::new();
    for line in text.lines() {
        // TeX's parenthesized file stack. Only source-looking tokens establish a file;
        // message parentheses cannot create a spurious source attribution.
        if !line.starts_with(['!', ' '])
            && !line.starts_with("l.")
            && !line.contains("Warning:")
            && !line.starts_with("Overfull ")
            && !line.starts_with("Underfull ")
        {
            for (at, c) in line.char_indices() {
                if c == '(' {
                    let name = line[at + 1..]
                        .split([' ', '\t', '(', ')'])
                        .next()
                        .unwrap_or("");
                    let source = [
                        ".tex", ".sty", ".cls", ".def", ".lco", ".cfg", ".ldf", ".fd",
                    ]
                    .iter()
                    .any(|ext| name.ends_with(ext));
                    files.push(source.then(|| normalize_path(&directory.join(name))));
                } else if c == ')' {
                    files.pop();
                }
            }
        }
        let mut explicit = None;
        // Search colon boundaries so Windows drive letters do not consume the line.
        for (at, _) in line.match_indices(':') {
            if let Some((number, message)) = line[at + 1..].split_once(':')
                && let Ok(number) = number.parse::<u32>()
                && number > 0
                && !line[..at].trim().is_empty()
            {
                explicit = Some(Message {
                    path: Some(normalize_path(&directory.join(line[..at].trim()))),
                    line: Some(number),
                    level: Level::Error,
                    message: message.trim().into(),
                    hint: None,
                });
                break;
            }
        }
        if let Some(message) = explicit {
            messages.push(message);
        } else if let Some(message) = line.strip_prefix("! ") {
            messages.push(Message {
                path: files.last().cloned().flatten(),
                line: None,
                level: Level::Error,
                message: message.into(),
                hint: None,
            });
        } else if let Some(rest) = line.strip_prefix("l.") {
            if let Some(last) = messages
                .last_mut()
                .filter(|m| m.level == Level::Error && m.line.is_none())
            {
                last.hint = rest
                    .split_once(char::is_whitespace)
                    .map(|(_, hint)| hint.trim().to_owned())
                    .filter(|hint| !hint.is_empty());
                last.line = rest
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .filter(|n| *n > 0);
            }
        } else if line.contains("Warning:")
            || line.starts_with("Overfull ")
            || line.starts_with("Underfull ")
        {
            let number = line
                .rsplit_once("on input line ")
                .and_then(|(_, n)| n.trim_end_matches('.').parse().ok());
            messages.push(Message {
                path: files.last().cloned().flatten(),
                line: number,
                level: Level::Warning,
                message: line.trim().into(),
                hint: None,
            });
        }
    }
    messages
}

/// External findings are selected structurally, independently of native lint IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct Filter {
    pub sources: Vec<String>,
    pub severities: Vec<Level>,
}
impl Default for Filter {
    fn default() -> Self {
        Self {
            sources: vec!["compiler".into()],
            severities: vec![Level::Error, Level::Warning, Level::Information],
        }
    }
}
impl Filter {
    pub fn accepts(&self, source: &str, level: Level) -> bool {
        self.sources.iter().any(|name| name == source) && self.severities.contains(&level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_lines_stack_and_recorder_are_loss_tolerant() {
        let root = Path::new(if cfg!(windows) { "C:/" } else { "/" });
        let project = root.join("p");
        let build = root.join("build");
        let log = parse_log(
            "(./main.tex\n(./child.tex\n! Undefined control sequence.\nl.8 \\oops\n)\nLaTeX Warning: Something on input line 9.\n./other.tex:12: Bad input\n! Partial",
            &project,
        );
        assert_eq!(log[0].path, Some(project.join("child.tex")));
        assert_eq!(log[0].line, Some(8));
        assert_eq!(log[1].path, Some(project.join("main.tex")));
        assert_eq!(log[2].line, Some(12));
        assert_eq!(log[3].line, None);
        let fls = parse_recorder(
            &format!(
                "PWD {}\nINPUT a b.tex\nINPUT a b.tex\nOUTPUT out.pdf\nINPUT ../p/main.tex",
                build.display()
            ),
            &project,
        );
        assert_eq!(
            fls.inputs,
            vec![build.join("a b.tex"), project.join("main.tex")]
        );
        assert_eq!(fls.outputs, vec![build.join("out.pdf")]);
    }
}
