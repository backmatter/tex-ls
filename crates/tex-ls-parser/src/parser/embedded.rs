//! Source-only listing settings and bounded foreign lexical profiles.
//!
//! Nothing here depends on installed editor grammars or executes TeX. Unknown
//! option values retain a protected body instead of guessing island ownership.
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Deref;
use std::rc::Rc;

use smol_str::SmolStr;

use super::lexer::{LexConfig, ParseCtx, Token};
use crate::syntax::SyntaxKind as K;

#[cfg(test)]
mod tests {
    use super::super::lexer::RawReaders;
    use super::*;

    #[test]
    fn large_setting_values_are_shared_across_clones_and_resolved_specs() {
        let language = "literal-language-".repeat(8192);
        let mut defaults = Settings::default();
        defaults
            .values
            .insert("language".into(), language.clone().into());
        let pointer = defaults.values["language"].as_ptr();
        let retained = defaults.clone();
        assert_eq!(retained.values["language"].as_ptr(), pointer);

        let mut state = State {
            listings: defaults,
            ..State::default()
        };
        let plain = Spec {
            family: Family::Listings,
            settings: Settings::default(),
            delegate: false,
        };
        let mut readers = RawReaders::new("");
        let resolved = state.resolve(plain.clone(), "", true, &mut readers);
        assert_eq!(resolved.settings.values["language"].as_ptr(), pointer);
        assert_eq!(&*resolved.settings.values["language"], language.as_str());

        let mut override_settings = Settings::default();
        override_settings
            .values
            .insert("language".into(), "override-language-".repeat(8192).into());
        let override_pointer = override_settings.values["language"].as_ptr();
        state
            .styles
            .insert("large".into(), override_settings.clone());
        let header = "[style=large]";
        let mut readers = RawReaders::new(header);
        let styled = state.resolve(plain, header, true, &mut readers);
        assert_eq!(
            styled.settings.values["language"].as_ptr(),
            override_pointer
        );

        let explicit = Spec {
            family: Family::Listings,
            settings: override_settings,
            delegate: false,
        };
        let mut readers = RawReaders::new("");
        let overridden = state.resolve(explicit, "", true, &mut readers);
        assert_eq!(
            overridden.settings.values["language"].as_ptr(),
            override_pointer
        );

        let shortcut = Spec {
            family: Family::Minted,
            settings: retained.clone(),
            delegate: false,
        };
        state.inline.insert("cachedinline".into(), shortcut);
        let mut readers = RawReaders::new("");
        let minted = state.inline("cachedinline", "", &mut readers).unwrap();
        assert_eq!(minted.settings.values["language"].as_ptr(), pointer);

        state.listings.update("language=Python", &state.styles);
        assert!(state.listings.python());
        assert!(!resolved.settings.python());
        assert_eq!(&*resolved.settings.values["language"], language.as_str());
        assert_eq!(retained.values["language"].as_ptr(), pointer);
    }

    #[test]
    fn delegate_languages_match_ascii_case_without_normalizing_owned_values() {
        let state = State::default();
        for language in [
            "C",
            "C++",
            "Cpp",
            "JavaScript",
            "JS",
            "TypeScript",
            "TS",
            "JSON",
            "CSS",
            "HTML",
            "ÇSS",
            "Python",
        ] {
            let header = format!("[language={language}]");
            let mut readers = RawReaders::new(&header);
            let spec = state.resolve(
                Spec {
                    family: Family::Listings,
                    settings: Settings::default(),
                    delegate: true,
                },
                &header,
                false,
                &mut readers,
            );
            assert_eq!(
                spec.delegate,
                !matches!(language, "ÇSS" | "Python"),
                "{language}"
            );
            assert_eq!(&*spec.settings.values["language"], language);
        }
    }

