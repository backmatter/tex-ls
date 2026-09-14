const assert = require('node:assert/strict');
const path = require('node:path');
const vscode = require('vscode');

async function eventually(description, check) {
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) {
    const result = await check();
    if (result) return result;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  assert.fail(`Timed out: ${description}`);
}

async function format(document) {
  return vscode.commands.executeCommand('vscode.executeFormatDocumentProvider', document.uri, { tabSize: 2, insertSpaces: true });
}

async function showSource(document, column) {
  // Close the report before switching groups on unattended CI desktops.
  const reports = vscode.window.tabGroups.all.flatMap((group) => group.tabs)
    .filter((tab) => tab.input instanceof vscode.TabInputText && tab.input.uri.scheme === 'tex-ls-project');
  if (reports.length) await vscode.window.tabGroups.close(reports);
  await eventually('source editor becomes active', async () => {
    await vscode.window.showTextDocument(document, { viewColumn: column, preview: false, preserveFocus: false });
    return vscode.window.activeTextEditor?.document.uri.toString() === document.uri.toString();
  });
}

async function apply(document, edits) {
  if (!edits.length) return;
  const edit = new vscode.WorkspaceEdit();
  edit.set(document.uri, edits);
  assert.ok(await vscode.workspace.applyEdit(edit));
}

