// Compare the native LSP process and a real WASM instance using identical events.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve, dirname, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';

const [binary, bindings, transcript, output] = process.argv.slice(2);
if (!output) throw new Error('Usage: node scripts/replay_language_hosts.mjs BINARY BINDINGS TRANSCRIPT OUTPUT');
const { LanguageSession } = createRequire(import.meta.url)(resolve(bindings));
let fixture = JSON.parse(readFileSync(transcript, 'utf8'));
let project;
if (Object.hasOwn(fixture, 'nativeConfig')) {
  project = mkdtempSync(join(dirname(resolve(output)), 'settings-project-'));
  writeFileSync(join(project, 'tex-ls.toml'), fixture.nativeConfig);
  fixture = JSON.parse(JSON.stringify(fixture).replaceAll('file:///architecture-settings', pathToFileURL(project).href).replaceAll('/architecture-settings', project));
}
const events = Array.isArray(fixture) ? fixture : fixture.events;
const child = spawn(resolve(binary), ['lsp'], { stdio: ['pipe', 'pipe', 'inherit'] });
const pending = new Map();
let nextId = 1;
let bytes = Buffer.alloc(0);
function send(message) {
  const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', ...message }));
  child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
  child.stdin.write(body);
}
async function request(method, params) {
  for (let attempt = 0; attempt < 8; attempt++) {
    try { return await requestOnce(method, params); }
    catch (error) {
      const diagnosticRetry = ["textDocument/diagnostic", "workspace/diagnostic"].includes(method)
        && error.code === -32802 && error.data?.retriggerRequest !== false;
      if (error.code !== -32801 && !diagnosticRetry) throw error;
    }
  }
  throw new Error(`Repeatedly invalidated: ${method}`);
}
function requestOnce(method, params) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`Timed out: ${method}`)), 30000);
    pending.set(id, { resolve, reject, timer });
    send({ id, method, params });
  });
}
child.stdout.on('data', chunk => {
  bytes = Buffer.concat([bytes, chunk]);
  for (;;) {
    const end = bytes.indexOf('\r\n\r\n');
    if (end < 0) return;
    const size = Number(/Content-Length: (\d+)/i.exec(bytes.subarray(0, end).toString())[1]);
    if (bytes.length < end + 4 + size) return;
    const message = JSON.parse(bytes.subarray(end + 4, end + 4 + size));
    bytes = bytes.subarray(end + 4 + size);
    if (message.method && message.id !== undefined) {
      send({ id: message.id, result: null });
    } else if (pending.has(message.id)) {
      const p = pending.get(message.id);
      pending.delete(message.id);
      clearTimeout(p.timer);
      if (message.error) p.reject(Object.assign(new Error(JSON.stringify(message.error)), { code: message.error.code, data: message.error.data }));
      else p.resolve(message.result);
    }
  }
});
// Completion applicability tokens identify host-local source/storage lifetimes.
// Their behavior is tested separately; shared language results exclude token values.
function languageResult(value) {
  if (Array.isArray(value)) return value.map(languageResult);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value)
    .filter(([key]) => key !== 'sourceRevision' && key !== 'storageEpoch')
    .map(([key, value]) => [key, languageResult(value)]));
  return value;
}
const session = new LanguageSession();
const samples = [];
try {
  // Tests supply installation observations explicitly; do not inherit a local
  // machine's TeX tree unless the fixture deliberately configures discovery.
  const initialize = { capabilities: {}, ...fixture.initialize };
  initialize.initializationOptions = { texmf: { enabled: false }, ...initialize.initializationOptions };
  await request('initialize', initialize);
  send({ method: 'initialized', params: {} });
  session.dispatch('initialize', JSON.stringify(initialize));
  if (fixture.settings) {
    assert.deepEqual(JSON.parse(session.dispatch('tex-ls/updateSettings', JSON.stringify({ settings: fixture.settings }))), { applied: true });
  }
  for (const event of events) {
    const changes = [];
    for (const [name, contents] of Object.entries(event.files ?? {})) {
      if (!project) throw new Error('Disk fixture requires nativeConfig');
      const path = join(project, name);
      const existed = existsSync(path);
      if (contents === null) rmSync(path, { force: true });
      else writeFileSync(path, contents);
      changes.push({ uri: pathToFileURL(path).href, type: contents === null ? 3 : existed ? 2 : 1 });
    }
    // Match explicit browser input publication with an ordered native watch event.
    if (changes.length) send({ method: 'workspace/didChangeWatchedFiles', params: { changes } });
    const start = performance.now();
    const wasm = JSON.parse(session.dispatch(event.method, JSON.stringify(event.params)));
    const wasmMs = performance.now() - start;
    if (Object.hasOwn(event, "expected")) assert.deepEqual(wasm, event.expected, event.method);
    if (event.expectedCompletionLabels) assert.deepEqual(wasm.items.map(item => item.label), event.expectedCompletionLabels, event.method);
    if (Object.hasOwn(event, "expectedActiveParameter")) {
      assert.ok(wasm && Array.isArray(wasm.signatures), event.method);
      assert.deepEqual(wasm.activeParameter, event.expectedActiveParameter, event.method);
    }
    if (event.expectedNonEmpty) {
      assert.ok(Array.isArray(wasm) && wasm.length > 0, `${event.method}: expected nonempty results`);
    }
    const nativeStart = performance.now();
    if (event.embeddedOnly) {
      // Explicit browser observations correspond to disk acquisition in native.
    } else if (event.request) {
      const native = await request(event.method, event.params);
      assert.deepEqual(languageResult(wasm), languageResult(native), event.method);
    } else send({ method: event.method, params: event.params });
    samples.push({ method: event.method, request: !!event.request, wasmMs, nativeMs: performance.now() - nativeStart });
  }
  assert.equal(JSON.parse(session.dispatch('shutdown', 'null')), null);
  assert.throws(() => session.dispatch('workspace/symbol', '{"query":""}'));
  session.dispatch('exit', 'null');
  await request('shutdown', null);
  await assert.rejects(request('workspace/symbol', { query: '' }));
  send({ method: 'exit', params: null });
  writeFileSync(output, JSON.stringify({ node: process.version, samples }, null, 2) + '\n');
  console.log(`Native/WASM parity passed for ${events.length} events.`);
} finally {
  session.free();
  for (const p of pending.values()) clearTimeout(p.timer);
  child.kill();
  if (project) rmSync(project, { recursive: true });
}