    #[test]
    fn decoded_listing_delimiters_are_shared_across_resolved_settings() {
        let opener = "opening-marker-".repeat(4096);
        let closer = "closing-marker-".repeat(4096);
        let source = format!("language=Python,escapeinside={{{opener}}}{{{closer}}}");
        let mut settings = Settings::default();
        settings.update(&source, &HashMap::new());
        let first = settings.escapes(Family::Listings).unwrap().unwrap();
        assert_eq!(&*first.0, opener.as_str());
        assert_eq!(&*first.1, closer.as_str());
        let second = settings.escapes(Family::Listings).unwrap().unwrap();
        assert_eq!(first.0.as_ptr(), second.0.as_ptr());
        assert_eq!(first.1.as_ptr(), second.1.as_ptr());

        let retained = settings.clone();
        let clone_pair = retained.escapes(Family::Listings).unwrap().unwrap();
        assert_eq!(first.0.as_ptr(), clone_pair.0.as_ptr());
        let state = State {
            listings: settings.clone(),
            ..State::default()
        };
        let mut readers = RawReaders::new("");
        let resolved = state.inline("lstinline", "", &mut readers).unwrap();
        let resolved_pair = resolved
            .settings
            .escapes(Family::Listings)
            .unwrap()
            .unwrap();
        assert_eq!(first.0.as_ptr(), resolved_pair.0.as_ptr());

        let mut cold = Settings::default();
        cold.update(&source, &HashMap::new());
        assert_eq!(
            settings, cold,
            "memo warmth must not affect source identity"
        );
        settings.update("escapeinside={A}{B}", &HashMap::new());
        let changed = settings.escapes(Family::Listings).unwrap().unwrap();
        assert_eq!((&*changed.0, &*changed.1), ("A", "B"));
        assert_eq!(
            retained
                .escapes(Family::Listings)
                .unwrap()
                .unwrap()
                .0
                .as_ptr(),
            first.0.as_ptr()
        );

        settings.uncertain = true;
        assert!(settings.escapes(Family::Listings).is_err());
        settings.uncertain = false;
        settings.update("mathescape=unknown", &HashMap::new());
        assert!(settings.escapes(Family::Listings).is_err());
    }
    #[test]
    fn decoded_setting_profiles_keep_errors_overrides_and_families_separate() {
        let mut settings = Settings::default();
        settings.update("escapeinside={AB}", &HashMap::new());
        assert!(settings.escapes(Family::Listings).is_err());
        let value = &settings.values["escapeinside"];
        assert!(matches!(value.0.listing_pair.get(), Some(Err(()))));
        let pair = settings.escapes(Family::Minted).unwrap().unwrap();
        assert_eq!((&*pair.0, &*pair.1), ("A", "B"));
        let fancy = settings.escapes(Family::Fancy).unwrap().unwrap();
        assert_eq!(pair.0.as_ptr(), fancy.0.as_ptr());
        assert!(settings.escapes(Family::Listings).is_err());

        let retained = settings.clone();
        let mut replacement = Settings::default();
        replacement.update("escapeinside={X}{Y}", &HashMap::new());
        settings.update(
            "style=replacement",
            &HashMap::from([("replacement".into(), replacement.clone())]),
        );
        let styled = settings.escapes(Family::Listings).unwrap().unwrap();
        let direct = replacement.escapes(Family::Listings).unwrap().unwrap();
        assert_eq!(styled.0.as_ptr(), direct.0.as_ptr());
        assert!(settings.escapes(Family::Minted).is_err());
        assert_eq!(
            retained
                .escapes(Family::Minted)
                .unwrap()
                .unwrap()
                .0
                .as_ptr(),
            pair.0.as_ptr()
        );

        let mut character = Settings::default();
        character.update("escapechar={\\@}", &HashMap::new());
        let pair = character.escapes(Family::Listings).unwrap().unwrap();
        assert_eq!((&*pair.0, &*pair.1), ("@", "@"));
        assert_eq!(pair.0.as_ptr(), pair.1.as_ptr());
        assert_eq!(
            pair.0.as_ptr(),
            character
                .escapes(Family::Minted)
                .unwrap()
                .unwrap()
                .0
                .as_ptr()
        );
        character.update("escapechar={\\unknown}", &HashMap::new());
        assert!(character.escapes(Family::Listings).is_err());
        assert!(matches!(
            character.values["escapechar"].0.escape_character.get(),
            Some(Err(()))
        ));
        assert!(character.escapes(Family::Listings).is_err());
    }

