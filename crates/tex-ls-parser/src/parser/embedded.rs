//! Source-only listing settings and bounded foreign lexical profiles.
//!
//! Nothing here depends on installed editor grammars or executes TeX. Unknown
//! option values retain a protected body instead of guessing island ownership.
use std::collections::HashMap;

use smol_str::SmolStr;

use super::lexer::{LexConfig, ParseCtx, Token, lex_with};
use crate::syntax::SyntaxKind as K;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    Listings,
    Minted,
    Fancy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Settings {
    values: HashMap<String, String>,
    uncertain: bool,
}

impl Settings {
    fn update(&mut self, source: &str, styles: &HashMap<String, Settings>) {
        for field in fields(source) {
            let (key, value) = field.split_once('=').unwrap_or((field, "true"));
            let key = key.trim();
            if key.contains('\\') {
                self.uncertain = true;
                continue;
            }
            let value = value.trim();
            let value = if matches!(
                key,
                "style" | "language" | "mathescape" | "texcomments" | "texcl"
            ) {
                unbrace(value)
            } else {
                value
            };
            if key == "style" {
                if let Some(style) = styles.get(value) {
                    self.values.extend(style.values.clone());
                    self.uncertain |= style.uncertain;
                } else {
                    self.uncertain = true;
                }
            } else if matches!(
                key,
                "language"
                    | "escapeinside"
                    | "escapechar"
                    | "mathescape"
                    | "texcomments"
                    | "texcl"
                    | "commandchars"
                    | "commentchar"
            ) {
                self.values.insert(key.to_owned(), value.to_owned());
            }
        }
    }

    fn flag(&self, key: &str) -> bool {
        self.values.get(key).is_some_and(|v| v == "true")
    }

    pub(crate) fn python(&self) -> bool {
        self.values.get("language").is_some_and(|v| {
            v.eq_ignore_ascii_case("python")
                || v.eq_ignore_ascii_case("python3")
                || v.eq_ignore_ascii_case("py")
        })
    }

