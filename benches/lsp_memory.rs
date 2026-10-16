//! Live heap retained by equivalent language-server histories.
//!
//! The harness drives the real in-process LSP server and compares sessions that
//! end with the same open buffers and project membership. A counting wrapper
//! around the system allocator measures live requested bytes, rather than RSS,
//! so allocator pages that are available for reuse do not look like a leak.
//! Opens and final measurements wait for a quiet acquisition window. Diagnostic
//! pulls honor explicit server retry requests within one 30-second deadline.
//! Other protocol errors still fail the benchmark.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    ClientCapabilities, DiagnosticClientCapabilities, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, DocumentDiagnosticParams, HoverParams, InitializeParams,
    InitializedParams, Position, TextDocumentClientCapabilities, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, Uri, VersionedTextDocumentIdentifier,
    WorkDoneProgressParams,
};

#[path = "../examples/support/allocations.rs"]
mod allocations;
use allocations::{live_bytes, peak_bytes, reset_peak};

fn begin_measurement() -> usize {
    let baseline = live_bytes();
    reset_peak();
    baseline
}

#[derive(Clone, Copy)]
struct Memory {
    live: usize,
    peak: usize,
}

fn finish_measurement(baseline: usize) -> Memory {
    Memory {
        live: live_bytes().saturating_sub(baseline),
        peak: peak_bytes().saturating_sub(baseline),
    }
}

const ACQUISITION_QUIET: Duration = Duration::from_millis(250);
const ACQUISITION_POLL: Duration = Duration::from_millis(10);

/// A zero-pending sample can precede a follow-up compiler batch. Require a quiet
/// interval with unchanged status, including source-read and compiler-probe counts.
struct AcquisitionWait {
    deadline: Instant,
    quiet: Duration,
    quiet_since: Option<Instant>,
    last_status: Option<serde_json::Value>,
}

impl AcquisitionWait {
    fn new(deadline: Instant, quiet: Duration) -> Self {
        Self {
            deadline,
            quiet,
            quiet_since: None,
            last_status: None,
        }
    }

    fn observe(&mut self, status: serde_json::Value, now: Instant) -> bool {
        assert!(
            now < self.deadline,
            "acquisition did not settle before the deadline"
        );
        let count = |field| {
            status
                .get(field)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_else(|| panic!("invalid acquisition status field {field}: {status}"))
        };
        let reads = count("pendingReads");
        let files = count("pendingFileAcquisitions");
        let compiler = count("pendingCompilerAcquisitions");
        // These counters must be present even while acquisition is busy.
        let _ = count("sourceReadAttempts");
        let _ = count("compilerProbeAttempts");
        let loading = status
            .get("installationLoading")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or_else(|| panic!("invalid acquisition installation status: {status}"));
        if reads == 0 && files == 0 && compiler == 0 && !loading {
            if self.last_status.as_ref() != Some(&status) || self.quiet_since.is_none() {
                self.quiet_since = Some(now);
            }
        } else {
            self.quiet_since = None;
        }
        self.last_status = Some(status);
        self.quiet_since
            .is_some_and(|since| now.duration_since(since) >= self.quiet)
    }
}

struct Client {
    connection: Connection,
    server: Option<JoinHandle<()>>,
    next_id: i32,
}

