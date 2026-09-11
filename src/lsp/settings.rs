//! Native settings responsibility.
use super::*;

/// Settings supplied by the editor, as `initializationOptions` at startup or via
/// `workspace/didChangeConfiguration`. The width knobs are a fallback beneath a
/// discovered `meaning.toml` and the per-request [`FormattingOptions`]; `texmf` is
/// *machine* configuration with no `meaning.toml` counterpart — the editor is its
/// only source, and the file-wins rule does not apply to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct EditorSettings {
    pub(super) line_width: Option<u32>,
    pub(super) indent_width: Option<u32>,
    /// Installed-tree discovery for LSP package resolution (see [`InstalledPackages`]).
    /// Session-stable in practice: the
    /// [`texmf::global_index`](crate::texmf::global_index) it drives is
    /// first-config-wins.
    pub(super) texmf: InstalledPackages,
    /// The PDF viewer forward search drives (see [`ForwardSearchSettings`]).
    pub(super) forward_search: ForwardSearchSettings,
}

/// The external PDF viewer `textDocument/forwardSearch` launches.
///
/// *Machine* configuration with no `meaning.toml` counterpart, for the same
/// reason as [`InstalledPackages`]: which viewer is installed, and under what name, is
/// a fact about the machine rather than about the project. The editor is its only
/// source, and the file-wins rule does not apply. Where the *PDF* lives is
/// project data and belongs to `[build]` instead.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct ForwardSearchSettings {
    /// The viewer program. Spawned directly, never through a shell, so there is
    /// no word splitting: flags belong in [`args`](Self::args), and
    /// `"zathura --synctex-forward"` is a misconfiguration that cannot launch.
    pub(super) executable: Option<String>,
    /// The viewer's argument vector, each element admitting `%f`/`%p`/`%l` (see
    /// [`forward_search::viewer_args`]). There is no useful default — every
    /// viewer spells forward search differently — so an unset `args` leaves
    /// forward search unconfigured exactly as an unset `executable` does.
    pub(super) args: Option<Vec<String>>,
    /// Override for the inverse-search IPC directory. An escape hatch for
    /// containers and sandboxes, where the runtime directory may not be shared
    /// between the viewer and the server.
    pub(super) ipc_dir: Option<PathBuf>,
}

impl ForwardSearchSettings {
    /// The configured viewer, or `None` when forward search is unconfigured.
    ///
    /// Both halves are required. That matches texlab, and it is not mere
    /// compatibility: an `executable` with no `args` would launch a viewer on no
    /// document at all.
    pub(super) fn viewer(&self) -> Option<(&str, &[String])> {
        Some((self.executable.as_deref()?, self.args.as_deref()?))
    }
}

impl EditorSettings {
    /// Extract our settings from a client-supplied JSON value. Accepts either the
    /// bare options object or a tree namespaced under a `"meaning"` key (how
    /// `workspace/didChangeConfiguration` clients typically scope settings).
    pub(super) fn from_client_value(value: &serde_json::Value) -> Result<Self, String> {
        let section = value
            .get("meaning")
            .filter(|v| v.is_object())
            .unwrap_or(value);
        let settings: Self =
            serde_json::from_value(section.clone()).map_err(|error| error.to_string())?;
        crate::config::FormatConfig {
            line_width: settings.line_width.unwrap_or(80),
            indent_width: settings.indent_width.unwrap_or(2),
            ..Default::default()
        }
        .validate()
        .map_err(|error| error.to_string())?;
        Ok(settings)
    }

    /// Overlay these settings onto the formatter defaults.
    pub(super) fn to_format_style(&self) -> FormatStyle {
        let mut style = FormatStyle::default();
        if let Some(width) = self.line_width {
            style.line_width = width as usize;
        }
        if let Some(width) = self.indent_width {
            style.indent_width = width as usize;
        }
        style
    }
}

