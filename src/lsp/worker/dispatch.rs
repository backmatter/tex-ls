use super::*;

impl Worker {
    pub(in crate::lsp) fn handle_job(&mut self, job: WorkerJob) {
        let enc = self.encoding;
        match job {
            WorkerJob::InspectAcquisition { id } => {
                let failures: Vec<_> = self
                    .acquisition_failures
                    .values()
                    .flat_map(|failures| {
                        failures.iter().map(
                            |(path, message)| serde_json::json!({"path":path,"message":message}),
                        )
                    })
                    .collect();
                let installation_issues: Vec<_> = self
                    .discovery_anchors
                    .values()
                    .flat_map(|(_, installed)| installed.issues())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let result = serde_json::json!({"failures":failures,"installationIssues":installation_issues,"sourceReadAttempts":file_acquisition::source_reads(), "compilerProbeAttempts":services::compiler_probes(), "pendingReads":self.pending_reads.len(), "pendingFileAcquisitions":self.files_inflight.len(), "pendingCompilerAcquisitions":self.compiler_acquisition.pending_count(), "installationLoading":!self.loading_indexes.is_empty(), "projectInspectionCommand":"tex-ls.inspectProject"});
                let _ = self
                    .out_tx
                    .send(Outbound::Response(Response::new_ok(id, result)));
            }
            WorkerJob::Colors {
                id,
                path,
                presentation,
            } => {
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests.lock().expect("ledger").contains(&id) {
                        return;
                    }
                    respond(id, &out_tx, || match presentation {
                        Some((range, color)) => {
                            serde_json::to_value(tex_ls_protocol::colors::color_presentations(
                                &snapshot, &path, enc, range, color,
                            ))
                            .expect("color presentations")
                        }
                        None => serde_json::to_value(tex_ls_protocol::colors::document_colors(
                            &snapshot, &path, enc,
                        ))
                        .expect("document colors"),
                    });
                });
            }
            WorkerJob::SemanticTokens { id, path, range } => {
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests.lock().expect("ledger").contains(&id) {
                        return;
                    }
                    respond(id, &out_tx, || {
                        tex_ls_protocol::semantic_tokens::compute(&snapshot, &path, enc, range)
                    });
                });
            }
            WorkerJob::WillRenameFiles { id, files } => {
                let snapshots: Vec<_> = self
                    .projects
                    .projects()
                    .map(|project| self.db.snapshot_for(project).expect("registered project"))
                    .collect();
                let result = tex_ls_protocol::file_operations::will_rename(&snapshots, &files, enc);
                let _ = self
                    .out_tx
                    .send(Outbound::Response(Response::new_ok(id, result)));
            }
            WorkerJob::DidRenameFiles { files } => {
                for (old, _) in &files {
                    if let Some(uri) = path_to_uri(old) {
                        self.pending.remove(&uri);
                    }
                }
                self.compiler_acquisition.clear(&mut self.db);
                self.projects.rename_files(&mut self.db, &files);
                self.seeded_dirs.clear();
                self.bib_lookups.clear();
                self.acquisition_keys.clear();
                self.parent_keys.clear();
                self.refresh(refresh::Feature::Folding);
                self.refresh(refresh::Feature::SemanticTokens);
            }
            WorkerJob::WorkspaceRoots { roots, open } => {
                self.compiler_acquisition.clear(&mut self.db);
                self.projects.update_roots(&mut self.db, &roots, &open);
                self.seeded_dirs.clear();
                self.bib_lookups.clear();
                self.acquisition_keys.clear();
                self.parent_keys.clear();
                self.refresh(refresh::Feature::Folding);
                self.refresh(refresh::Feature::SemanticTokens);
            }
            WorkerJob::Edit {
                stamp,
                uri,
                path,
                text,
                version,
                opened,
                kind,
                rules,
                build,
                texmf,
                declarations,
                exclude,
                edits,
            } => {
                // Write-phase: push the live buffer into the db. Cheap — the parse
                // is a lazy salsa query deferred to the analyze. Acquiring `&mut
                // db` blocks until any outstanding read snapshot drops (single
                // writer), which is how a fresher edit preempts an in-flight read.
                let project = self.project_for(&path);
                let package_before = self.package_token_inputs(&path);
                self.set_source_declarations(&path, &declarations);
                if opened {
                    self.bib_lookups.remove(&project);
                    self.extra_sources
                        .entry(project)
                        .or_default()
                        .insert(path.clone());
                    self.db
                        .replace_overlay(project, &path, text.text_arc())
                        .expect("registered project");
                } else {
                    self.db
                        .apply_project_change(project, &path, text.text_arc(), edits)
                        .expect("registered project");
                }
                if package_before != self.package_token_inputs(&path) {
                    self.refresh(refresh::Feature::SemanticTokens);
                }
                // Lazily pull the rest of the project off disk so cross-file rules
                // can fire. If this grows the member set, every open document's
                // resolution may have changed — re-lint them all.
                let parent_key = {
                    let snapshot = self.snapshot_for(&path);
                    let file = snapshot.lookup_file(&path).expect("edited source");
                    format!(
                        "{:?}{:?}{:?}",
                        build.root,
                        tex_ls_analysis::external::explicit_root_hints(
                            snapshot.file_text(file),
                            &path
                        ),
                        snapshot
                            .file_references(file)
                            .iter()
                            .map(|r| &r.candidates)
                            .collect::<Vec<_>>()
                    )
                };
                if self.parent_keys.get(&(project, path.clone())) != Some(&parent_key) {
                    self.parent_keys.insert((project, path.clone()), parent_key);
                    if let Some(root) = &build.root {
                        self.extra_sources
                            .entry(project)
                            .or_default()
                            .insert(root.clone());
                    }
                    self.acquisition_keys.remove(&(project, path.clone()));
                }
                self.seed_dir(&path, &exclude);
                self.acquire_files(project, &path, None, &texmf);
                self.acquire_compiler(project, &path, &build);
                self.enqueue(AnalyzeRequest {
                    stamp,
                    uri,
                    path,
                    version,
                    kind,
                    rules,
                });
            }
            WorkerJob::Declarations { path, declarations } => {
                // `set_declarations` no-ops on an unchanged value, so this is safe
                // to send defensively; the main loop's own mirror keeps it rare.
                self.set_source_declarations(&path, &declarations);
            }
            WorkerJob::Close { path } => {
                let project = self.project_for(&path);
                let before = self.source_version_at(project, &path);
                self.refresh_backing(project, &path, false);
                self.db
                    .close_overlay(project, &path)
                    .expect("registered project");
                if before != self.source_version_at(project, &path) {
                    let _ = self.out_tx.send(Outbound::RelintAll);
                    self.refresh(refresh::Feature::SemanticTokens);
                }
            }
            WorkerJob::WatchedChange {
                path,
                deleted,
                exclude,
            } => {
                let artifact = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| matches!(ext, "aux" | "log" | "fls"));
                // Source discovery excludes do not suppress build observations.
                if !artifact
                    && exclude.force_excludes(&path)
                    && self.snapshot_for(&path).lookup_file(&path).is_none()
                {
                    return;
                }
                // A filesystem refresh can change search-path resolution, including
                // an earlier cached miss or an alias whose target disappeared.
                self.bib_lookups.clear();
                self.acquisition_keys.clear();
                self.parent_keys.clear();
                if self.apply_watched_change(&path, deleted) {
                    let _ = self.out_tx.send(Outbound::RelintAll);
                    if !artifact {
                        self.refresh(refresh::Feature::SemanticTokens);
                    }
                }
            }
            WorkerJob::Format {
                id,
                path,
                style,
                kind,
                sentence_lang,
                sentence_no_break,
            } => {
                // Format reads run on the read pool against a snapshot, concurrent
                // with the analyze slot (they are id-bound responses, not coalesced).
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    let sentence =
                        SentenceOptions::from_resolved(sentence_lang, &sentence_no_break);
                    respond(id, &out_tx, || {
                        compute_format(&snapshot, &path, enc, style, kind, sentence)
                    })
                });
            }
            WorkerJob::RangeFormat {
                id,
                path,
                style,
                kind,
                ranges,
                sentence_lang,
                sentence_no_break,
            } => {
                // Range formatting runs on the read pool against a snapshot, exactly
                // like `Format` (an id-bound response, not coalesced).
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    let sentence =
                        SentenceOptions::from_resolved(sentence_lang, &sentence_no_break);
                    respond(id, &out_tx, || {
                        tex_ls_protocol::formatting::compute_ranges_format(
                            &snapshot, &path, enc, style, kind, &ranges, sentence,
                        )
                    })
                });
            }
            WorkerJob::OnTypeFormat {
                id,
                path,
                style,
                kind,
                position,
                sentence_lang,
                sentence_no_break,
            } => {
                // On-type formatting reads on the read pool against a snapshot,
                // exactly like `RangeFormat` (an id-bound response, not coalesced).
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    let sentence =
                        SentenceOptions::from_resolved(sentence_lang, &sentence_no_break);
                    respond(id, &out_tx, || {
                        compute_on_type_format(
                            &snapshot, &path, enc, style, kind, position, sentence,
                        )
                    })
                });
            }
            WorkerJob::InlayHints {
                id,
                path,
                range,
                options,
                build: _,
            } => {
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    respond(id, &out_tx, || {
                        tex_ls_protocol::hints::compute(&snapshot, &path, range, enc, &options)
                    });
                });
            }
            WorkerJob::Symbols {
                id,
                path,
                kind,
                build: _,
                options,
            } => {
                // Symbol reads, like formatting, run on the read pool against a
                // snapshot (id-bound responses, not coalesced).
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_symbols(&snapshot, id, &path, enc, kind, &options, &out_tx)
                });
            }
            WorkerJob::WorkspaceSymbols {
                id,
                query,
                options,
                active,
            } => {
                // Workspace symbols scan every file in the database snapshot.
                let permit = self.read_spawner.reserve();
                let snapshots: Vec<_> = self
                    .projects
                    .projects()
                    .map(|project| {
                        self.db
                            .snapshot_for(project)
                            .expect("registered project")
                            .with_cancellation(
                                self.requests.lock().expect("ledger").cancellation(&id),
                            )
                    })
                    .collect();
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_workspace_symbols(
                        &snapshots,
                        id,
                        &query,
                        enc,
                        &options,
                        active.as_deref(),
                        &out_tx,
                    )
                });
            }
            WorkerJob::FoldingRange { id, path, kind } => {
                // Folding reads run on the read pool against a snapshot, like
                // symbols (id-bound responses, not coalesced).
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_folding(&snapshot, id, &path, enc, kind, &out_tx)
                });
            }
            WorkerJob::SelectionRange {
                id,
                path,
                kind,
                positions,
            } => {
                // Selection ranges run on the read pool against a snapshot, like
                // folding (single-file, id-bound responses).
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_selection_range(&snapshot, id, &path, enc, kind, &positions, &out_tx)
                });
            }
            WorkerJob::DocumentLink {
                id,
                path,
                kind,
                texmf,
            } => {
                // Document links run on the read pool against a snapshot, like
                // folding (single-file, id-bound responses). Resolution is positional
                // and disk-aware, so no project membership snapshot is needed. The
                // TEXMF index is built/consulted here (off the main loop).
                let project = self.project_for(&path);
                let _ = (project, texmf);
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_document_link(&snapshot, id, &path, enc, kind, &out_tx)
                });
            }
            WorkerJob::Completion {
                id,
                uri,
                position,
                texmf,
            } => {
                // Only file URIs acquire native inputs. Untitled buffers retain
                // the synthetic source identity used by synchronization and can
                // complete against their captured text and local declarations.
                let path = uri_to_path(&uri);
                let _ = texmf;
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_completion(&snapshot, id, &uri, enc, position, &out_tx)
                });
            }
            WorkerJob::ResolveCompletion { id, item } => {
                let permit = self.read_spawner.reserve();
                let snapshot = completion_resolve::source_path(&item)
                    .map(|path| self.snapshot_for(&path))
                    .unwrap_or_else(|| self.db.snapshot());
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_completion_resolve(&snapshot, id, *item, &out_tx)
                });
            }
            WorkerJob::Hover {
                id,
                path,
                position,
                build: _,
            } => {
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_hover(&snapshot, id, &path, enc, position, &out_tx)
                });
            }
            WorkerJob::ForwardSearch {
                id,
                path,
                line,
                build,
                executable,
                args,
            } => {
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_forward_search(
                        &snapshot,
                        id,
                        &path,
                        line,
                        &build,
                        &executable,
                        &args,
                        &out_tx,
                    )
                });
            }
            WorkerJob::SignatureHelp { id, path, position } => {
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_signature_help(&snapshot, id, &path, position, enc, &out_tx)
                });
            }
            WorkerJob::GotoDefinition {
                id,
                path,
                position,
                texmf,
            } => {
                let project = self.project_for(&path);
                let _ = (project, texmf);
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_goto_definition(&snapshot, id, &path, position, enc, &out_tx)
                });
            }
            WorkerJob::References {
                id,
                path,
                position,
                include_declaration,
            } => {
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_references(
                        &snapshot,
                        id,
                        &path,
                        position,
                        include_declaration,
                        enc,
                        &out_tx,
                    )
                });
            }
            WorkerJob::DocumentHighlight { id, path, position } => {
                // Single-file like prepareRename: no project membership, just a db
                // snapshot to reach the cached model when the buffer is current.
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_document_highlight(&snapshot, id, &path, enc, position, &out_tx)
                });
            }
            WorkerJob::PrepareRename { id, path, position } => {
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_prepare_rename(&snapshot, id, &path, enc, position, &out_tx)
                });
            }
            WorkerJob::Rename {
                id,
                path,
                position,
                new_name,
            } => {
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_rename(&snapshot, id, &path, position, &new_name, enc, &out_tx)
                });
            }
            WorkerJob::InspectProject { id, path } => {
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let result = tex_ls_protocol::projects::inspect(&snapshot, &path);
                let _ = self
                    .out_tx
                    .send(Outbound::Response(Response::new_ok(id, result)));
            }
            WorkerJob::LinkedEditing { id, path, position } => {
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    run_linked_editing(&snapshot, id, &path, enc, position, &out_tx)
                });
            }
            WorkerJob::WorkspaceDiagnostic {
                id,
                previous,
                partial,
            } => {
                let paths = self
                    .projects
                    .projects()
                    .flat_map(|project| {
                        self.db
                            .snapshot_for(project)
                            .expect("registered project")
                            .tracked_files()
                            .into_iter()
                            .map(|(path, _)| path)
                    })
                    .collect();
                let _ = self.out_tx.send(Outbound::WorkspaceDiagnosticSettings(
                    WorkspaceDiagnosticSources {
                        id,
                        previous,
                        partial,
                        paths,
                    },
                ));
            }
            WorkerJob::WorkspaceDiagnosticReport { request, settings } => {
                let WorkspaceDiagnosticSources {
                    id,
                    previous,
                    partial,
                    ..
                } = request;
                let paths: Vec<_> = self
                    .projects
                    .projects()
                    .flat_map(|project| {
                        // This temporary snapshot drops with the owned file list;
                        // no analysis storage is retained while reserving capacity.
                        self.db
                            .snapshot_for(project)
                            .expect("registered project")
                            .tracked_files()
                    })
                    .map(|(path, _)| path)
                    .filter(|path| path_to_uri(path).is_some())
                    .collect();
                if paths.iter().any(|path| !settings.contains_key(path)) {
                    let _ = self.out_tx.send(Outbound::WorkspaceDiagnosticSettings(
                        WorkspaceDiagnosticSources {
                            id,
                            previous,
                            partial,
                            paths,
                        },
                    ));
                    return;
                }
                let permit = self.read_spawner.reserve();
                let snapshots: Vec<_> = self
                    .projects
                    .projects()
                    .map(|project| {
                        self.db
                            .snapshot_for(project)
                            .expect("registered project")
                            .with_cancellation(
                                self.requests.lock().expect("ledger").cancellation(&id),
                            )
                    })
                    .collect();
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    respond_diagnostic(id.clone(), &out_tx, || {
                        let mut items = Vec::new();
                        tex_ls_protocol::diagnostic_store::stream_workspace_diagnostics(
                            &snapshots,
                            &previous,
                            enc,
                            |path| settings[path].0.rule_selection(),
                            |path| settings[path].1,
                            |batch| {
                                if !requests.lock().expect("request ledger").contains(&id) {
                                    return false;
                                }
                                if let Some(token) = &partial {
                                    let _ = out_tx.send(Outbound::Progress {
                                        id: id.clone(),
                                        token: token.clone(),
                                        value: serde_json::json!({"items":batch}),
                                    });
                                } else {
                                    items.extend(batch);
                                }
                                true
                            },
                        );
                        serde_json::json!({"items":items})
                    });
                });
            }
            WorkerJob::Diagnostic {
                id,
                path,
                kind,
                previous_result_id,
                rules,
                build: _,
            } => {
                // On-demand pull is a free, id-bound read—not the coalesced analyze
                // slot—so it never blocks or supersedes the push analyze.
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    respond_diagnostic(id, &out_tx, || {
                        tex_ls_protocol::diagnostic_store::document_diagnostics(
                            &snapshot,
                            &path,
                            kind,
                            &rules,
                            enc,
                            previous_result_id.as_deref(),
                        )
                    })
                });
            }
            WorkerJob::CodeAction {
                id,
                uri,
                path,
                kind,
                range,
                only,
                rules,
            } => {
                // On-demand re-lint, like the pull-diagnostics path, runs against a
                // snapshot on the read pool.
                let permit = self.read_spawner.reserve();
                let snapshot = self
                    .snapshot_for(&path)
                    .with_cancellation(self.requests.lock().expect("ledger").cancellation(&id));
                let out_tx = self.out_tx.clone();
                let requests = self.requests.clone();
                let request_id = id.clone();
                self.read_spawner.spawn_reserved(permit, move || {
                    if !requests
                        .lock()
                        .expect("request ledger")
                        .contains(&request_id)
                    {
                        return;
                    }
                    respond(id, &out_tx, || {
                        compute_code_actions(
                            &snapshot,
                            &uri,
                            &path,
                            kind,
                            range,
                            only.as_deref(),
                            &rules,
                            enc,
                        )
                    })
                });
            }
        }
    }
}
