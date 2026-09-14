const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { serverCommand, serverSettings, withCurrentDocument } = require('../dist/settings');
const { Registry, parseRawGrammar } = require('vscode-textmate');
const oniguruma = require('vscode-oniguruma');

test('bundled server wins over PATH unless explicitly overridden', () => {
  assert.equal(serverCommand('/extension', '', 'linux'), path.join('/extension', 'server', 'tex-ls'));
  assert.equal(serverCommand('/extension', '', 'win32'), path.join('/extension', 'server', 'tex-ls.exe'));
  assert.equal(serverCommand('/extension', '/some path/tex-ls'), '/some path/tex-ls');
  assert.equal(serverCommand('/extension', 'tex-ls'), 'tex-ls');
});

test('configuration preserves nested server settings and excludes client settings', () => {
  const values = {
    'lineWidth': 72, 'texmf.enabled': false, 'texmf.roots': ['/packages'],
    'forwardSearch.args': ['%p', '%l', '%f'], 'outline': { labels: false },
    'server.path': '/unrelated', 'trace.server': 'verbose',
  };
  const settings = JSON.parse(JSON.stringify(serverSettings({ get: (key) => values[key] })));
  assert.deepEqual(settings, {
    lineWidth: 72, texmf: { enabled: false, roots: ['/packages'] },
    diagnostics: {}, outline: { labels: false },
  });
});

test('formatting rejects edits after document changes, close, or cancellation', async () => {
  for (const change of [
    (document) => { document.version++; },
    (document) => { document.isClosed = true; },
    (_, token) => { token.isCancellationRequested = true; },
  ]) {
    const document = { version: 1, isClosed: false };
    const token = { isCancellationRequested: false };
    let respond;
    const pending = withCurrentDocument(document, token, () => new Promise((resolve) => { respond = resolve; }));
    change(document, token);
    respond(['obsolete edits']);
    assert.equal(await pending, null);
  }
  assert.deepEqual(await withCurrentDocument({ version: 1, isClosed: false },
    { isCancellationRequested: false }, () => ['current edits']), ['current edits']);
  assert.equal(await withCurrentDocument({ version: 1, isClosed: true },
    { isCancellationRequested: false }, () => assert.fail('closed document was requested')), null);
});

const wasm = fs.readFileSync(require.resolve('vscode-oniguruma/release/onig.wasm'));
const registry = new Registry({
  onigLib: oniguruma.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength)).then(() => ({
    createOnigScanner: (patterns) => new oniguruma.OnigScanner(patterns),
    createOnigString: (value) => new oniguruma.OnigString(value),
  })),
  loadGrammar: async (scope) => {
    const file = path.join(__dirname, '..', 'syntaxes', scope === 'text.bibtex' ? 'bibtex.tmLanguage.json' : 'latex.tmLanguage.json');
    return parseRawGrammar(fs.readFileSync(file, 'utf8'), file);
  },
});

test('LaTeX grammar keeps escaped comments and protected content intact', async () => {
  const grammar = await registry.loadGrammar('text.tex.latex');
  const line = String.raw`Text \% escaped \verb|% $| % comment`;
  const tokens = grammar.tokenizeLine(line).tokens;
  const scopesAt = (offset) => tokens.find((token) => token.startIndex <= offset && token.endIndex > offset).scopes;
  assert.ok(!scopesAt(line.indexOf('%')).some((scope) => scope.startsWith('comment')));
  assert.ok(scopesAt(line.indexOf('% $')).includes('string.quoted.other.verbatim.latex'));
  assert.ok(scopesAt(line.lastIndexOf('%')).includes('comment.line.percentage.latex'));
  const begin = grammar.tokenizeLine(String.raw`\begin{verbatim}`);
  const body = grammar.tokenizeLine('% $ { arbitrary', begin.ruleStack);
  assert.ok(body.tokens.every((token) => token.scopes.includes('string.other.verbatim.latex')));
  const end = grammar.tokenizeLine(String.raw`\end{verbatim}`, body.ruleStack);
  assert.ok(!grammar.tokenizeLine('Text', end.ruleStack).tokens[0].scopes.includes('string.other.verbatim.latex'));
});

test('BibTeX grammar handles nested values and resumes after comments', async () => {
  const grammar = await registry.loadGrammar('text.bibtex');
  let state;
  for (const line of ['@comment{ignored {nested} text}', '@article{key,', 'title = {A {Nested} Title},', 'year = 2026', '}']) {
    const result = grammar.tokenizeLine(line, state);
    state = result.ruleStack;
    if (line.startsWith('title')) assert.ok(result.tokens[0].scopes.includes('entity.other.attribute-name.bibtex'));
    if (line.startsWith('year')) assert.ok(result.tokens.some((token) => token.scopes.includes('constant.numeric.bibtex')));
  }
  assert.ok(grammar.tokenizeLine('outside entry', state).tokens[0].scopes.includes('comment.line.bibtex'));
});

test('project toolchain isolation and diagnostic policy reach the server', () => {
  const values = { 'texmf.roots': ['.local/texmf'], 'texmf.explicitOnly': true,
    'diagnostics.compiler': false };
  const settings = serverSettings({ get: (key) => values[key] });
  assert.deepEqual(settings.texmf.roots, ['.local/texmf']);
  assert.equal(settings.texmf.explicitOnly, true);
  assert.equal(settings.diagnostics.compiler, false);
  const manifest = require('../package.json');
  for (const key of ['enabled', 'roots', 'useKpsewhich', 'explicitOnly']) {
    assert.equal(manifest.contributes.configuration.properties[`tex-ls.texmf.${key}`].scope, 'machine-overridable');
  }
  assert.equal(manifest.contributes.configuration.properties['tex-ls.server.path'].scope, 'machine');
});
