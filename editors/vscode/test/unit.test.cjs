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
const blockComment = { begin: '/\\*', end: '\\*/', name: 'comment.block.foreign' };
const foreignPatterns = {
  'source.c': [blockComment],
  'source.cpp': [blockComment],
  'source.js': [blockComment, { match: '\\breturn\\b', name: 'keyword.control.js' }],
  'source.ts': [blockComment],
  'source.json': [{ begin: '"', end: '"', name: 'string.quoted.double.json' }],
  'source.css': [blockComment],
  'text.html.basic': [
    { begin: '<!--', end: '-->', name: 'comment.block.html' },
    { match: '<[a-z]+>', name: 'entity.name.tag.html' },
  ],
};
const registry = new Registry({
  onigLib: oniguruma.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength)).then(() => ({
    createOnigScanner: (patterns) => new oniguruma.OnigScanner(patterns),
    createOnigString: (value) => new oniguruma.OnigString(value),
  })),
  loadGrammar: async (scope) => {
    if (foreignPatterns[scope]) {
      return parseRawGrammar(JSON.stringify({ scopeName: scope, patterns: foreignPatterns[scope] }), `${scope}.tmLanguage.json`);
    }
    if (!['text.tex.latex', 'text.bibtex'].includes(scope)) return null;
    const file = path.join(__dirname, '..', 'syntaxes', scope === 'text.bibtex' ? 'bibtex.tmLanguage.json' : 'latex.tmLanguage.json');
    return parseRawGrammar(fs.readFileSync(file, 'utf8'), file);
  },
});

test('embedded grammars select a literal language and end at the LaTeX closer', async () => {
  const grammar = await registry.loadGrammar('text.tex.latex');
  for (const [language, source, scope] of [
    ['javascript', 'return 42;', 'keyword.control.js'],
    ['html', '<div>text</div>', 'entity.name.tag.html'],
  ]) {
    for (const environment of ['minted', 'lstlisting']) {
      const header = environment === 'minted'
        ? `\\begin{minted}[linenos]{${language}}`
        : `\\begin{lstlisting}[language=${language}]`;
      const start = grammar.tokenizeLine(header);
      const code = grammar.tokenizeLine(source, start.ruleStack);
      assert.ok(code.tokens.some((token) => token.scopes.includes(scope)));
      assert.ok(code.tokens.every((token) => token.scopes.includes(`meta.embedded.block.${language}.latex`)));
      assert.ok(grammar.tokenizeLine(`${header} ${source}`).tokens.some((token) => token.scopes.includes(scope)));
      const end = grammar.tokenizeLine(`\\end{${environment}}`, code.ruleStack);
      assert.ok(!grammar.tokenizeLine('Plain text', end.ruleStack).tokens[0].scopes.some((scope) => scope.startsWith('meta.embedded')));
    }
  }
});