    fn escapes(&self, family: Family) -> Result<Option<(String, String)>, ()> {
        if self.uncertain {
            return Err(());
        }
        for key in ["mathescape", "texcomments", "texcl"] {
            if self
                .values
                .get(key)
                .is_some_and(|v| !matches!(v.as_str(), "true" | "false"))
            {
                return Err(());
            }
        }
        if let Some(value) = self.values.get("escapeinside") {
            if family == Family::Listings {
                let (a, rest) = argument(value, '{', '}').ok_or(())?;
                let (b, rest) = argument(rest, '{', '}').ok_or(())?;
                if !rest.trim().is_empty() || a.is_empty() || b.is_empty() {
                    return Err(());
                }
                return Ok(Some((
                    control_symbols(a).ok_or(())?,
                    control_symbols(b).ok_or(())?,
                )));
            }
            let value = unbrace(value);
            let decoded = control_symbols(value).ok_or(())?;
            let mut chars = decoded.chars();
            let a = chars.next().ok_or(())?;
            let b = chars.next().ok_or(())?;
            if chars.next().is_some() {
                return Err(());
            }
            return Ok(Some((a.to_string(), b.to_string())));
        }
        if let Some(value) = self.values.get("escapechar") {
            let decoded = control_symbols(unbrace(value)).ok_or(())?;
            if decoded.chars().count() != 1 {
                return Err(());
            }
            return Ok(Some((decoded.clone(), decoded)));
        }
        for key in ["mathescape", "texcomments", "texcl"] {
            if self
                .values
                .get(key)
                .is_some_and(|v| !matches!(v.as_str(), "true" | "false"))
            {
                return Err(());
            }
        }
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spec {
    pub family: Family,
    pub settings: Settings,
    pub delegate: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct State {
    pub environments: HashMap<String, Spec>,
    pub inline: HashMap<String, Spec>,
    pub comments: HashMap<String, Option<bool>>,
    listings: Settings,
    fancy: Settings,
    minted: HashMap<String, Settings>,
    minted_inline: HashMap<String, Settings>,
    styles: HashMap<String, Settings>,
}

pub(crate) fn observes_settings_command(name: &str) -> bool {
    matches!(
        name,
        "lstset"
            | "fvset"
            | "lstdefinestyle"
            | "setminted"
            | "setmintedinline"
            | "includecomment"
            | "excludecomment"
            | "newminted"
            | "newmintinline"
            | "lstnewenvironment"
            | "DefineVerbatimEnvironment"
    )
}

impl State {
    pub fn observe(&mut self, command: &str, after: &str, conditional: bool) {
        match command {
            "\\lstset" | "\\fvset" => {
                let target = if command == "\\lstset" {
                    &mut self.listings
                } else {
                    &mut self.fancy
                };
                if let Some((value, _)) = argument(after, '{', '}') {
                    target.update(value, &self.styles);
                    target.uncertain |= conditional;
                } else {
                    target.uncertain = true;
                }
            }
            "\\lstdefinestyle" => {
                let Some((name, after)) =
                    argument(after, '{', '}').filter(|(name, _)| static_setting_name(name))
                else {
                    for settings in self.styles.values_mut() {
                        settings.uncertain = true;
                    }
                    return;
                };
                let mut settings = Settings::default();
                if let Some((value, _)) = argument(after, '{', '}') {
                    settings.update(value, &self.styles);
                    settings.uncertain |= conditional;
                } else {
                    settings.uncertain = true;
                }
                self.styles.insert(name.to_owned(), settings);
            }
            "\\setminted" | "\\setmintedinline" => {
                let (language, after) = argument(after, '[', ']').unwrap_or(("", after));
                let target = if command == "\\setmintedinline" {
                    &mut self.minted_inline
                } else {
                    &mut self.minted
                };
                if !language.is_empty() && !static_setting_name(language) {
                    target.entry(String::new()).or_default().uncertain = true;
                    return;
                }
                let settings = target.entry(language.to_owned()).or_default();
                if let Some((value, _)) = argument(after, '{', '}') {
                    settings.update(value, &self.styles);
                    settings.uncertain |= conditional;
                } else {
                    settings.uncertain = true;
                }
            }
            "\\includecomment" | "\\excludecomment" => {
                if let Some((name, _)) = argument(after, '{', '}')
                    && literal_name(name)
                {
                    self.comments.insert(
                        name.to_owned(),
                        (!conditional).then_some(command == "\\includecomment"),
                    );
                }
            }
            "\\newminted" | "\\newmintinline" => {
                let (alias, after) = argument(after, '[', ']')
                    .map_or((None, after), |(name, rest)| (Some(name), rest));
                let language_argument = argument(after, '{', '}');
                let declared_name = alias.map(str::to_owned).or_else(|| {
                    language_argument
                        .filter(|(language, _)| literal_name(language))
                        .map(|(language, _)| {
                            format!(
                                "{language}{}",
                                if command == "\\newminted" {
                                    "code"
                                } else {
                                    "inline"
                                }
                            )
                        })
                });
                if !language_argument.is_some_and(|(language, after)| {
                    literal_name(language) && argument(after, '{', '}').is_some()
                }) && let Some(name) = declared_name.filter(|name| literal_name(name))
                {
                    let target = if command == "\\newminted" {
                        &mut self.environments
                    } else {
                        &mut self.inline
                    };
                    mark_existing_spec_uncertain(target, &name);
                }
                if let Some((language, after)) = argument(after, '{', '}')
                    && let Some((options, _)) = argument(after, '{', '}')
                    && literal_name(language)
                {
                    let mut settings = Settings::default();
                    settings.values.insert("language".into(), language.into());
                    settings.update(options, &self.styles);
                    settings.uncertain |= conditional;
                    let name = alias.map(str::to_owned).unwrap_or_else(|| {
                        format!(
                            "{language}{}",
                            if command == "\\newminted" {
                                "code"
                            } else {
                                "inline"
                            }
                        )
                    });
                    if literal_name(&name) {
                        let target = if command == "\\newminted" {
                            &mut self.environments
                        } else {
                            &mut self.inline
                        };
                        target.insert(
                            name.clone(),
                            Spec {
                                family: Family::Minted,
                                settings,
                                delegate: false,
                            },
                        );
                        if command == "\\newminted"
                            && let Some(spec) = target.get(&name).cloned()
                        {
                            target.insert(format!("{name}*"), spec);
                        }
                    }
                }
            }
            "\\lstnewenvironment" | "\\DefineVerbatimEnvironment" => {
                if let Some((name, mut after)) = argument(after, '{', '}')
                    && literal_name(name)
                {
                    let family = if command == "\\lstnewenvironment" {
                        Family::Listings
                    } else {
                        Family::Fancy
                    };
                    if family == Family::Listings {
                        for _ in 0..2 {
                            if let Some((_, rest)) = argument(after, '[', ']') {
                                after = rest;
                            }
                        }
                    } else if let Some((_, rest)) = argument(after, '{', '}') {
                        after = rest;
                    }
                    if let Some((options, _)) = argument(after, '{', '}') {
                        let mut settings = Settings::default();
                        if family == Family::Listings {
                            if let Some(value) = options.trim().strip_prefix("\\lstset")
                                && let Some((value, rest)) = argument(value, '{', '}')
                                && rest.trim().is_empty()
                            {
                                settings.update(value, &self.styles);
                            } else if !options.trim().is_empty() {
                                settings.uncertain = true;
                            }
                        } else {
                            settings.update(options, &self.styles);
                        }
                        settings.uncertain |= conditional;
                        self.environments.insert(
                            name.to_owned(),
                            Spec {
                                family,
                                settings,
                                delegate: false,
                            },
                        );
                    } else {
                        mark_existing_spec_uncertain(&mut self.environments, name);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn environment(&self, name: &str, header: &str) -> Option<Spec> {
        let mut spec = self.environments.get(name).cloned().or_else(|| {
            Some(Spec {
                family: match name {
                    "lstlisting" => Family::Listings,
                    "minted" => Family::Minted,
                    "Verbatim" | "BVerbatim" | "LVerbatim" | "SaveVerbatim" => Family::Fancy,
                    _ => return None,
                },
                settings: Settings::default(),
                delegate: name == "minted" || name == "lstlisting",
            })
        })?;
        if name.ends_with('*')
            && self.environments.contains_key(name)
            && let Some((options, _)) = argument(header, '{', '}')
        {
            spec.settings.update(options, &self.styles);
            return Some(self.resolve(spec, "", false));
        }
        Some(self.resolve(spec, header, false))
    }

    pub fn inline(&self, name: &str, header: &str) -> Option<Spec> {
        let spec = self.inline.get(name).cloned().or_else(|| {
            Some(Spec {
                family: match name {
                    "mintinline" | "mint" => Family::Minted,
                    "lstinline" => Family::Listings,
                    _ => return None,
                },
                settings: Settings::default(),
                delegate: name == "minted" || name == "lstlisting",
            })
        })?;
        Some(self.resolve(spec, header, true))
    }

    fn resolve(&self, spec: Spec, header: &str, inline: bool) -> Spec {
        let mut settings = match spec.family {
            Family::Listings => self.listings.clone(),
            Family::Fancy => self.fancy.clone(),
            Family::Minted => self.minted.get("").cloned().unwrap_or_default(),
        };
        let (options, after) = argument(header, '[', ']').unwrap_or(("", header));
        let language =
            if spec.family == Family::Minted && !spec.settings.values.contains_key("language") {
                argument(after, '{', '}')
                    .map(|(name, _)| name)
                    .or_else(|| spec.settings.values.get("language").map(String::as_str))
            } else {
                spec.settings.values.get("language").map(String::as_str)
            };
        if spec.family == Family::Minted
            && let Some(language) = language
        {
            if let Some(defaults) = self.minted.get(language) {
                settings.values.extend(defaults.values.clone());
                settings.uncertain |= defaults.uncertain;
            }
            settings.values.insert("language".into(), language.into());
            if inline {
                for key in ["", language] {
                    if let Some(defaults) = self.minted_inline.get(key) {
                        settings.values.extend(defaults.values.clone());
                        settings.uncertain |= defaults.uncertain;
                    }
                }
            }
        }
        settings.values.extend(spec.settings.values.clone());
        settings.uncertain |= spec.settings.uncertain;
        settings.update(options, &self.styles);
        let mut local = Settings::default();
        local.update(options, &self.styles);
        let explicit = if spec.family == Family::Minted {
            language
        } else {
            local.values.get("language").map(String::as_str)
        };
        let header_len = if spec.family == Family::Minted {
            argument(after, '{', '}').map_or(0, |(_, tail)| header.len() - tail.len())
        } else {
            header.len() - after.len()
        };
        let delegate = spec.delegate
            && !header[..header_len].contains(['\n', '\r'])
            && !options.contains(['[', ']'])
            && explicit.is_some_and(|v| {
                settings
                    .values
                    .get("language")
                    .is_some_and(|name| v == name)
            })
            && settings.values.get("language").is_some_and(|v| {
                matches!(
                    v.to_ascii_lowercase().as_str(),
                    "c" | "c++"
                        | "cpp"
                        | "javascript"
                        | "js"
                        | "typescript"
                        | "ts"
                        | "json"
                        | "css"
                        | "html"
                )
            })
            && settings.escapes(spec.family).is_ok_and(|v| v.is_none())
            && !settings.flag("mathescape")
            && !settings.flag("texcomments")
            && !settings.flag("texcl")
            && (spec.family != Family::Listings || plain_language_option(options));
        Spec {
            family: spec.family,
            settings,
            delegate,
        }
    }
}

fn mark_existing_spec_uncertain(target: &mut HashMap<String, Spec>, name: &str) {
    for name in [name.to_owned(), format!("{name}*")] {
        if let Some(spec) = target.get_mut(&name) {
            spec.settings.uncertain = true;
        }
    }
}

fn static_setting_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\\', '#', '%', '{', '}'])
}

fn plain_language_option(options: &str) -> bool {
    // External grammar embedding requires one plain literal language field.
    // Nested or duplicate occurrences can select a different language from
    // keyval's last assignment, so keep those bodies protected.
    !options.contains(['{', '}', '\\'])
        && options
            .match_indices("language")
            .filter(|(at, word)| options[at + word.len()..].trim_start().starts_with('='))
            .count()
            == 1
        && fields(options)
            .iter()
            .filter(|field| {
                field
                    .split_once('=')
                    .is_some_and(|(key, _)| key.trim() == "language")
            })
            .count()
            == 1
}

pub(crate) fn argument(source: &str, open: char, close: char) -> Option<(&str, &str)> {
    let source = source.trim_start();
    let after = source.strip_prefix(open)?;
    let mut braces = 0;
    let mut escaped = false;
    for (i, c) in after.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == close && braces == 0 {
            return Some((&after[..i], &after[i + c.len_utf8()..]));
        }
        if c == '{' {
            braces += 1;
        } else if c == '}' {
            if braces == 0 {
                return None;
            }
            braces -= 1;
        }
    }
    None
}

fn fields(source: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in source.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                result.push(&source[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    result.push(&source[start..]);
    result
}

fn literal_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'*' | b'+' | b'#' | b'.')
        })
}
fn unbrace(s: &str) -> &str {
    argument(s, '{', '}')
        .filter(|(_, tail)| tail.trim().is_empty())
        .map_or(s, |(body, _)| body)
}
fn control_symbols(s: &str) -> Option<String> {
    let mut result = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let c = chars.next()?;
            if c.is_ascii_alphabetic() {
                return None;
            }
            result.push(c);
        } else {
            result.push(c);
        }
    }
    Some(result)
}
fn push(out: &mut Vec<Token>, kind: K, text: &str) {
    if !text.is_empty() {
        out.push(Token {
            kind,
            text: SmolStr::new(text),
        });
    }
}

/// Ownership classes needed by minted. Strings, including multiline strings,
/// are never scanned as ordinary code for escapeinside.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Code,
    String,
    Comment,
}
fn python(source: &str) -> Vec<(usize, usize, Class, K)> {
    let mut result = Vec::new();
    let mut i = 0;
    let mut function = false;
    while i < source.len() {
        let start = i;
        let c = source[i..].chars().next().unwrap();
        let mut class = Class::Code;
        let mut kind = K::FOREIGN_TEXT;
        if c == '#' {
            i += source[i..].find(['\n', '\r']).unwrap_or(source.len() - i);
            class = Class::Comment;
            kind = K::FOREIGN_COMMENT;
        } else if matches!(c, '\'' | '"') {
            class = Class::String;
            let quote = c.to_string();
            let triple = quote.repeat(3);
            let delimiter = if source[i..].starts_with(&triple) {
                triple
            } else {
                quote
            };
            i += delimiter.len();
            while i < source.len() {
                if source[i..].starts_with('\\') {
                    i += 1;
                    if i < source.len() {
                        i += source[i..].chars().next().unwrap().len_utf8();
                    }
                } else if source[i..].starts_with(&delimiter) {
                    i += delimiter.len();
                    break;
                } else if delimiter.len() == 1 && source[i..].starts_with(['\n', '\r']) {
                    break;
                } else {
                    i += source[i..].chars().next().unwrap().len_utf8();
                }
            }
        } else if c.is_alphabetic() || c == '_' {
            i += source[i..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .map(char::len_utf8)
                .sum::<usize>();
            let word = &source[start..i];
            if matches!(
                word,
                "False"
                    | "None"
                    | "True"
                    | "and"
                    | "as"
                    | "assert"
                    | "async"
                    | "await"
                    | "break"
                    | "class"
                    | "continue"
                    | "def"
                    | "del"
                    | "elif"
                    | "else"
                    | "except"
                    | "finally"
                    | "for"
                    | "from"
                    | "global"
                    | "if"
                    | "import"
                    | "in"
                    | "is"
                    | "lambda"
                    | "nonlocal"
                    | "not"
                    | "or"
                    | "pass"
                    | "raise"
                    | "return"
                    | "try"
                    | "while"
                    | "with"
                    | "yield"
            ) {
                kind = K::FOREIGN_KEYWORD;
            } else if function {
                kind = K::FOREIGN_FUNCTION;
            }
            function = word == "def";
        } else {
            i += c.len_utf8();
        }
        result.push((start, i, class, kind));
    }
    result
}

fn balanced(source: &str, ctx: &ParseCtx, config: LexConfig) -> bool {
    // Nested foreign captures need a separate ownership proof. Declining them
    // also bounds lexer recursion before the grammar's own nesting budget.
    if source.contains("\\mint")
        || source.contains("\\newmint")
        || source.contains("\\lst")
        || source.contains("\\DefineVerbatimEnvironment")
        || source.contains("\\begin")
            && ["minted", "Verbatim", "BVerbatim", "LVerbatim"]
                .iter()
                .any(|name| source.contains(name))
    {
        return false;
    }
    let tokens = lex_with(source, ctx, config);
    let mut depth = 0usize;
    for token in tokens {
        match token.kind {
            K::L_BRACE => {
                depth += 1;
                if depth >= crate::MAX_PARSE_DEPTH {
                    return false;
                }
            }
            K::R_BRACE => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    depth == 0
}
fn island(
    out: &mut Vec<Token>,
    body: &str,
    open: &str,
    close: &str,
    ctx: &ParseCtx,
    config: LexConfig,
) -> bool {
    if !balanced(body, ctx, config) {
        return false;
    }
    push(out, K::ESCAPE_OPEN, open);
    out.extend(lex_with(body, ctx, config));
    push(out, K::ESCAPE_CLOSE, close);
    true
}

pub(crate) fn body(
    source: &str,
    spec: &Spec,
    out: &mut Vec<Token>,
    ctx: &ParseCtx,
    config: LexConfig,
) {
    if spec.settings.uncertain {
        push(out, K::VERBATIM_BODY, source);
        return;
    }
    if spec.family == Family::Fancy {
        fancy(source, &spec.settings, out, ctx, config);
        return;
    }
    let Ok(escape) = spec.settings.escapes(spec.family) else {
        push(out, K::VERBATIM_BODY, source);
        return;
    };
    if spec.delegate {
        push(out, K::DELEGATED_CODE, source);
        return;
    }
    let py = spec.settings.python();
    if spec.family == Family::Minted && !py {
        push(out, K::VERBATIM_BODY, source);
        return;
    }
    let classes = if py {
        python(source)
    } else {
        vec![(0, source.len(), Class::Code, K::VERBATIM_BODY)]
    };
    let mut i = 0;
    while i < source.len() {
        let (_, end, class, kind) = classes[classes.partition_point(|(_, end, _, _)| *end <= i)];
        if (spec.settings.flag("texcomments") || spec.settings.flag("texcl"))
            && class == Class::Comment
        {
            push(out, K::FOREIGN_COMMENT, &source[i..i + 1]);
            let payload = &source[i + 1..end];
            if balanced(payload, ctx, config) {
                out.extend(lex_with(payload, ctx, config));
            } else {
                push(out, K::VERBATIM_BODY, payload);
            }
            i = end;
            continue;
        }
        let safe = spec.family == Family::Listings || class == Class::Code;
        if safe
            && let Some((open, close)) = &escape
            && source[i..].starts_with(open)
        {
            let a = i + open.len();
            if let Some(rel) = source[a..].find(close) {
                let b = a + rel;
                if island(out, &source[a..b], open, close, ctx, config) {
                    i = b + close.len();
                    continue;
                }
            }
            // An unfinished or malformed escape is one protected suffix. Never
            // expose its apparent commands or consume the environment closer.
            push(out, K::VERBATIM_BODY, &source[i..]);
            break;
        }
        if spec.settings.flag("mathescape")
            && (spec.family == Family::Listings || class == Class::Comment)
            && source[i..].starts_with('$')
        {
            if let Some(rel) = source[i + 1..].find('$') {
                let b = i + 1 + rel;
                let math = &source[i..=b];
                if balanced(math, ctx, config) {
                    out.extend(lex_with(math, ctx, config));
                    i = b + 1;
                    continue;
                }
            }
            push(out, K::VERBATIM_BODY, &source[i..]);
            break;
        }
        // Split code runs at possible escape starts, even inside listings strings
        // and comments. Python lexical classes otherwise keep their own role.
        let mut next = end;
        for (j, _) in source[i..end].char_indices().skip(1) {
            let pos = i + j;
            if safe
                && escape
                    .as_ref()
                    .is_some_and(|(a, _)| source[pos..].starts_with(a))
                || spec.settings.flag("mathescape")
                    && (spec.family == Family::Listings || class == Class::Comment)
                    && source[pos..].starts_with('$')
            {
                next = pos;
                break;
            }
        }
        push(out, kind, &source[i..next]);
        i = next;
    }
}

fn fancy(
    source: &str,
    settings: &Settings,
    out: &mut Vec<Token>,
    ctx: &ParseCtx,
    config: LexConfig,
) {
    let chars = settings
        .values
        .get("commandchars")
        .and_then(|v| control_symbols(unbrace(v)));
    let chars = chars.as_deref().unwrap_or("");
    let list: Vec<_> = chars.chars().collect();
    let comment = settings
        .values
        .get("commentchar")
        .and_then(|v| control_symbols(unbrace(v)));
    if list.is_empty()
        && let Some(comment) = comment
            .as_ref()
            .filter(|v| v.chars().count() == 1)
            .and_then(|v| v.chars().next())
    {
        for line in source.split_inclusive(['\n', '\r']) {
            if let Some(at) = line.find(comment) {
                push(out, K::VERBATIM_BODY, &line[..at]);
                let end = line.trim_end_matches(['\n', '\r']).len();
                push(out, K::EXCLUDED_BODY, &line[at..end]);
                push(out, K::VERBATIM_BODY, &line[end..]);
            } else {
                push(out, K::VERBATIM_BODY, line);
            }
        }
        return;
    }
    if list.len() != 3
        || list
            .iter()
            .any(|c| !c.is_ascii() || c.is_ascii_whitespace())
        || list[0] == list[1]
        || list[0] == list[2]
        || list[1] == list[2]
        || settings.values.contains_key("commentchar")
            && comment.as_ref().is_none_or(|c| c.chars().count() != 1)
    {
        push(out, K::VERBATIM_BODY, source);
        return;
    }
    let comment = comment.and_then(|s| s.chars().next());
    if comment.is_some_and(|c| list.contains(&c) || matches!(c, '\\' | '{' | '}')) {
        push(out, K::VERBATIM_BODY, source);
        return;
    }
    out.extend(super::lexer::lex_literal_commands(
        source,
        [list[0], list[1], list[2]],
        comment,
        ctx,
        config,
    ));
}

pub(crate) const FULL_REPARSE_SIGNALS: &[&str] = &[
    "\\catcode",
    "\\string",
    "\\newmint",
    "\\setminted",
    "\\lstset",
    "\\lstdefinestyle",
    "\\lstnewenvironment",
    "commandchars",
    "commentchar",
    "escapeinside",
    "escapechar",
    "mathescape",
    "texcomments",
    "texcl",
    "\\fvset",
    "\\DefineShortVerb",
    "\\MakeShortVerb",
    "\\DeleteShortVerb",
    "\\UndefineShortVerb",
    "{alltt}",
    "\\detokenize",
    "\\input",
    "\\DefineVerbatimEnvironment",
    "\\includecomment",
    "\\excludecomment",
    "{comment}",
];

pub(crate) fn needs_full_parse(source: &str) -> bool {
    FULL_REPARSE_SIGNALS
        .iter()
        .any(|signal| source.contains(signal))
}

/// Cached source-only restrictions. These do not participate in lexing or shape.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReparseGuard {
    pub full: bool,
    pub local_modes: bool,
}

pub(crate) const LOCAL_REPARSE_SIGNALS: &[&str] = &[
    "\\makeatletter",
    "\\makeatother",
    "\\ExplSyntax",
    "\\begingroup",
    "\\endgroup",
    "\\bgroup",
    "\\egroup",
];

impl ReparseGuard {
    pub fn for_source(source: &str) -> Self {
        Self {
            full: needs_full_parse(source),
            local_modes: LOCAL_REPARSE_SIGNALS
                .iter()
                .any(|signal| source.contains(signal)),
        }
    }
}
