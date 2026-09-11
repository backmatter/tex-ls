# Meaning browser host

This crate exposes Meaning's native language computations in WebAssembly.
The embedding application owns transport, project files, scheduling and storage.
No browser storage or networking is built into the engine.

Build with the compiler pinned in `rust-toolchain.toml` and the wasm32-unknown-unknown target:

```sh
cargo build -p meaning-browser --profile browser --target wasm32-unknown-unknown --locked
wasm-bindgen target/wasm32-unknown-unknown/browser/meaning_browser.wasm --target web --out-dir browser --out-name meaning
cp crates/meaning-browser/js/session.mjs browser/session.mjs
```

The `browser` profile enables whole-program LTO and one code generation unit.
It changes build optimization only; native and browser language features still
share their implementations.

Use wasm-bindgen-cli 0.2.128, matching Cargo.lock. Load the generated module in a
Web Worker and create a session through the JavaScript wrapper:

```js
import init, {LanguageSession} from "./browser/meaning.js";
import {createLanguageSession} from "./browser/session.mjs";

await init();
const session = createLanguageSession(LanguageSession);
const result = session.dispatch("initialize", {capabilities: {}});
```

Call `session.dispatch(method, params)` with ordinary LSP parameter objects. It returns
a JavaScript value directly and throws on invalid requests. Notifications return
`null`; result maps are plain objects. Pass JSON-compatible values.
Call `session.free()` when disposing a session without terminating its worker.
The wrapper handles JSON conversion. The raw wasm-bindgen `LanguageSession`
accepts and returns JSON strings.

`initialize` reports supported requests. `textDocument/didOpen` supplies a file
URI, languageId, version and text. `textDocument/didChange` accepts increasing
versions and sequential UTF-16 contentChanges. `textDocument/didClose` removes
that file from the project, unlike a native editor's close-with-file-on-disk
behavior. Keep project files open while analysis needs them.
`meaning/registerFile` accepts `{ "uri": "file:///project/figure.pdf" }` for
path-only assets. Closing the same URI removes an asset.

Completion, resolve, hover, signature help, definitions, references, highlights,
rename, document and workspace symbols, diagnostics, code actions, folding,
selection ranges, document links and formatting share the native implementations.
Default configuration applies. Native process integrations and installed TEXMF
lookup are disabled. This is an initial port, not a declaration of browser
feature parity or large-project performance.

Label completion includes definitions in connected project files, using the same
include namespace as label navigation. Unrelated documents stay separate. BibTeX
completion reuses the current parsed tree, and only string-macro candidates need
the semantic model. These changes also apply to the native LSP.

Check the exported JavaScript API after building the WASM crate:

```sh
wasm-bindgen target/wasm32-unknown-unknown/browser/meaning_browser.wasm --target nodejs --out-dir target/browser-test
node scripts/test_browser_bridge.mjs target/browser-test/meaning_browser.js
```