test('LaTeX closers unwind unfinished foreign strings and comments', async () => {
  const grammar = await registry.loadGrammar('text.tex.latex');
  for (const language of ['c', 'cpp', 'javascript', 'typescript', 'json', 'css', 'html']) {
    for (const environment of ['minted', 'lstlisting']) {
      for (const prefix of ['', String.raw`literal \ref{fake} `]) {
        const header = environment === 'minted'
          ? `\\begin{minted}{${language}}`
          : `\\begin{lstlisting}[language=${language}]`;
        let state = grammar.tokenizeLine(header).ruleStack;
        const opening = language === 'json' ? '"unterminated' : language === 'html' ? '<!-- unfinished' : '/* unfinished';
        state = grammar.tokenizeLine(opening, state).ruleStack;
        const continued = grammar.tokenizeLine('still inside', state);
        assert.ok(continued.tokens.every((token) => token.scopes.some((scope) => scope.startsWith('comment') || scope.startsWith('string'))), language);
        const closing = `${prefix}\\end{${environment}} after`;
        const end = grammar.tokenizeLine(closing, continued.ruleStack);
        const command = end.tokens.find((token) => token.startIndex === prefix.length);
        assert.ok(command.scopes.includes('entity.name.function.preprocessor.latex'), `${language}: ${closing}`);
        if (prefix) {
          assert.ok(!end.tokens[0].scopes.includes('entity.name.function.preprocessor.latex'));
          assert.ok(end.tokens[0].scopes.includes(`meta.embedded.block.${language}.latex`));
        }
        const after = grammar.tokenizeLine(String.raw`plain \ref{real}`, end.ruleStack);
        assert.ok(!after.tokens.some((token) => token.scopes.some((scope) => scope.startsWith('meta.embedded') || scope.startsWith('comment') || scope.startsWith('string'))));
        assert.ok(after.tokens.some((token) => token.scopes.includes('entity.name.function.preprocessor.latex')));
      }
    }
  }
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
    if (line.startsWith('year')) assert.ok(!result.tokens.some((token) => token.scopes.includes('constant.numeric.bibtex')));
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


test('LaTeX command and environment scopes are stable across syntax contexts', async () => {
  const grammar = await registry.loadGrammar('text.tex.latex');
  const macro = 'entity.name.function.preprocessor.latex';
  const environment = 'entity.name.type.environment.latex';
  const cases = [
    [String.raw`\begin{frame} \end{frame} \begin{customenv} \end{customenv}`, ['\\begin', '\\end', '\\begin', '\\end']],
    [String.raw`\newcommand{\custom}[1]{\textbf{#1}} \custom{x} \tem`, ['\\newcommand', '\\custom', '\\textbf', '\\custom', '\\tem']],
    [String.raw`$\alpha + \mystery$ \(\beta\) \[\gamma\]`, ['\\alpha', '\\mystery', '\\(', '\\beta', '\\)', '\\[', '\\gamma', '\\]']],
    [String.raw`\ExplSyntaxOn \foo_bar:n {\l_tmpa_tl} \ExplSyntaxOff`, ['\\ExplSyntaxOn', '\\foo_bar:n', '\\l_tmpa_tl', '\\ExplSyntaxOff']],
    [String.raw`\% \{ \} \\ % \hidden`, ['\\%', '\\{', '\\}', '\\\\']],
  ];
  for (const [line, expected] of cases) {
    const tokens = grammar.tokenizeLine(line).tokens;
    const actual = tokens.filter(t => t.scopes.includes(macro)).map(t => line.slice(t.startIndex, t.endIndex));
    assert.deepEqual(actual, expected, line);
  }
  const line = String.raw`\begin{customenv} x \end{customenv}`;
  assert.deepEqual(grammar.tokenizeLine(line).tokens.filter(t => t.scopes.includes(environment))
    .map(t => line.slice(t.startIndex, t.endIndex)), ['customenv', 'customenv']);
  for (const source of [String.raw`\verb|\fake % $| \real`, String.raw`$\verb|\fake| \real$`]) {
    const macros = grammar.tokenizeLine(source).tokens.filter(t => t.scopes.includes(macro))
      .map(t => source.slice(t.startIndex, t.endIndex));
    assert.deepEqual(macros, ['\\verb', '\\real']);
  }
  for (const name of ['verbatim', 'Verbatim', 'lstlisting', 'minted']) {
    const begin = grammar.tokenizeLine(`\\begin{${name}}`);
    const body = grammar.tokenizeLine(String.raw`\fake % $`, begin.ruleStack);
    assert.ok(body.tokens.every(t => !t.scopes.includes(macro)), name);
    const end = grammar.tokenizeLine(`\\end{${name}}`, body.ruleStack);
    assert.ok(end.tokens.some(t => t.scopes.includes(macro)), name);
    assert.ok(grammar.tokenizeLine('\\real', end.ruleStack).tokens.some(t => t.scopes.includes(macro)), name);
  }
});


test('module and path arguments have roles without coloring prose arguments', async () => {
  const grammar = await registry.loadGrammar('text.tex.latex');
  for (const command of ['usepackage', 'documentclass', 'usetheme', 'usecolortheme', 'usefonttheme',
    'useinnertheme', 'useoutertheme', 'usetikzlibrary', 'usepgflibrary', 'bibliographystyle',
    'RequireBibliographyStyle', 'RequireCitationStyle']) {
    const source = `\\${command}[option]{first, second}`;
    const names = grammar.tokenizeLine(source).tokens.filter(t => t.scopes.includes('entity.name.namespace.latex'))
      .map(t => source.slice(t.startIndex, t.endIndex));
    assert.deepEqual(names, ['first', 'second'], command);
  }
  for (const command of ['input', 'includegraphics', 'bibliography', 'addbibresource']) {
    const source = `\\${command}{folder/file}`;
    assert.ok(grammar.tokenizeLine(source).tokens.some(t => t.scopes.includes('string.other.path.latex')), command);
  }
  for (const source of [String.raw`\section{metropolis}`, String.raw`\textbf{metropolis}`,
    String.raw`\usetheme{\dynamic}`, String.raw`\usetheme{prefix\dynamic}`,
    String.raw`\usetheme{\dynamic suffix}`, String.raw`\usetheme{#1}`,
    String.raw`\usetheme{\dynamic{suffix}}`, String.raw`\usetheme{{literal}}`]) {
    assert.ok(grammar.tokenizeLine(source).tokens.every(t => !t.scopes.includes('entity.name.namespace.latex')), source);
  }
  for (const source of [String.raw`\input{prefix\dynamic}`, String.raw`\includegraphics{\dynamic suffix}`,
    String.raw`\includegraphics{\dynamic{suffix}}`]) {
    assert.ok(grammar.tokenizeLine(source).tokens.every(t => !t.scopes.includes('string.other.path.latex')), source);
  }
  const mixed = String.raw`\usetheme{first, prefix\dynamic, \dynamic suffix, {nested}, last}`;
  assert.deepEqual(grammar.tokenizeLine(mixed).tokens.filter(t => t.scopes.includes('entity.name.namespace.latex'))
    .map(t => mixed.slice(t.startIndex, t.endIndex)), ['first', 'last']);
});

test('fallback grammar protects URLs and inline code without coloring prose parameters', async () => {
  const grammar = await registry.loadGrammar('text.tex.latex');
  for (const [line, body, scope] of [
    [String.raw`\url{https://host/a%20b?x=1_2&y=$z#part} after`, 'a%20b', 'string.other.path.latex'],
    [String.raw`\path{a_b%file} after`, 'a_b', 'markup.raw.verbatim.latex'],
    [String.raw`\lstinline|a_b%$| after`, 'a_b', 'markup.raw.verbatim.latex'],
    [String.raw`\mintinline{python}|a_b%$| after`, 'a_b', 'markup.raw.verbatim.latex'],
    [String.raw`\detokenize{\ref{fake} $x_1$} after`, '\\ref', 'markup.raw.token-list.latex'],
  ]) {
    const result = grammar.tokenizeLine(line);
    const token = result.tokens.find((token) => token.startIndex <= line.indexOf(body) && token.endIndex > line.indexOf(body));
    assert.ok(token.scopes.includes(scope), `${line}: ${token.scopes}`);
    assert.ok(!token.scopes.some((scope) => scope.startsWith('comment') || scope.includes('meta.math')));
    assert.ok(!grammar.tokenizeLine('next line', result.ruleStack).tokens[0].scopes.includes(scope));
  }
  const prose = grammar.tokenizeLine('prose #1 x_y & ~').tokens;
  assert.ok(!prose.some((token) => token.scopes.some((scope) => scope.includes('variable.parameter') || scope.includes('keyword.operator'))));
  const math = grammar.tokenizeLine('$x_1$').tokens;
  assert.ok(math.some((token) => token.scopes.includes('keyword.operator.math.latex')));
  const open = grammar.tokenizeLine(String.raw`\begin{alltt}`);
  const body = grammar.tokenizeLine(String.raw`% $ x_1 \ref{real}`, open.ruleStack).tokens;
  assert.ok(!body.some((token) => token.scopes.some((scope) => scope.startsWith('comment') || scope.includes('meta.math'))));
  assert.ok(body.some((token) => token.scopes.includes('entity.name.function.preprocessor.latex')));
});

test('semantic purposes have neutral theme fallbacks and font-only defaults', () => {
  const manifest = require('../package.json').contributes;
  assert.deepEqual(manifest.semanticTokenTypes.map((type) => type.id), ['texText', 'texLiteral', 'texPath', 'texMath']);
  const rules = manifest.configurationDefaults['editor.semanticTokenColorCustomizations'].rules;
  for (const entry of manifest.semanticTokenScopes) {
    assert.ok(entry.scopes.texText.includes('meta.embedded'));
    assert.ok(entry.scopes.texLiteral.includes('meta.embedded'));
    assert.notDeepEqual(entry.scopes.texLiteral, entry.scopes.texPath);
    assert.ok(!Object.values(entry.scopes).flat().includes('markup.bold'));
    assert.deepEqual(rules[`texText:${entry.language}`], { bold: false, italic: false });
    assert.ok(Object.keys(rules).indexOf(`texText:${entry.language}`)
      < Object.keys(rules).indexOf(`*.texBold:${entry.language}`));
    assert.ok(Object.keys(rules).indexOf(`texText:${entry.language}`)
      < Object.keys(rules).indexOf(`*.texItalic:${entry.language}`));
    assert.deepEqual(rules[`*.texBold:${entry.language}`], { bold: true });
    assert.deepEqual(rules[`*.texItalic:${entry.language}`], { italic: true });
    assert.deepEqual(rules[`*.declaration:${entry.language}`], { bold: true });
  }
});


test('client advertises custom semantic purposes after the standard feature', () => {
  const { semanticHighlighting } = require('../dist/semanticHighlighting');
  const capabilities = {
    experimental: { existingCapability: true },
    textDocument: { semanticTokens: { tokenTypes: ['macro', 'variable'], tokenModifiers: ['declaration'] } },
  };
  semanticHighlighting.fillClientCapabilities(capabilities);
  semanticHighlighting.fillClientCapabilities(capabilities);
  const manifest = require('../package.json').contributes;
  for (const type of manifest.semanticTokenTypes) assert.ok(capabilities.textDocument.semanticTokens.tokenTypes.includes(type.id));
  for (const modifier of manifest.semanticTokenModifiers) assert.ok(capabilities.textDocument.semanticTokens.tokenModifiers.includes(modifier.id));
  assert.equal(new Set(capabilities.textDocument.semanticTokens.tokenTypes).size, capabilities.textDocument.semanticTokens.tokenTypes.length);
  assert.equal(new Set(capabilities.textDocument.semanticTokens.tokenModifiers).size, capabilities.textDocument.semanticTokens.tokenModifiers.length);
  assert.deepEqual(capabilities.experimental, { existingCapability: true, texEmbeddedHighlighting: true });
  const unsupported = {};
  semanticHighlighting.fillClientCapabilities(unsupported);
  assert.deepEqual(unsupported, {});
});