impl Client {
    fn start() -> Self {
        let (server, connection) = Connection::memory();
        let thread = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());
        let mut client = Self {
            connection,
            server: Some(thread),
            next_id: 1,
        };
        let params = InitializeParams {
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    diagnostic: Some(DiagnosticClientCapabilities::default()),
                    semantic_tokens: Some(serde_json::from_value(serde_json::json!({
                        "requests": {"full": true}, "formats": ["relative"],
                        "tokenTypes": tex_ls_protocol::semantic_tokens::TYPES.iter()
                            .filter(|kind| **kind != "texEmbedded").copied().collect::<Vec<_>>(),
                        "tokenModifiers": tex_ls_protocol::semantic_tokens::MODIFIERS,
                    })).expect("semantic-token capabilities")),
                    ..Default::default()
                }),
                ..Default::default()
            },
            initialization_options: Some(serde_json::json!({"texmf": {"enabled": false}})),
            ..Default::default()
        };
        let response = client.request("initialize", serde_json::to_value(params).unwrap());
        assert!(
            response.response_result.is_ok(),
            "initialize failed: {response:?}"
        );
        client.notify(
            "initialized",
            serde_json::to_value(InitializedParams {}).unwrap(),
        );
        client
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> Response {
        self.request_until(method, params, Instant::now() + Duration::from_secs(30))
    }

    fn request_until(
        &mut self,
        method: &str,
        params: serde_json::Value,
        deadline: Instant,
    ) -> Response {
        self.request_until_with_clock(method, params, deadline, Instant::now)
    }

    fn request_until_with_clock(
        &mut self,
        method: &str,
        params: serde_json::Value,
        deadline: Instant,
        mut now: impl FnMut() -> Instant,
    ) -> Response {
        assert!(
            now() < deadline,
            "timed out waiting for the language server"
        );
        let id = RequestId::from(self.next_id);
        self.next_id += 1;
        self.connection
            .sender
            .send(Message::Request(Request {
                id: id.clone(),
                method: method.to_owned(),
                params,
            }))
            .unwrap();
        loop {
            let remaining = deadline
                .checked_duration_since(now())
                .filter(|remaining| !remaining.is_zero())
                .expect("timed out waiting for the language server");
            let message = self
                .connection
                .receiver
                .recv_timeout(remaining)
                .expect("timed out waiting for the language server");
            assert!(
                now() < deadline,
                "timed out waiting for the language server"
            );
            match message {
                Message::Response(response) if response.id == id => return response,
                Message::Response(response) => {
                    panic!("expected response {id:?}, got {:?}", response.id)
                }
                Message::Notification(_) => {}
                Message::Request(request) => {
                    self.connection
                        .sender
                        .send(Message::Response(Response::new_ok(
                            request.id,
                            serde_json::Value::Null,
                        )))
                        .unwrap();
                }
            }
        }
    }

    fn notify(&self, method: &str, params: serde_json::Value) {
        self.connection
            .sender
            .send(Message::Notification(Notification {
                method: method.to_owned(),
                params,
            }))
            .unwrap();
    }

    fn open(&self, uri: &Uri, version: i32, text: &str) {
        self.notify(
            "textDocument/didOpen",
            serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "latex".into(),
                    version,
                    text: text.to_owned(),
                },
            })
            .unwrap(),
        );
    }

    fn change(&self, uri: &Uri, version: i32, text: &str) {
        self.notify(
            "textDocument/didChange",
            serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    text_document_identifier: lsp_types::TextDocumentIdentifier {
                        uri: uri.clone(),
                    },
                    version,
                },
                content_changes: vec![
                    lsp_types::TextDocumentContentChangeWholeDocument {
                        text: text.to_owned(),
                    }
                    .into(),
                ],
            })
            .unwrap(),
        );
    }

    fn ready(&mut self) {
        self.ready_until(Instant::now() + Duration::from_secs(30), ACQUISITION_QUIET);
    }

    fn ready_until(&mut self, deadline: Instant, quiet: Duration) {
        let mut wait = AcquisitionWait::new(deadline, quiet);
        loop {
            let response = self.request_until(
                "workspace/executeCommand",
                serde_json::json!({"command": "tex-ls.inspectAcquisition", "arguments": []}),
                deadline,
            );
            let status = response
                .response_result
                .expect("acquisition status request failed");
            if wait.observe(status, Instant::now()) {
                return;
            }
            std::thread::sleep(
                ACQUISITION_POLL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    fn diagnostic(&mut self, uri: &Uri) {
        self.diagnostic_until(uri, Instant::now() + Duration::from_secs(30));
    }

    fn diagnostic_until(&mut self, uri: &Uri, deadline: Instant) {
        let params = serde_json::to_value(DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            identifier: None,
            previous_result_id: None,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap();
        loop {
            let response = self.request_until("textDocument/diagnostic", params.clone(), deadline);
            if response.response_result.as_ref().is_err_and(|error| {
                error.code == lsp_server::ErrorCode::ServerCancelled as i32
                    && error
                        .data
                        .as_ref()
                        .and_then(|data| data.get("retriggerRequest"))
                        == Some(&serde_json::Value::Bool(true))
            }) {
                continue;
            }
            assert!(
                response.response_result.is_ok(),
                "diagnostic failed: {response:?}"
            );
            return;
        }
    }

    fn hover(&mut self, uri: &Uri) {
        let response = self.request(
            "textDocument/hover",
            serde_json::to_value(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(4, 2),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .unwrap(),
        );
        assert!(
            response.response_result.is_ok(),
            "hover failed: {response:?}"
        );
    }

    fn barrier(&mut self, uri: &Uri) {
        self.diagnostic(uri);
        self.hover(uri);
        let response = self.request(
            "textDocument/semanticTokens/full",
            serde_json::json!({"textDocument": {"uri": uri}}),
        );
        assert!(
            response.response_result.is_ok(),
            "semantic tokens failed: {response:?}"
        );
        for method in [
            "textDocument/definition",
            "textDocument/references",
            "textDocument/documentHighlight",
            "textDocument/rename",
        ] {
            let mut params = serde_json::json!({"textDocument": {"uri": uri}, "position": {"line": 4, "character": 34}});
            if method == "textDocument/references" {
                params["context"] = serde_json::json!({"includeDeclaration": true});
            } else if method == "textDocument/rename" {
                params["newName"] = "replacement".into();
            }
            let response = self.request(method, params);
            assert!(
                response.response_result.is_ok(),
                "{method} failed: {response:?}"
            );
        }
    }

    fn shutdown(mut self) {
        let response = self.request("shutdown", serde_json::Value::Null);
        assert!(
            response.response_result.is_ok(),
            "shutdown failed: {response:?}"
        );
        self.notify("exit", serde_json::Value::Null);
        self.server
            .take()
            .unwrap()
            .join()
            .expect("language-server thread panicked");
    }
}

fn path_to_uri(path: &Path) -> Uri {
    let mut path = path.display().to_string().replace('\\', "/");
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    format!("file://{path}").parse().expect("valid file URI")
}

fn query_session(path: &Path, generations: usize, change_text: bool) -> Memory {
    let baseline = begin_measurement();
    let mut client = Client::start();
    let uri = path_to_uri(path);
    let base = fs::read_to_string(path).unwrap();
    let changed = format!("{base}% changed\n");
    client.open(&uri, 1, &base);
    client.ready();
    client.barrier(&uri);
    for generation in 1..=generations {
        let text = if change_text && generation % 2 == 1 {
            &changed
        } else {
            &base
        };
        client.change(&uri, generation as i32 + 1, text);
        client.barrier(&uri);
    }
    client.ready();
    let memory = finish_measurement(baseline);
    client.shutdown();
    memory
}

struct ProjectCorpus {
    _dir: tempfile::TempDir,
    root: PathBuf,
    leaves: Vec<PathBuf>,
}

fn project_corpus(generations: usize) -> ProjectCorpus {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("all.tex");
    fs::write(
        &root,
        "\\documentclass{article}\n% root\n% root\n% root\n\\section{All}\n",
    )
    .unwrap();
    let mut leaves = Vec::with_capacity(generations);
    for generation in 0..generations {
        let group = dir.path().join(format!("group{generation:03}"));
        fs::create_dir(&group).unwrap();
        let main = group.join("main.tex");
        fs::write(
            &main,
            format!(
                "\\documentclass{{article}}\n\\usepackage{{local}}\n\\addbibresource{{refs.bib}}\n\\input{{part}}\n\\section{{Group {generation}}}\\ref{{label{generation}}}\\cite{{key{generation}}}\n"
            ),
        )
        .unwrap();
        fs::write(
            group.join("part.tex"),
            format!("\\label{{label{generation}}}\n"),
        )
        .unwrap();
        fs::write(
            group.join("local.sty"),
            format!("\\newcommand{{\\local{generation}}}{{value}}\n"),
        )
        .unwrap();
        fs::write(
            group.join("refs.bib"),
            format!("@article{{key{generation}, title={{Title {generation}}}}}\n"),
        )
        .unwrap();
        leaves.push(main);
    }
    ProjectCorpus {
        _dir: dir,
        root,
        leaves,
    }
}

fn project_session(corpus: &ProjectCorpus, progressive: bool) -> Memory {
    let baseline = begin_measurement();
    let mut client = Client::start();
    let mut paths = Vec::with_capacity(corpus.leaves.len() + 1);
    if progressive {
        paths.extend(corpus.leaves.iter());
        paths.push(&corpus.root);
    } else {
        paths.push(&corpus.root);
        paths.extend(corpus.leaves.iter());
    }
    for (index, path) in paths.into_iter().enumerate() {
        let uri = path_to_uri(path);
        let text = fs::read_to_string(path).unwrap();
        client.open(&uri, index as i32 + 1, &text);
        client.ready();
        client.barrier(&uri);
    }
    client.ready();
    let memory = finish_measurement(baseline);
    client.shutdown();
    memory
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn warm_up() {
    let corpus = project_corpus(1);
    let _ = project_session(&corpus, false);
}

fn main() {
    let scenario = std::env::var("TEX_LS_MEMORY_SCENARIO").unwrap_or_else(|_| "all".to_owned());
    let query_generations = env_usize("TEX_LS_MEMORY_QUERY_GENERATIONS", 10_000);
    let project_generations = env_usize("TEX_LS_MEMORY_PROJECT_GENERATIONS", 48);
    let assert = std::env::var_os("TEX_LS_MEMORY_ASSERT").is_some();
    assert!(
        matches!(scenario.as_str(), "all" | "query-log" | "project"),
        "TEX_LS_MEMORY_SCENARIO must be all, query-log, or project"
    );

    warm_up();
    let scratch = tempfile::tempdir().unwrap();
    let query_path = scratch.path().join("query.tex");
    fs::write(
        &query_path,
        "\\documentclass{article}\n% query\n% query\n% query\n\\section{Query}\\label{demo}\\ref{demo} é😀\n",
    )
    .unwrap();

    let query = matches!(scenario.as_str(), "all" | "query-log").then(|| {
        let no_op = query_session(&query_path, query_generations, false);
        let changed = query_session(&query_path, query_generations, true);
        (no_op, changed)
    });
    let project = matches!(scenario.as_str(), "all" | "project").then(|| {
        let corpus = project_corpus(project_generations);
        let one_shot = project_session(&corpus, false);
        let progressive = project_session(&corpus, true);
        (one_shot, progressive)
    });

    let mut output = serde_json::Map::new();
    output.insert("query_generations".to_owned(), query_generations.into());
    output.insert("project_generations".to_owned(), project_generations.into());
    if let Some((no_op, changed)) = query {
        let excess = changed.live.saturating_sub(no_op.live);
        println!(
            "query log: no-op {:.2} MiB live / {:.2} peak; changed {:.2} MiB live / {:.2} peak; excess {:.2} MiB",
            mib(no_op.live),
            mib(no_op.peak),
            mib(changed.live),
            mib(changed.peak),
            mib(excess),
        );
        output.insert(
            "query_log".to_owned(),
            serde_json::json!({
                "no_op_live_bytes": no_op.live,
                "no_op_peak_bytes": no_op.peak,
                "changed_live_bytes": changed.live,
                "changed_peak_bytes": changed.peak,
                "excess_bytes": excess,
            }),
        );
        if assert {
            assert!(
                excess <= 1024 * 1024,
                "query history retained {:.2} MiB (limit 1 MiB)",
                mib(excess)
            );
        }
    }
    if let Some((one_shot, progressive)) = project {
        let excess = progressive.live.saturating_sub(one_shot.live);
        let limit = (one_shot.live / 10).max(1024 * 1024);
        println!(
            "project: one-shot {:.2} MiB live / {:.2} peak; progressive {:.2} MiB live / {:.2} peak; excess {:.2} MiB",
            mib(one_shot.live),
            mib(one_shot.peak),
            mib(progressive.live),
            mib(progressive.peak),
            mib(excess),
        );
        output.insert(
            "project".to_owned(),
            serde_json::json!({
                "one_shot_live_bytes": one_shot.live,
                "one_shot_peak_bytes": one_shot.peak,
                "progressive_live_bytes": progressive.live,
                "progressive_peak_bytes": progressive.peak,
                "excess_bytes": excess,
                "gate_limit_bytes": limit,
            }),
        );
        if assert {
            assert!(
                excess <= limit,
                "project history retained {:.2} MiB (limit {:.2} MiB)",
                mib(excess),
                mib(limit)
            );
        }
    }

    if let Some(path) = std::env::var_os("TEX_LS_MEMORY_OUTPUT_JSON") {
        fs::write(
            path,
            serde_json::to_string_pretty(&serde_json::Value::Object(output)).unwrap(),
        )
        .unwrap();
    }
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "This benchmark has no test harness; its tests run through tests/lsp_memory_client.rs"
)]
mod client_tests {
    use super::*;

    fn cancelled(code: lsp_server::ErrorCode, retrigger: Option<bool>) -> Response {
        Response {
            id: RequestId::from(0),
            response_result: Err(lsp_server::ResponseError {
                code: code as i32,
                message: "controlled cancellation".into(),
                data: retrigger.map(|value| serde_json::json!({"retriggerRequest":value})),
            }),
        }
    }

    fn successful() -> Response {
        Response::new_ok(
            RequestId::from(0),
            serde_json::json!({"kind":"full","items":[]}),
        )
    }

    fn exercise_client(
        script: Vec<Response>,
        action: impl FnOnce(&mut Client),
    ) -> (bool, Vec<Request>) {
        let (server, connection) = Connection::memory();
        let server_thread = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for mut response in script {
                let message = match server.receiver.recv_timeout(Duration::from_secs(5)) {
                    Ok(message) => message,
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    Err(error) => panic!("test client did not complete: {error}"),
                };
                let Message::Request(request) = message else {
                    panic!("diagnostic request");
                };
                response.id = request.id.clone();
                requests.push(request);
                if server.sender.send(Message::Response(response)).is_err() {
                    break;
                }
            }
            requests
        });
        let mut client = Client {
            connection,
            server: None,
            next_id: 1,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| action(&mut client)));
        drop(client);
        (result.is_ok(), server_thread.join().unwrap())
    }

    fn exercise(script: Vec<Response>, expired: bool) -> (bool, Vec<Request>) {
        exercise_client(script, |client| {
            let uri = "file:///memory-client.tex".parse().unwrap();
            if expired {
                client.diagnostic_until(&uri, Instant::now() - Duration::from_secs(1));
            } else {
                client.diagnostic(&uri);
            }
        })
    }

    fn status() -> serde_json::Value {
        serde_json::json!({
            "pendingReads": 0, "pendingFileAcquisitions": 0,
            "pendingCompilerAcquisitions": 0, "installationLoading": false,
            "sourceReadAttempts": 5, "compilerProbeAttempts": 8, "failures": []
        })
    }

    fn status_response(status: serde_json::Value) -> Response {
        Response::new_ok(RequestId::from(0), status)
    }

    #[test]
    fn request_deadline_stops_buffered_notification_traffic() {
        let (server, connection) = Connection::memory();
        server
            .sender
            .send(Message::Notification(Notification {
                method: "test/notification".into(),
                params: serde_json::Value::Null,
            }))
            .unwrap();
        server
            .sender
            .send(Message::Response(Response::new_ok(
                RequestId::from(1),
                serde_json::Value::Null,
            )))
            .unwrap();
        let mut client = Client {
            connection,
            server: None,
            next_id: 1,
        };
        let start = Instant::now();
        let deadline = start + Duration::from_secs(30);
        // The notification arrives before expiry; the next receive starts at expiry.
        let mut ticks = [start, start, start, deadline].into_iter();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.request_until_with_clock(
                "test/request",
                serde_json::Value::Null,
                deadline,
                || ticks.next().unwrap_or(deadline),
            );
        }));
        assert!(
            result.is_err(),
            "buffered traffic must not extend the deadline"
        );
        assert!(ticks.next().is_none(), "the receive loop must check expiry");
        assert!(matches!(
            client.connection.receiver.try_recv().unwrap(),
            Message::Response(_)
        ));
    }

    #[test]
    fn request_deadline_rejects_a_response_received_at_expiry() {
        let (server, connection) = Connection::memory();
        server
            .sender
            .send(Message::Response(Response::new_ok(
                RequestId::from(1),
                serde_json::Value::Null,
            )))
            .unwrap();
        let mut client = Client {
            connection,
            server: None,
            next_id: 1,
        };
        let start = Instant::now();
        let deadline = start + Duration::from_secs(30);
        // Expiry occurs after receive, before the matching response is accepted.
        let mut ticks = [start, start, deadline].into_iter();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.request_until_with_clock(
                "test/request",
                serde_json::Value::Null,
                deadline,
                || ticks.next().unwrap_or(deadline),
            );
        }));
        assert!(result.is_err(), "late responses must fail the strict gate");
        assert!(
            ticks.next().is_none(),
            "responses must check expiry after receive"
        );
    }

    #[test]
    fn acquisition_requires_a_quiet_interval_after_each_busy_kind() {
        for (field, busy) in [
            ("pendingReads", serde_json::json!(1)),
            ("pendingFileAcquisitions", serde_json::json!(1)),
            ("pendingCompilerAcquisitions", serde_json::json!(1)),
            ("installationLoading", serde_json::json!(true)),
        ] {
            let start = Instant::now();
            let mut wait = AcquisitionWait::new(start + Duration::from_secs(30), ACQUISITION_QUIET);
            let mut pending = status();
            pending[field] = busy;
            let at = |millis| start + Duration::from_millis(millis);
            assert!(!wait.observe(status(), at(0)));
            assert!(!wait.observe(pending, at(100)), "{field} is still busy");
            assert!(!wait.observe(status(), at(200)));
            assert!(!wait.observe(status(), at(449)));
            assert!(
                wait.observe(status(), at(450)),
                "{field} must restart the quiet interval"
            );
        }
    }

    #[test]
    fn acquisition_counter_and_status_changes_restart_quiet_interval() {
        let start = Instant::now();
        let mut wait = AcquisitionWait::new(start + Duration::from_secs(30), ACQUISITION_QUIET);
        let at = |millis| start + Duration::from_millis(millis);
        let mut sample = status();
        assert!(!wait.observe(sample.clone(), at(0)));
        sample["sourceReadAttempts"] = 6.into();
        assert!(!wait.observe(sample.clone(), at(200)));
        sample["compilerProbeAttempts"] = 9.into();
        assert!(!wait.observe(sample.clone(), at(400)));
        sample["failures"] = serde_json::json!(["new observation"]);
        assert!(!wait.observe(sample.clone(), at(600)));
        assert!(!wait.observe(sample.clone(), at(849)));
        assert!(wait.observe(sample, at(850)));
    }

    #[test]
    fn acquisition_deadline_is_absolute_even_for_zero_pending_samples() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(30);
        let mut wait = AcquisitionWait::new(deadline, ACQUISITION_QUIET);
        assert!(!wait.observe(status(), deadline - Duration::from_millis(1)));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            wait.observe(status(), deadline)
        }));
        assert!(
            result.is_err(),
            "zero pending work must not reset the deadline"
        );
    }

    #[test]
    fn acquisition_client_polls_pending_status_and_uses_inspection_command() {
        let mut pending = status();
        pending["pendingCompilerAcquisitions"] = 1.into();
        let (succeeded, requests) = exercise_client(
            vec![status_response(pending), status_response(status())],
            |client| client.ready_until(Instant::now() + Duration::from_secs(30), Duration::ZERO),
        );
        assert!(succeeded);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].id, RequestId::from(1));
        assert_eq!(requests[1].id, RequestId::from(2));
        assert!(requests.iter().all(|request| {
            request.method == "workspace/executeCommand"
                && request.params
                    == serde_json::json!({
                        "command": "tex-ls.inspectAcquisition", "arguments": []
                    })
        }));
    }

    #[test]
    fn acquisition_client_expired_deadline_sends_no_request() {
        let (succeeded, requests) = exercise_client(vec![status_response(status())], |client| {
            client.ready_until(Instant::now() - Duration::from_secs(1), ACQUISITION_QUIET);
        });
        assert!(!succeeded);
        assert!(requests.is_empty());
    }

    #[test]
    fn acquisition_client_keeps_status_errors_fatal() {
        for code in [
            lsp_server::ErrorCode::InternalError,
            lsp_server::ErrorCode::ServerCancelled,
            lsp_server::ErrorCode::ContentModified,
        ] {
            let (succeeded, requests) = exercise_client(
                vec![cancelled(code, Some(true)), status_response(status())],
                |client| {
                    client.ready_until(Instant::now() + Duration::from_secs(30), Duration::ZERO)
                },
            );
            assert!(!succeeded, "inspection errors must fail the strict gate");
            assert_eq!(requests.len(), 1, "inspection errors must never retry");
        }
    }

    #[test]
    fn acquisition_client_rejects_missing_or_malformed_status_fields() {
        let fields = [
            "pendingReads",
            "pendingFileAcquisitions",
            "pendingCompilerAcquisitions",
            "sourceReadAttempts",
            "compilerProbeAttempts",
            "installationLoading",
        ];
        let mut malformed = vec![serde_json::Value::Null, serde_json::json!([])];
        for field in fields {
            let mut missing = status();
            missing.as_object_mut().unwrap().remove(field);
            malformed.push(missing);
            for value in [
                serde_json::Value::Null,
                serde_json::json!("zero"),
                serde_json::json!(-1),
            ] {
                let mut sample = status();
                sample[field] = value;
                malformed.push(sample);
            }
        }
        let mut numeric_loading = status();
        numeric_loading["installationLoading"] = 0.into();
        malformed.push(numeric_loading);
        let mut boolean_count = status();
        boolean_count["pendingReads"] = false.into();
        malformed.push(boolean_count);
        for sample in malformed {
            let (succeeded, requests) = exercise_client(vec![status_response(sample)], |client| {
                client.ready_until(Instant::now() + Duration::from_secs(30), Duration::ZERO);
            });
            assert!(
                !succeeded,
                "malformed acquisition status must fail the strict gate"
            );
            assert_eq!(requests.len(), 1);
        }
    }

    #[test]
    fn diagnostic_retriggers_only_explicit_write_cancellations() {
        let (succeeded, requests) = exercise(
            vec![
                cancelled(lsp_server::ErrorCode::ServerCancelled, Some(true)),
                cancelled(lsp_server::ErrorCode::ServerCancelled, Some(true)),
                successful(),
            ],
            false,
        );
        assert!(succeeded, "legal retry must reach a successful diagnostic");
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .map(|request| &request.id)
                .collect::<Vec<_>>(),
            vec![
                &RequestId::from(1),
                &RequestId::from(2),
                &RequestId::from(3)
            ]
        );
        assert!(
            requests
                .iter()
                .all(|request| request.method == "textDocument/diagnostic"
                    && request.params == requests[0].params)
        );
    }

    #[test]
    fn diagnostic_keeps_genuine_and_unrequested_errors_fatal() {
        for response in [
            cancelled(lsp_server::ErrorCode::InternalError, Some(true)),
            cancelled(lsp_server::ErrorCode::ServerCancelled, None),
            cancelled(lsp_server::ErrorCode::ServerCancelled, Some(false)),
            cancelled(lsp_server::ErrorCode::RequestCanceled, Some(true)),
            cancelled(lsp_server::ErrorCode::ContentModified, Some(true)),
        ] {
            let (succeeded, requests) = exercise(vec![response, successful()], false);
            assert!(!succeeded, "unexpected errors must fail the strict gate");
            assert_eq!(requests.len(), 1, "unexpected errors must never be retried");
        }
    }

    #[test]
    fn diagnostic_expired_deadline_sends_no_request() {
        let (succeeded, requests) = exercise(vec![successful()], true);
        assert!(!succeeded, "an expired deadline must fail");
        assert!(
            requests.is_empty(),
            "the deadline must not be reset before sending"
        );
    }
}
