import assert from 'node:assert/strict';
import {resolve} from 'node:path';
import {createRequire} from 'node:module';
import {createLanguageSession} from '../crates/tex-ls-browser/js/session.mjs';
const require = createRequire(import.meta.url);
if (!process.argv[2]) throw new Error('Pass the generated Node.js bindings path');
const {LanguageSession} = require(resolve(process.argv[2]));
const raw = new LanguageSession();
assert.throws(() => raw.dispatch('initialized', '{}'), /Session is not running/);
raw.dispatch('initialize', '{"capabilities":{}}');
assert.equal(raw.dispatch('initialized', '{}'), 'null');
assert.throws(() => raw.dispatch('initialize', '{'));
raw.free();
const session = createLanguageSession(LanguageSession);
const dispatch = (method, params) => session.dispatch(method, params);
const uri = 'file:///project/main%20file.tex';
const initialized = dispatch('initialize', {capabilities: {}, rootUri: null});
assert.equal(Object.getPrototypeOf(initialized), Object.prototype);
assert.equal(initialized.capabilities.textDocumentSync.change, 2);
assert.equal(dispatch('initialized', {}), null);
assert.throws(() => dispatch('textDocument/didOpen', {}));
dispatch('textDocument/didOpen', {textDocument: {uri, languageId: 'latex', version: 0, text: '\\label{old}\n\\ref{old}\n'}});
const update = {textDocument: {uri, version: 1}, contentChanges: [
 {range: {start: {line:0, character:7}, end: {line:0, character:10}}, text:'new'},
 {range: {start: {line:1, character:5}, end: {line:1, character:8}}, text:'new'}
]};
dispatch('textDocument/didChange', update);
assert.throws(() => dispatch('textDocument/didChange', update));
const query = {textDocument: {uri}, position: {line:1, character:6}};
const definitions = dispatch('textDocument/definition', query);
assert.equal(definitions.length, 1);
assert.equal(definitions[0].uri, uri);
assert.equal(definitions[0].range.start.line, 0);
const rename = dispatch('textDocument/rename', {...query, newName:'final'});
assert.equal(Object.getPrototypeOf(rename.changes), Object.prototype);
assert.equal(rename.changes[uri].length, 2);
const captured = session.dispatch_with_context('textDocument/rename', {...query, newName:'final'});
assert.deepEqual(captured.result, rename);
assert.deepEqual(dispatch('tex-ls/checkPreconditions', {preconditions:captured.preconditions}), {current:true});
dispatch('textDocument/didChange', {textDocument: {uri, version: 2}, contentChanges: [{text:'\\section{Ångström 😀}\n'}]});
assert.deepEqual(dispatch('tex-ls/checkPreconditions', {preconditions:captured.preconditions}), {current:false});
assert.equal(dispatch('textDocument/documentSymbol', {textDocument:{uri}})[0].name, 'Ångström 😀');
const beforeClose = session.dispatch_with_context('textDocument/documentSymbol', {textDocument:{uri}});
dispatch('textDocument/didClose', {textDocument:{uri}});
dispatch('textDocument/didOpen', {textDocument: {uri, languageId:'latex', version:2, text:'\\section{Ångström 😀}\n'}});
assert.deepEqual(dispatch('tex-ls/checkPreconditions', {preconditions:beforeClose.preconditions}), {current:false});
dispatch('textDocument/didClose', {textDocument:{uri}});
for (const text of [
  '{'.repeat(10000) + 'x',
  '{'.repeat(10000) + 'x' + '}'.repeat(10000),
  '$' + '{'.repeat(10000) + 'x' + '}'.repeat(10000) + '$',
  '\\ExplSyntaxOn ' + '\\use:n {'.repeat(1000) + 'x' + '}'.repeat(1000),
]) {
  dispatch('textDocument/didOpen', {textDocument: {uri, languageId:'latex', version:1,
    text}});
  assert.deepEqual(dispatch('textDocument/documentSymbol', {textDocument:{uri}}), []);
  dispatch('textDocument/didChange', {textDocument:{uri,version:2},contentChanges:[{text:'\\section{Recovered}'}]});
  assert.equal(dispatch('textDocument/documentSymbol', {textDocument:{uri}})[0].name, 'Recovered');
  dispatch('textDocument/didClose', {textDocument:{uri}});
}
assert.equal(dispatch('shutdown', null), null);
session.free();
console.log('Browser bridge passed: objects, nulls, invalid requests, edits, preconditions, URI keys, rename, Unicode, close, and nesting recovery');
