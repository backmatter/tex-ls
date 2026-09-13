//! Profile browser session allocation lifetime on the native allocator.
use serde_json::json;
use tex_ls_browser::session::Session;

#[path = "../crates/tex-ls-browser/src/metrics.rs"]
mod metrics;

fn main() {
    let mut rows = Vec::new();
    for rounds in [0, 1, 8, 32] {
        let mut session = Session::default();
        session.dispatch("initialize", json!({})).unwrap();
        for n in 0..rounds {
            let uri = format!("file:///profile/source{n}.tex");
            session.dispatch("textDocument/didOpen", json!({"textDocument": {
                "uri": uri, "languageId": "latex", "version": 0,
                "text": format!("\\section{{Title}}\n{}", "Unicode 𝕏 prose.\r\n".repeat(4096))
            }})).unwrap();
            session
                .dispatch(
                    "textDocument/documentSymbol",
                    json!({"textDocument": {"uri": uri}}),
                )
                .unwrap();
            session
                .dispatch(
                    "textDocument/didClose",
                    json!({"textDocument": {"uri": uri}}),
                )
                .unwrap();
        }
        let live_session = metrics::sample();
        drop(session);
        let disposed_session = metrics::sample();
        rows.push(json!({"rounds": rounds, "liveSession": live_session, "disposedSession": disposed_session}));
    }
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
}
