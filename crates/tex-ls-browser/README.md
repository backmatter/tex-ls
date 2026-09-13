# tex-ls browser host

`LanguageSession` exposes tex-ls through wasm-bindgen. The application supplies
sources and owns transport, storage, and workers.
See the [embedding contract](../../docs/development/embedding.md).

Install the wasm-bindgen CLI version matching `Cargo.lock`, then build:

```sh
cargo install --locked wasm-bindgen-cli --version 0.2.128
cargo build -p tex-ls-browser --profile browser --locked --target wasm32-unknown-unknown
wasm-bindgen target/wasm32-unknown-unknown/browser/tex_ls_browser.wasm --target web --out-dir target/browser --out-name tex-ls
cp crates/tex-ls-browser/js/session.mjs target/browser/session.mjs
```

The build writes web bindings and an object-based wrapper to `target/browser/`.

```js
import init, {LanguageSession} from "./tex-ls.js";
import {createLanguageSession} from "./session.mjs";

await init();
const session = createLanguageSession(LanguageSession);
session.dispatch("initialize", {capabilities: {}});
const uri = "file:///project/main.tex";
session.dispatch("textDocument/didOpen", {textDocument: {
  uri, languageId: "latex", version: 1, text: "\\section{Introduction}\n",
}});
const {result, preconditions} = session.dispatch_with_context(
  "textDocument/documentSymbol", {textDocument: {uri}},
);
if (session.dispatch("tex-ls/checkPreconditions", {preconditions}).current) {
  console.log(result);
}
session.free();
```

For delayed results, validate preconditions and apply or publish the result in one
application transaction with no intervening session writes.

Run `bash scripts/check_host_parity.sh` to compare native and WASM results.
The `browser` profile uses link-time optimization and one code-generation unit.
