const assert = require('node:assert/strict');
const path = require('node:path');
const vscode = require('vscode');

async function eventually(description, check) {
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) {
    let timeout;
    try {
      const result = await Promise.race([
        Promise.resolve().then(check),
        new Promise((_, reject) => {
          timeout = setTimeout(() => reject(new Error(`Timed out: ${description}`)), deadline - Date.now());
        }),
      ]);
      if (result) return result;
    } catch (error) {
      // Startup/configuration refresh can cancel an otherwise valid request.
      if (error.name !== 'Canceled' && error.code !== -32800 && error.code !== -32801) throw error;
    } finally {
      clearTimeout(timeout);
    }
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
  const canFocusWindow = (await vscode.commands.getCommands(true)).includes('workbench.action.focusWindow');
  await eventually('source editor becomes active', async () => {
    // Recent VS Code versions can focus the native window on unattended desktops.
    if (canFocusWindow) await vscode.commands.executeCommand('workbench.action.focusWindow');
    await vscode.window.showTextDocument(document, { viewColumn: column, preview: false, preserveFocus: false });
    // Code-action and undo commands need editor focus, not just an active tab.
    await vscode.commands.executeCommand('workbench.action.focusActiveEditorGroup');
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
  for (const [line, column, otherLine, otherColumn, otherKind] of [
    [1, 9, 4, 5, 'closing'],
    [4, 7, 1, 7, 'opening'],
  ]) {
    const at = new vscode.Position(line, column);
    const links = await eventually('matching document delimiter definition', async () => {
      const result = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', document.uri, at);
      return result?.length && result;
    });
    assert.equal((links[0].targetUri || links[0].uri).toString(), document.uri.toString());
    assert.deepEqual(links[0].targetSelectionRange?.start || links[0].range.start,
      new vscode.Position(otherLine, otherColumn));
    const hovers = await eventually('matching document delimiter hover', async () => {
      const result = await vscode.commands.executeCommand('vscode.executeHoverProvider', document.uri, at);
      return result?.length && result;
    });
    assert.ok(hovers.some((hover) => hover.range.start.line === line
      && hover.contents.some((content) => content.value.includes(`Matching ${otherKind} delimiter:`))));
  }
  for (const [line, commandEnd, nameStart, nameEnd] of [[1, 6, 7, 15], [4, 4, 5, 13]]) {
    const command = await vscode.commands.executeCommand('vscode.executeHoverProvider',
      document.uri, new vscode.Position(line, 1));
    assert.ok(command?.some((hover) => hover.range.start.character === 0
      && hover.range.end.character === commandEnd));
    for (const column of [commandEnd, nameEnd]) {
      const brace = await vscode.commands.executeCommand('vscode.executeHoverProvider',
        document.uri, new vscode.Position(line, column));
      assert.deepEqual(brace || [], [], 'braces between adjacent hover targets are not hoverable');
    }
    const name = await vscode.commands.executeCommand('vscode.executeHoverProvider',
      document.uri, new vscode.Position(line, nameStart));
    assert.ok(name?.some((hover) => hover.range.start.character === nameStart
      && hover.range.end.character === nameEnd));
  }
  const colorUri = uri('first', 'highlighting.tex');
  const colorSource = String.raw`\begin{customenv}
\newcommand{\custom}{} \custom \tem \item
$\alpha+\unknown$ \% % \commenthidden
\verb|\inlinehidden|
\texttt{chapters/intro.tex}
\usetheme{metropolis}
\usetikzlibrary{calc}
\bibliographystyle{plain}
\end{customenv}`;
  for (const name of ['beamerthememetropolis.sty', 'tikzlibrarycalc.code.tex', 'plain.bst']) {
    await vscode.workspace.fs.writeFile(uri('first', name), Buffer.from('% module fixture\n'));
  }
  await vscode.workspace.fs.writeFile(colorUri, Buffer.from(colorSource));
  const colorDocument = await vscode.workspace.openTextDocument(colorUri);
  const legend = await eventually('semantic token legend', () =>
    vscode.commands.executeCommand('vscode.provideDocumentSemanticTokensLegend', colorUri));
  const colorTokens = await eventually('semantic command tokens', async () => {
    const result = await vscode.commands.executeCommand('vscode.provideDocumentSemanticTokens', colorUri);
    return result?.data?.length && result;
  });
  let tokenLine = 0, tokenColumn = 0;
  const commands = [];
  const modules = [];
  const printed = [];
  for (let i = 0; i < colorTokens.data.length; i += 5) {
    const [deltaLine, deltaColumn, length, type] = colorTokens.data.slice(i, i + 5);
    tokenLine += deltaLine;
    tokenColumn = (deltaLine ? 0 : tokenColumn) + deltaColumn;
    if (legend.tokenTypes[type] === 'namespace') {
      modules.push(colorDocument.lineAt(tokenLine).text.slice(tokenColumn, tokenColumn + length));
    }
    if (legend.tokenTypes[type] === 'macro') {
      commands.push(colorDocument.lineAt(tokenLine).text.slice(tokenColumn, tokenColumn + length));
    }
    if (legend.tokenTypes[type] === 'string') {
      printed.push(colorDocument.lineAt(tokenLine).text.slice(tokenColumn, tokenColumn + length));
    }
  }
  assert.deepEqual(commands, ['\\begin', '\\newcommand', '\\custom', '\\custom', '\\tem', '\\item',
    '\\alpha', '\\unknown', '\\%', '\\verb', '\\texttt', '\\usetheme', '\\usetikzlibrary', '\\bibliographystyle', '\\end']);
  assert.deepEqual(printed, ['\\inlinehidden', 'chapters/intro.tex']);
  assert.deepEqual(modules, ['metropolis', 'calc', 'plain']);
  for (const [name, target] of [['metropolis', 'beamerthememetropolis.sty'], ['calc', 'tikzlibrarycalc.code.tex'], ['plain', 'plain.bst']]) {
    const position = colorDocument.positionAt(colorSource.indexOf(name) + 1);
    await eventually(`module definition for ${name}`, async () => {
      const links = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', colorUri, position);
      return links?.some(link => (link.targetUri || link.uri).path.endsWith('/' + target));
    });
  }

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
  await showSource(fixDocument, vscode.ViewColumn.One);
  assert.equal(await vscode.commands.executeCommand('tex-ls.fixAll'), true,
    'the command applies the current server fix-all action');
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

  // The same package name must resolve to each folder's own relative TEXMF root.
  // The trees are editor-excluded, so installation changes need native refresh.
  const packageDocuments = [];
  for (const folder of ['first', 'second']) {
    const file = uri(folder, 'packages.tex');
    await vscode.workspace.fs.writeFile(file, Buffer.from('\\usepackage{integrationlocal}\n\\usepackage{integrationadded}\n'));
    packageDocuments.push(await vscode.workspace.openTextDocument(file));
  }
  const packageLinks = (doc) => vscode.commands.executeCommand('vscode.executeLinkProvider', doc.uri);
  for (let i = 0; i < packageDocuments.length; i++) {
    const expected = uri(i === 0 ? 'first' : 'second', '.local/texmf/integrationlocal.sty').toString();
    await eventually('folder-scoped package resolution', async () => {
      const links = await packageLinks(packageDocuments[i]);
      return links?.some((link) => link.target?.toString() === expected);
    });
  }
  const added = uri('first', '.local/texmf/integrationadded.sty');
  const hasAdded = async (doc) => {
    const links = await packageLinks(doc);
    return Array.isArray(links) ? links.some((link) => link.target?.toString() === added.toString()) : undefined;
  };
  assert.equal(await hasAdded(packageDocuments[0]), false);
  await vscode.workspace.fs.writeFile(added, Buffer.from('% installed during the session\n'));
  await vscode.workspace.fs.writeFile(uri('first', '.local/texmf/ls-R'), Buffer.from('./:\nintegrationlocal.sty\nintegrationadded.sty\n'));
  await eventually('package installation refreshes without editor events', () => hasAdded(packageDocuments[0]));
  assert.equal(await hasAdded(packageDocuments[1]), false, 'another workspace must not inherit installed packages');
  await vscode.workspace.fs.delete(added);
  await vscode.workspace.fs.writeFile(uri('first', '.local/texmf/ls-R'), Buffer.from('./:\nintegrationlocal.sty\n'));
  await eventually('package removal clears navigation', async () => (await hasAdded(packageDocuments[0])) === false);

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
  await vscode.workspace.fs.writeFile(vscode.Uri.file(path.join(root, '.integration-complete')), Buffer.from('passed\n'));
  console.log('VS Code integration passed: activation, symbols, UTF-16 definition/rename, safe fix-all and undo, read-only project inspection, completion, formatting, scoped settings, build-only formatting, isolated relative TEXMF roots, excluded package refresh, config watching, diagnostics, BibTeX, queued restarts.');
};