exports.run = async function run() {
  const root = process.env.TEX_LS_TEST_WORKSPACE;
  const uri = (folder, name = 'main.tex') => vscode.Uri.file(path.join(root, folder, name));
  const document = await vscode.workspace.openTextDocument(uri('first'));
  await vscode.window.showTextDocument(document);
  assert.equal(document.languageId, 'latex');
  assert.ok(document.getText().includes('Introduction'), 'fixture source is loaded');
  const extension = vscode.extensions.getExtension('backmatter.tex-ls');
  assert.ok(extension, 'extension is installed');
  await extension.activate();

  const symbols = await eventually('document symbols', async () => {
    const result = await vscode.commands.executeCommand('vscode.executeDocumentSymbolProvider', document.uri);
    if (result?.length) return result;
    // VS Code caches an empty outline if a startup request is cancelled.
    // A real edit invalidates it and exercises document synchronization.
    await apply(document, [vscode.TextEdit.insert(document.positionAt(document.getText().length), '\n')]);
    return undefined;
  });
  assert.ok(JSON.stringify(symbols).includes('Introduction'));
  const position = new vscode.Position(3, document.lineAt(3).text.indexOf('sec:intro') + 3);
  const definitions = await eventually('label definition', async () => {
    const result = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', document.uri, position);
    return result?.length && result;
  });
  assert.equal((definitions[0].targetUri || definitions[0].uri).toString(), document.uri.toString());
  const rename = await vscode.commands.executeCommand('vscode.executeDocumentRenameProvider', document.uri, position, 'sec:renamed');
  assert.ok(rename instanceof vscode.WorkspaceEdit);
  assert.equal(rename.get(document.uri).length, 2);
  await vscode.workspace.applyEdit(rename);
  assert.equal((document.getText().match(/sec:renamed/g) || []).length, 2);

  const beforeInspection = document.getText();
  await vscode.commands.executeCommand('tex-ls.showProject');
  const reportEditor = vscode.window.activeTextEditor;
  const reportDocument = reportEditor.document;
  assert.equal(reportDocument.uri.scheme, 'tex-ls-project');
  assert.equal(reportDocument.languageId, 'json');
  const project = JSON.parse(reportDocument.getText());
  assert.ok(project.path.replaceAll('\\', '/').endsWith('/first/main.tex'));
  assert.ok(project.candidateRoots.some((candidate) => candidate.replaceAll('\\', '/').endsWith('/first/main.tex')));
  assert.ok(Array.isArray(project.edges));
  assert.equal(vscode.workspace.fs.isWritableFileSystem(reportDocument.uri.scheme), undefined,
    'project report uses a text content provider, not a writable filesystem');
  assert.equal(document.getText(), beforeInspection, 'inspection leaves sources unchanged');

  const otherRoot = await vscode.workspace.openTextDocument(uri('second'));
  await showSource(otherRoot, vscode.ViewColumn.One);
  await vscode.commands.executeCommand('tex-ls.showProject');
  assert.equal(vscode.window.activeTextEditor.document.uri.toString(), reportDocument.uri.toString());
  await eventually('inspection refreshes for the selected root', () =>
    JSON.parse(vscode.window.activeTextEditor.document.getText()).path.replaceAll('\\', '/').endsWith('/second/main.tex'));

  const fixSource = '😀  $x^{2}$  and $y_{3}$. {\\bf bold}\n';
  const fixDocument = await vscode.workspace.openTextDocument({ language: 'latex', content: fixSource });
  await showSource(fixDocument, vscode.ViewColumn.One);
  await eventually('safe fixes are available', async () => {
    const actions = await vscode.commands.executeCommand('vscode.executeCodeActionProvider',
      fixDocument.uri, new vscode.Range(0, 0, 0, 0), 'source.fixAll.tex-ls');
    return actions?.length > 0;
  });
  await vscode.commands.executeCommand('tex-ls.fixAll');
  await eventually('fix-all changes the document', () => fixDocument.getText() !== fixSource);
  assert.equal(fixDocument.getText(), '😀  $x^2$  and $y_3$. {\\bf bold}\n',
    'fix-all preserves formatting and unsafe changes');
  await vscode.commands.executeCommand('undo');
  assert.equal(fixDocument.getText(), fixSource, 'one undo restores all fixes');

  const completionDocument = await vscode.workspace.openTextDocument({ language: 'latex', content: '\\sec' });
  await vscode.window.showTextDocument(completionDocument);
  await eventually('command completion', async () => {
    const result = await vscode.commands.executeCommand('vscode.executeCompletionItemProvider',
      completionDocument.uri, new vscode.Position(0, 4));
    return result?.items.some((item) => (typeof item.label === 'string' ? item.label : item.label.label).includes('section'));
  });

  // Different workspace folders must receive different formatter fallback settings.
  const prose = 'These words form a paragraph with enough plain text to wrap at forty columns but not at one hundred.\n';
  const documents = [];
  for (const folder of ['first', 'second']) {
    const file = uri(folder, 'prose.tex');
    await vscode.workspace.fs.writeFile(file, Buffer.from(prose));
    const doc = await vscode.workspace.openTextDocument(file);
    await vscode.window.showTextDocument(doc, { preview: false });
    documents.push(doc);
  }
  await eventually('folder-scoped formatting', async () => {
    const edits = await format(documents[0]);
    return edits?.length && edits.some((edit) => edit.newText.includes('\n'));
  });
  await apply(documents[0], await format(documents[0]));
  await apply(documents[1], await format(documents[1]) || []);
  assert.ok(documents[0].lineCount > documents[1].lineCount);
  assert.deepEqual(await format(documents[0]) || [], [], 'formatting is idempotent');

  // Changing editor settings must take effect without a manual restart.
  await vscode.workspace.getConfiguration('tex-ls', documents[1].uri)
    .update('lineWidth', 40, vscode.ConfigurationTarget.WorkspaceFolder);
  await eventually('updated folder configuration', async () => (await format(documents[1]))?.length > 0);

  // A newly created project configuration must override the editor setting.
  await vscode.workspace.fs.writeFile(uri('second', 'tex-ls.toml'), Buffer.from('[format]\nline-width = 100\n'));
  await eventually('project configuration overrides editor settings', async () => !(await format(documents[1]))?.length);

  const broken = await vscode.workspace.openTextDocument({ language: 'latex', content: '\\begin{itemize}\n\\end{enumerate}\n' });
  await vscode.window.showTextDocument(broken);
  await eventually('untitled diagnostics', () => vscode.languages.getDiagnostics(broken.uri).length > 0);
  await apply(broken, [vscode.TextEdit.replace(new vscode.Range(0, 0, broken.lineCount, 0), 'Valid text.\n')]);
  await eventually('diagnostics clear after repair', () => vscode.languages.getDiagnostics(broken.uri).length === 0);

  const bib = uri('first', 'references.bib');
  await vscode.workspace.fs.writeFile(bib, Buffer.from('@article{sample, title={A title}, author={Doe, Jane}, year={2026}}\n'));
  const bibliography = await vscode.workspace.openTextDocument(bib);
  await vscode.window.showTextDocument(bibliography);
  assert.equal(bibliography.languageId, 'bibtex');
  await eventually('BibTeX formatting', async () => (await format(bibliography))?.length > 0);

  await Promise.all([1, 2, 3].map(() => vscode.commands.executeCommand('tex-ls.restartServer')));
  await eventually('providers recover after restart', async () => (await format(bibliography))?.length > 0);
  console.log('VS Code integration passed: activation, symbols, UTF-16 definition/rename, safe fix-all and undo, read-only project inspection, completion, formatting, scoped settings, config watching, diagnostics, BibTeX, queued restarts.');
};