/// A document's resolved configuration: the formatter [`FormatStyle`] (with `wrap`
/// still a placeholder — the file kind decides it per request) plus the lint
/// selection. Built from a discovered `meaning.toml` (file-wins) or, absent one,
/// from the editor settings. Cached per anchor dir in [`GlobalState::config_cache`].
#[derive(Debug, Clone)]
pub(super) struct ResolvedSettings {
    /// Width knobs and `math_wrap` set; `wrap` is the [`WrapMode::default`]
    /// placeholder. `math_wrap` needs no per-file resolution here: its `Auto`
    /// default resolves against the effective wrap inside the formatter.
    pub(super) style: FormatStyle,
    /// Configured paragraph wrap, if any. `None` ⇒ the file-kind default applies.
    pub(super) wrap_override: Option<WrapMode>,
    /// Whether a `meaning.toml` governed this resolution. When `true` the file
    /// config wins outright and a request's `tab_size` is ignored.
    pub(super) config_present: bool,
    /// The `[lint]` `select`/`ignore` selection (the default-enabled rules when
    /// no file has resolved settings).
    pub(super) rules: RuleSelection,
    /// The sibling-discovery exclude filter, rooted at the config's directory. The
    /// exclude-nothing [`ExcludeFilter::none`] when no config governs (editor
    /// fallback) — preserving the unfiltered walk that path always did.
    pub(super) exclude: ExcludeFilter,
    /// The `sentence`/`semantic` language, resolved once from `[format] lang`.
    /// English (the default) when no config governs; ignored by other wrap modes.
    pub(super) sentence_lang: SentenceLanguage,
    /// The merged, normalized user no-break abbreviations from
    /// `[format.no-break-abbreviations]`. Held owned so a worker job can borrow it
    /// when building a [`SentenceOptions`] at format time.
    pub(super) sentence_no_break: Vec<String>,
    /// The `[build]` settings locating the compiler's `.aux` artifacts. Consumed
    /// only by label hover and document symbols (resolved numbers), never the
    /// formatter — so it cannot affect `meaning format` output.
    pub(super) build: BuildConfig,
    /// The project's resolved declarations (`AGENTS.md` decision #12), the one
    /// non-text input to the parse. Held behind an `Arc` because every dispatch
    /// site clones the settings and the value is identical (and usually empty)
    /// across a workspace. Reaches the worker's salsa input via
    /// [`WorkerJob::Declarations`].
    pub(super) declarations: Arc<ResolvedDeclarations>,
}

/// The cheap part of a config file's identity. Modification time catches an
/// in-place save; length catches writes on coarse-mtime filesystems when their
/// size changes. Missing files are represented by `None` in the enclosing
/// fingerprint, which also makes creation and deletion observable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConfigFileStamp {
    pub(super) modified: Option<SystemTime>,
    pub(super) len: u64,
}