    #[test]
    fn fancy_symbols_share_decoding_and_reject_oversized_profiles() {
        let mut settings = Settings::default();
        settings.update(
            "commandchars={\\\\\\{\\}},commentchar={\\%}",
            &HashMap::new(),
        );
        for key in ["commandchars", "commentchar"] {
            let first = settings.values[key].symbols().unwrap();
            let cloned = settings.clone();
            let second = cloned.values[key].symbols().unwrap();
            assert_eq!(first.as_ptr(), second.as_ptr());
        }
        settings.update(
            &format!("commandchars={{{}}}", "x".repeat(65536)),
            &HashMap::new(),
        );
        let mut tokens = Vec::new();
        fancy(
            "\\ref{protected}",
            &settings,
            &mut tokens,
            &ParseCtx::default(),
            LexConfig::default(),
            0,
        );
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, K::VERBATIM_BODY);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    Listings,
    Minted,
    Fancy,
}

type EscapeDelimiters = (Rc<str>, Rc<str>);
type EscapePair = Result<EscapeDelimiters, ()>;

// Values are immutable. Cloned scopes share source and decoded results; replacing
// a setting creates a fresh value without changing an already resolved Spec.
#[derive(Clone)]
struct SettingValue(Rc<SettingValueData>);

struct SettingValueData {
    source: Box<str>,
    listing_pair: OnceCell<EscapePair>,
    character_pair: OnceCell<EscapePair>,
    escape_character: OnceCell<EscapePair>,
    symbols: OnceCell<Option<Rc<str>>>,
}

impl From<&str> for SettingValue {
    fn from(source: &str) -> Self {
        Self::from(source.to_owned())
    }
}

impl From<String> for SettingValue {
    fn from(source: String) -> Self {
        Self(Rc::new(SettingValueData {
            source: source.into_boxed_str(),
            listing_pair: OnceCell::new(),
            character_pair: OnceCell::new(),
            escape_character: OnceCell::new(),
            symbols: OnceCell::new(),
        }))
    }
}

impl Deref for SettingValue {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0.source
    }
}

impl AsRef<str> for SettingValue {
    fn as_ref(&self) -> &str {
        self
    }
}

impl fmt::Debug for SettingValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_ref(), f)
    }
}

impl PartialEq for SettingValue {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0) || self.as_ref() == other.as_ref()
    }
}

impl Eq for SettingValue {}

impl SettingValue {
    fn symbols(&self) -> Option<Rc<str>> {
        self.0
            .symbols
            .get_or_init(|| control_symbols(unbrace(self)).map(Rc::from))
            .clone()
    }

    fn inside(&self, family: Family) -> EscapePair {
        if family == Family::Listings {
            self.0
                .listing_pair
                .get_or_init(|| {
                    let (a, rest) = argument(self, '{', '}').ok_or(())?;
                    let (b, rest) = argument(rest, '{', '}').ok_or(())?;
                    if !rest.trim().is_empty() || a.is_empty() || b.is_empty() {
                        return Err(());
                    }
                    Ok((
                        Rc::from(control_symbols(a).ok_or(())?),
                        Rc::from(control_symbols(b).ok_or(())?),
                    ))
                })
                .clone()
        } else {
            self.0
                .character_pair
                .get_or_init(|| {
                    let decoded = self.symbols().ok_or(())?;
                    let mut chars = decoded.chars();
                    let a = chars.next().ok_or(())?;
                    let b = chars.next().ok_or(())?;
                    if chars.next().is_some() {
                        return Err(());
                    }
                    Ok((Rc::from(a.to_string()), Rc::from(b.to_string())))
                })
                .clone()
        }
    }

