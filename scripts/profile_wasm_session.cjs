const { createRequire } = require('node:module');
const { resolve } = require('node:path');
const { readFileSync } = require('node:fs');
const { performance } = require('node:perf_hooks');
const bindings = resolve(process.argv[2]);
const api = require(bindings);
const rows = [];
for (const rounds of [0, 1, 8, 32]) {
  const session = new api.LanguageSession();
  const dispatch = (method, params) => session.dispatch(method, JSON.stringify(params));
  dispatch('initialize', {});
  const times = [];
  for (let n = 0; n < rounds; n++) {
    const uri = `file:///profile/source${n}.tex`;
    const start = performance.now();
    dispatch('textDocument/didOpen', {textDocument: {
      uri, languageId: 'latex', version: 0,
      text: '\\section{Title}\n' + 'Unicode 𝕏 prose.\r\n'.repeat(4096),
    }});
    const opened = performance.now();
    dispatch('textDocument/documentSymbol', {textDocument: {uri}});
    const cold = performance.now();
    dispatch('textDocument/documentSymbol', {textDocument: {uri}});
    const warm = performance.now();
    dispatch('textDocument/didClose', {textDocument: {uri}});
    times.push({openMs: opened-start, coldMs: cold-opened, warmMs: warm-cold, closeMs: performance.now()-warm});
  }
  const liveSession = JSON.parse(api.allocation_metrics());
  session.free();
  const disposedSession = JSON.parse(api.allocation_metrics());
  rows.push({rounds, liveSession, disposedSession, linearMemoryBytes: api.linear_memory_bytes(), times});
}
// A fresh module instance gives a separate allocator and linear memory.
delete require.cache[require.resolve(bindings)];
const fresh = createRequire(bindings)(bindings);
const restarted = new fresh.LanguageSession();
restarted.free();
console.log(JSON.stringify({node: process.version, artifactBytes: readFileSync(bindings.replace(/\.js$/, '_bg.wasm')).length, rows, restart: JSON.parse(fresh.allocation_metrics())}, null, 2));