pub(super) fn config_file_stamp(path: &Path) -> Option<ConfigFileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    metadata.is_file().then(|| ConfigFileStamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

/// Snapshot every project-config candidate consulted by the ancestor walk. The
/// absent entries matter: if a nearer `meaning.toml` is created, it must displace
/// the cached parent, environment, global, or default configuration.
pub(super) fn project_config_fingerprint(
    anchor: &Path,
) -> Option<Vec<(PathBuf, Option<ConfigFileStamp>)>> {
    let canonical = anchor.canonicalize().ok()?;
    let mut fingerprint = Vec::new();
    for dir in canonical.ancestors() {
        let candidate = dir.join(crate::config::CONFIG_FILE_NAME);
        let stamp = config_file_stamp(&candidate);
        let found = stamp.is_some();
        fingerprint.push((candidate, stamp));
        if found || dir.join(".git").exists() {
            break;
        }
    }
    Some(fingerprint)
}

#[derive(Debug, Clone)]
pub(super) struct CachedSettings {
    invalidated: bool,
    pub(super) resolved: ResolvedSettings,
    pub(super) project_fingerprint: Option<Vec<(PathBuf, Option<ConfigFileStamp>)>>,
    /// The resolved environment/global file may live outside the project walk.
    pub(super) source_fingerprint: Option<(PathBuf, Option<ConfigFileStamp>)>,
}

impl CachedSettings {
    pub(super) fn new(resolved: ResolvedSettings, anchor: &Path, source: Option<PathBuf>) -> Self {
        let source_fingerprint = source.map(|path| {
            let stamp = config_file_stamp(&path);
            (path, stamp)
        });
        Self {
            invalidated: false,
            resolved,
            project_fingerprint: project_config_fingerprint(anchor),
            source_fingerprint,
        }
    }

    pub(super) fn is_fresh(&self, anchor: &Path) -> bool {
        !self.invalidated
            && self.project_fingerprint == project_config_fingerprint(anchor)
            && self
                .source_fingerprint
                .as_ref()
                .is_none_or(|(path, stamp)| *stamp == config_file_stamp(path))
    }
}

impl ResolvedSettings {
    /// Resolution from a discovered config (when `present`), else from the editor
    /// settings, applying the file-wins rule. The `exclude` filter is left
    /// exclude-nothing here; [`resolve_settings`] compiles and installs the real
    /// one (it holds the config's root directory).
    ///
    /// [`resolve_settings`]: GlobalState::resolve_settings
    pub(super) fn from_config(
        config: &crate::config::ValidatedConfig,
        present: bool,
        editor: &EditorSettings,
    ) -> Self {
        if present {
            let format = config.formatter_settings();
            Self {
                style: format.style,
                wrap_override: config.format.wrap,
                config_present: true,
                rules: config.rules().clone(),
                exclude: ExcludeFilter::none(),
                sentence_lang: format.language,
                sentence_no_break: format.no_break.clone(),
                build: config.build.clone(),
                declarations: Arc::new(config.resolved_declarations().clone()),
            }
        } else {
            Self::from_editor(editor)
        }
    }

    /// Editor-settings-only resolution: width knobs over the built-in defaults, no
    /// configured wrap, the default-enabled rule set, and no exclude filter.
    pub(super) fn from_editor(editor: &EditorSettings) -> Self {
        Self {
            style: editor.to_format_style(),
            wrap_override: None,
            config_present: false,
            rules: RuleSelection::resolve(None, &[]).0,
            exclude: ExcludeFilter::none(),
            sentence_lang: SentenceLanguage::default(),
            sentence_no_break: Vec::new(),
            build: BuildConfig::default(),
            declarations: Arc::new(ResolvedDeclarations::default()),
        }
    }

    /// Reuse the selection validated when these settings were published.
    pub(super) fn rule_selection(&self) -> RuleSelection {
        self.rules.clone()
    }
}

impl GlobalState {
    pub(super) fn invalidate_settings(&mut self) {
        for cached in self.config_cache.values_mut() {
            cached.invalidated = true;
        }
    }

    fn invalid_settings(&mut self, anchor: &Path, error: String) -> ResolvedSettings {
        if self.config_errors.get(anchor) != Some(&error) {
            self.config_messages.push(error.clone());
            self.config_errors.insert(anchor.to_owned(), error);
        }
        self.config_cache
            .get(anchor)
            .map(|cached| cached.resolved.clone())
            .unwrap_or_else(|| ResolvedSettings::from_editor(&self.editor_settings))
    }

    /// Resolve (and cache) the [`ResolvedSettings`] for `uri`'s document: discover a
    /// `meaning.toml` from the document's anchor directory (its parent), falling back
    /// to the global user config (`~/.config/meaning/config.toml`), then to the
    /// editor settings when neither is found. Cached by anchor dir; cache hits
    /// stat the small ancestor candidate set and the resolved source, but avoid
    /// rereading, parsing, and rebuilding derived settings while it is unchanged.
    ///
    /// Invalid files retain the last valid settings and report a client error.
    /// Without a valid predecessor, editor defaults apply until repair. Untitled
    /// buffers use editor settings without filesystem discovery.
    pub(super) fn resolve_settings(&mut self, uri: &Uri) -> ResolvedSettings {
        let Some(anchor) = uri_to_fs_path(uri).and_then(|p| p.parent().map(Path::to_path_buf))
        else {
            return ResolvedSettings::from_editor(&self.editor_settings);
        };
        if let Some(cached) = self
            .config_cache
            .get(&anchor)
            .filter(|cached| !self.config_errors.contains_key(&anchor) && cached.is_fresh(&anchor))
        {
            return cached.resolved.clone();
        }
        let (resolved, source_path) =
            match crate::config::ConfigLoader::resolve(None, false, &anchor) {
                Ok((config, source)) => {
                    let present = source.path().is_some();
                    let source_path = source.path().map(Path::to_path_buf);
                    let mut resolved =
                        ResolvedSettings::from_config(&config, present, &self.editor_settings);
                    if present {
                        // Compile the sibling-discovery exclude filter, rooted at the
                        // config's directory — or at the document's directory for the
                        // global user config (the same `ConfigSource::exclude_root` rule
                        // as the CLI's `build_exclude_filter`; the LSP contributes no
                        // `--exclude`). Invalid patterns retain the last good settings
                        // and report an error through the same configuration channel.
                        let root = source.exclude_root(&anchor);
                        resolved.exclude =
                            match ExcludeFilter::new(root, &config.exclude_patterns(&[])) {
                                Ok(filter) => filter,
                                Err(error) => {
                                    return self.invalid_settings(&anchor, error.to_string());
                                }
                            };
                        // `[build] root` is documented relative to the config's own
                        // directory, and the consumers (forward search) only ever see
                        // the resolved `BuildConfig`. Absolutize once, here, where that
                        // directory is still in hand. `pdf-dir` is deliberately left
                        // alone: it resolves against the *root document's* directory,
                        // which is not known until the root is.
                        if let Some(build_root) =
                            resolved.build.root.as_ref().filter(|p| p.is_relative())
                        {
                            resolved.build.root = Some(root.join(build_root));
                        }
                    }
                    (resolved, source_path)
                }
                Err(error) => return self.invalid_settings(&anchor, error.to_string()),
            };
        self.config_errors.remove(&anchor);
        self.config_cache.insert(
            anchor.clone(),
            CachedSettings::new(resolved.clone(), &anchor, source_path),
        );
        resolved
    }

    /// Publish this source's resolved declarations before its request enters the
    /// worker queue. Other sources keep their own settings when focus changes.
    pub(super) fn publish_declarations(&mut self, uri: &Uri, job_tx: &Sender<WorkerJob>) {
        let declarations = self.resolve_settings(uri).declarations;
        self.publish_resolved_declarations(uri, declarations, job_tx);
    }

    /// Resolve settings for a source update. The caller carries declarations in
    /// the same worker job as the text, so readers cannot capture between them.
    pub(super) fn analysis_settings(&mut self, uri: &Uri) -> ResolvedSettings {
        let resolved = self.resolve_settings(uri);
        self.declarations
            .insert(uri_to_path(uri), resolved.declarations.clone());
        resolved
    }

    /// The write half of [`publish_declarations`](Self::publish_declarations),
    /// over declarations already resolved.
    pub(super) fn publish_resolved_declarations(
        &mut self,
        uri: &Uri,
        declarations: Arc<ResolvedDeclarations>,
        job_tx: &Sender<WorkerJob>,
    ) {
        let path = uri_to_path(uri);
        if self.declarations.get(&path) == Some(&declarations) {
            return;
        }
        self.declarations.insert(path.clone(), declarations.clone());
        let _ = job_tx.send(WorkerJob::Declarations { path, declarations });
    }
}

/// Refresh the named source's declarations before dispatching a read request.
/// Execute-command arguments use the same text-document descriptor one level down.
pub(super) fn publish_declarations_for_request(
    state: &mut GlobalState,
    req: &Request,
    job_tx: &Sender<WorkerJob>,
) {
    let document_uri = |value: &serde_json::Value| {
        value
            .get("textDocument")
            .and_then(|doc| doc.get("uri"))
            .and_then(serde_json::Value::as_str)
            .and_then(|uri| uri.parse::<Uri>().ok())
    };
    let Some(uri) = document_uri(&req.params).or_else(|| {
        req.params
            .get("arguments")
            .and_then(|args| args.get(0))
            .and_then(document_uri)
    }) else {
        return;
    };
    state.publish_declarations(&uri, job_tx);
}