    fn character(&self) -> EscapePair {
        self.0
            .escape_character
            .get_or_init(|| {
                let decoded = self.symbols().ok_or(())?;
                if decoded.chars().take(2).count() != 1 {
                    return Err(());
                }
                Ok((decoded.clone(), decoded))
            })
            .clone()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Settings {
    values: HashMap<String, SettingValue>,
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
                self.values.insert(key.to_owned(), value.into());
            }
        }
    }

    fn flag(&self, key: &str) -> bool {
        self.values.get(key).is_some_and(|v| v.as_ref() == "true")
    }

    pub(crate) fn python(&self) -> bool {
        self.values.get("language").is_some_and(|v| {
            v.eq_ignore_ascii_case("python")
                || v.eq_ignore_ascii_case("python3")
                || v.eq_ignore_ascii_case("py")
        })
    }

    fn escapes(&self, family: Family) -> Result<Option<EscapeDelimiters>, ()> {
        if self.uncertain {
            return Err(());
        }
        for key in ["mathescape", "texcomments", "texcl"] {
            if self
                .values
                .get(key)
                .is_some_and(|v| !matches!(v.as_ref(), "true" | "false"))
            {
                return Err(());
            }
        }
        if let Some(value) = self.values.get("escapeinside") {
            return value.inside(family).map(Some);
        }
        if let Some(value) = self.values.get("escapechar") {
            return value.character().map(Some);
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
    pub environment_name_lengths: HashSet<usize>,
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
    pub fn observe(
        &mut self,
        command: &str,
        after: &str,
        conditional: bool,
        readers: &mut super::lexer::RawReaders<'_>,
    ) {
        match command {
            "\\lstset" | "\\fvset" => {
                let target = if command == "\\lstset" {
                    &mut self.listings
                } else {
                    &mut self.fancy
                };
                if let Some((value, _)) = readers.embedded_argument(after, '{', '}') {
                    target.update(value, &self.styles);
                    target.uncertain |= conditional;
                } else {
                    target.uncertain = true;
                }
            }
            "\\lstdefinestyle" => {
                let Some((name, after)) = readers
                    .embedded_argument(after, '{', '}')
                    .filter(|(name, _)| static_setting_name(name))
                else {
                    for settings in self.styles.values_mut() {
                        settings.uncertain = true;
                    }
                    return;
                };
                let mut settings = Settings::default();
                if let Some((value, _)) = readers.embedded_argument(after, '{', '}') {
                    settings.update(value, &self.styles);
                    settings.uncertain |= conditional;
                } else {
                    settings.uncertain = true;
                }
                self.styles.insert(name.to_owned(), settings);
            }
            "\\setminted" | "\\setmintedinline" => {
                let (language, after) = readers
                    .embedded_argument(after, '[', ']')
                    .unwrap_or(("", after));
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
                if let Some((value, _)) = readers.embedded_argument(after, '{', '}') {
                    settings.update(value, &self.styles);
                    settings.uncertain |= conditional;
                } else {
                    settings.uncertain = true;
                }
            }
            "\\includecomment" | "\\excludecomment" => {
                if let Some((name, _)) = readers.embedded_argument(after, '{', '}')
                    && literal_name(name)
                {
                    self.environment_name_lengths.insert(name.len());
                    self.comments.insert(
                        name.to_owned(),
                        (!conditional).then_some(command == "\\includecomment"),
                    );
                }
            }
            "\\newminted" | "\\newmintinline" => {
                let (alias, after) = readers
                    .embedded_argument(after, '[', ']')
                    .map_or((None, after), |(name, rest)| (Some(name), rest));
                let language_argument = readers.embedded_argument(after, '{', '}');
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
                    literal_name(language) && readers.embedded_argument(after, '{', '}').is_some()
                }) && let Some(name) = declared_name.filter(|name| literal_name(name))
                {
                    let target = if command == "\\newminted" {
                        &mut self.environments
                    } else {
                        &mut self.inline
                    };
                    mark_existing_spec_uncertain(target, &name);
                }
                if let Some((language, after)) = readers.embedded_argument(after, '{', '}')
                    && let Some((options, _)) = readers.embedded_argument(after, '{', '}')
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
                        if command == "\\newminted" {
                            self.environment_name_lengths.insert(name.len());
                            self.environment_name_lengths.insert(name.len() + 1);
                        }
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
                if let Some((name, mut after)) = readers.embedded_argument(after, '{', '}')
                    && literal_name(name)
                {
                    let family = if command == "\\lstnewenvironment" {
                        Family::Listings
                    } else {
                        Family::Fancy
                    };
                    if family == Family::Listings {
                        for _ in 0..2 {
                            if let Some((_, rest)) = readers.embedded_argument(after, '[', ']') {
                                after = rest;
                            }
                        }
                    } else if let Some((_, rest)) = readers.embedded_argument(after, '{', '}') {
                        after = rest;
                    }
                    if let Some((options, _)) = readers.embedded_argument(after, '{', '}') {
                        let mut settings = Settings::default();
                        if family == Family::Listings {
                            if let Some(value) = options.trim().strip_prefix("\\lstset")
                                && let Some((value, rest)) =
                                    readers.embedded_argument(value, '{', '}')
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
                        self.environment_name_lengths.insert(name.len());
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

    pub fn environment(
        &self,
        name: &str,
        header: &str,
        readers: &mut super::lexer::RawReaders<'_>,
    ) -> Option<Spec> {
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
            && let Some((options, _)) = readers.embedded_argument(header, '{', '}')
        {
            spec.settings.update(options, &self.styles);
            return Some(self.resolve(spec, &header[header.len()..], false, readers));
        }
        Some(self.resolve(spec, header, false, readers))
    }

    pub fn inline(
        &self,
        name: &str,
        header: &str,
        readers: &mut super::lexer::RawReaders<'_>,
    ) -> Option<Spec> {
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
        Some(self.resolve(spec, header, true, readers))
    }

    fn resolve(
        &self,
        spec: Spec,
        header: &str,
        inline: bool,
        readers: &mut super::lexer::RawReaders<'_>,
    ) -> Spec {
        let mut settings = match spec.family {
            Family::Listings => self.listings.clone(),
            Family::Fancy => self.fancy.clone(),
            Family::Minted => self.minted.get("").cloned().unwrap_or_default(),
        };
        let (options, after) = readers
            .embedded_argument(header, '[', ']')
            .unwrap_or(("", header));
        let language =
            if spec.family == Family::Minted && !spec.settings.values.contains_key("language") {
                readers
                    .embedded_argument(after, '{', '}')
                    .map(|(name, _)| name)
                    .or_else(|| {
                        spec.settings
                            .values
                            .get("language")
                            .map(SettingValue::as_ref)
                    })
            } else {
                spec.settings
                    .values
                    .get("language")
                    .map(SettingValue::as_ref)
            };
        if spec.family == Family::Minted
            && let Some(language) = language
        {
            if let Some(defaults) = self.minted.get(language) {
                settings.values.extend(defaults.values.clone());
                settings.uncertain |= defaults.uncertain;
            }
            let value = spec
                .settings
                .values
                .get("language")
                .cloned()
                .unwrap_or_else(|| language.into());
            settings.values.insert("language".into(), value);
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
            local.values.get("language").map(SettingValue::as_ref)
        };
        let header_len = if spec.family == Family::Minted {
            readers
                .embedded_argument(after, '{', '}')
                .map_or(0, |(_, tail)| header.len() - tail.len())
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
                    .is_some_and(|name| v == name.as_ref())
            })
            && settings.values.get("language").is_some_and(|v| {
                [
                    "c",
                    "c++",
                    "cpp",
                    "javascript",
                    "js",
                    "typescript",
                    "ts",
                    "json",
                    "css",
                    "html",
                ]
                .iter()
                .any(|language| v.eq_ignore_ascii_case(language))
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

fn balanced(source: &str, ctx: &ParseCtx, config: LexConfig, source_offset: usize) -> bool {
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
    let tokens = super::lexer::lex_with_offset(source, ctx, config, source_offset);
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
    source_offset: usize,
) -> bool {
    if !balanced(body, ctx, config, source_offset) {
        return false;
    }
    push(out, K::ESCAPE_OPEN, open);
    out.extend(super::lexer::lex_with_offset(
        body,
        ctx,
        config,
        source_offset,
    ));
    push(out, K::ESCAPE_CLOSE, close);
    true
}

pub(crate) fn body(
    source: &str,
    spec: &Spec,
    out: &mut Vec<Token>,
    ctx: &ParseCtx,
    config: LexConfig,
    source_offset: usize,
) {
    if spec.settings.uncertain {
        push(out, K::VERBATIM_BODY, source);
        return;
    }
    if spec.family == Family::Fancy {
        fancy(source, &spec.settings, out, ctx, config, source_offset);
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
            if balanced(payload, ctx, config, source_offset + i + 1) {
                out.extend(super::lexer::lex_with_offset(
                    payload,
                    ctx,
                    config,
                    source_offset + i + 1,
                ));
            } else {
                push(out, K::VERBATIM_BODY, payload);
            }
            i = end;
            continue;
        }
        let safe = spec.family == Family::Listings || class == Class::Code;
        if safe
            && let Some((open, close)) = &escape
            && source[i..].starts_with(open.as_ref())
        {
            let a = i + open.len();
            if let Some(rel) = source[a..].find(close.as_ref()) {
                let b = a + rel;
                if island(
                    out,
                    &source[a..b],
                    open,
                    close,
                    ctx,
                    config,
                    source_offset + a,
                ) {
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
                if balanced(math, ctx, config, source_offset + i) {
                    out.extend(super::lexer::lex_with_offset(
                        math,
                        ctx,
                        config,
                        source_offset + i,
                    ));
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
                    .is_some_and(|(a, _)| source[pos..].starts_with(a.as_ref()))
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
    source_offset: usize,
) {
    let chars = settings
        .values
        .get("commandchars")
        .and_then(SettingValue::symbols);
    let chars = chars.as_deref().unwrap_or("");
    let list: Vec<_> = chars.chars().take(4).collect();
    let comment = settings
        .values
        .get("commentchar")
        .and_then(SettingValue::symbols);
    if list.is_empty()
        && let Some(comment) = comment
            .as_ref()
            .filter(|v| v.chars().take(2).count() == 1)
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
            && comment
                .as_ref()
                .is_none_or(|c| c.chars().take(2).count() != 1)
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
        source_offset,
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
    "\\DoNotIndex",
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
    pub letter_short_verbs: bool,
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
        let full = needs_full_parse(source);
        Self {
            full,
            // Only an ASCII letter delimiter can affect an ASCII-letter WORD
            // edit. Use the lexer's exact operand reader and conservatively
            // include every source occurrence, even inactive/commented ones.
            letter_short_verbs: full
                && [
                    "\\DefineShortVerb",
                    "\\MakeShortVerb",
                    "\\DeleteShortVerb",
                    "\\UndefineShortVerb",
                ]
                .iter()
                .any(|signal| {
                    source.match_indices(signal).any(|(start, _)| {
                        super::lexer::may_declare_letter_short_verb(&source[start + signal.len()..])
                    })
                }),
            local_modes: LOCAL_REPARSE_SIGNALS
                .iter()
                .any(|signal| source.contains(signal)),
        }
    }
}
