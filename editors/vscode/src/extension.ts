import * as vscode from 'vscode';
import { LanguageClient, type LanguageClientOptions } from 'vscode-languageclient/node';
import { serverCommand, serverSettings, withCurrentDocument } from './settings';

let controller: Controller | undefined;

function settings(uri?: vscode.Uri) {
  return serverSettings(vscode.workspace.getConfiguration('tex-ls', uri));
}

class Controller {
  private client: LanguageClient | undefined;
  private pending: Promise<void> = Promise.resolve();
  private disposed = false;

  constructor(private readonly context: vscode.ExtensionContext, private readonly output: vscode.LogOutputChannel) {}

  // Serialize restarts, configuration changes, and shutdown so only one server runs.
  schedule(action: () => Promise<void>): Promise<void> {
    this.pending = this.pending.then(async () => {
      if (!this.disposed) await action();
    }).catch((error: unknown) => {
      this.output.appendLine(String(error));
      void vscode.window.showErrorMessage(
        'tex-ls could not start or update the language server. Check tex-ls.server.path or reinstall the extension.',
        'Show Output',
      ).then((choice) => { if (choice) this.output.show(); });
    });
    return this.pending;
  }

  async restart(): Promise<void> {
    const previous = this.client;
    this.client = undefined;
    await previous?.dispose();
    const command = serverCommand(this.context.extensionPath,
      vscode.workspace.getConfiguration('tex-ls').get<string>('server.path', ''));
    const options: LanguageClientOptions = {
      documentSelector: ['latex', 'bibtex'].flatMap((language) =>
        ['file', 'untitled'].map((scheme) => ({ language, scheme }))),
      initializationOptions: settings(),
      outputChannel: this.output,
      traceOutputChannel: this.output,
      middleware: {
        workspace: {
          configuration: async (params, token, next) => {
            const values = await next(params, token);
            if (!Array.isArray(values)) return values;
            return params.items.map((item, index) => item.section === 'tex-ls'
              ? settings(item.scopeUri ? vscode.Uri.parse(item.scopeUri) : undefined)
              : values[index]);
          },
        },
        provideDocumentFormattingEdits: (document, options, token, next) =>
          withCurrentDocument(document, token, () => next(document, options, token)),
        provideDocumentRangeFormattingEdits: (document, range, options, token, next) =>
          withCurrentDocument(document, token, () => next(document, range, options, token)),
        provideDocumentRangesFormattingEdits: (document, ranges, options, token, next) =>
          withCurrentDocument(document, token, () => next(document, ranges, options, token)),
        provideOnTypeFormattingEdits: (document, position, character, options, token, next) =>
          withCurrentDocument(document, token, () => next(document, position, character, options, token)),
      },
    };
    this.output.appendLine(`Starting ${command} lsp`);
    const client = new LanguageClient('tex-ls', 'tex-ls', { command, args: ['lsp'] }, options);
    this.client = client;
    try {
      await client.start();
    } catch (error) {
      this.client = undefined;
      await client.dispose();
      throw error;
    }
  }

  async configure(): Promise<void> {
    if (this.client?.isRunning()) {
      await this.client.sendNotification('workspace/didChangeConfiguration', { settings: settings() });
    }
  }

  async inspectProject(uri: vscode.Uri): Promise<unknown> {
    await this.pending;
    if (!this.client?.isRunning()) {
      throw new Error('tex-ls is not running. Use tex-ls: Restart Language Server.');
    }
    return this.client.sendRequest('workspace/executeCommand', {
      command: 'tex-ls.inspectProject', arguments: [{ uri: uri.toString() }],
    });
  }

  async dispose(): Promise<void> {
    this.disposed = true;
    await this.pending;
    await this.client?.dispose();
    this.client = undefined;
  }
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  if (!vscode.workspace.isTrusted) return;
  const output = vscode.window.createOutputChannel('tex-ls', { log: true });
  const server = new Controller(context, output);
  controller = server;
  const register = (name: string, action: () => unknown) => context.subscriptions.push(
    vscode.commands.registerCommand(name, async () => {
      try { return await action(); }
      catch (error) { void vscode.window.showErrorMessage(String(error)); return undefined; }
    }),
  );
  register('tex-ls.restartServer', () => server.schedule(() => server.restart()));
  register('tex-ls.showOutput', () => output.show());
  const sourceDocument = () => {
    const document = vscode.window.activeTextEditor?.document;
    if (!document || !['latex', 'bibtex'].includes(document.languageId)
      || !['file', 'untitled'].includes(document.uri.scheme)) {
      throw new Error('Open a LaTeX or BibTeX document first.');
    }
    return document;
  };
  register('tex-ls.fixAll', () => {
    sourceDocument();
    return vscode.commands.executeCommand('editor.action.codeAction', {
      kind: 'source.fixAll.tex-ls', apply: 'ifSingle',
    });
  });

  // Keep one read-only report per window. Inspection never writes project files.
  const reportUri = vscode.Uri.parse('tex-ls-project:/project.json');
  const reportChanged = new vscode.EventEmitter<vscode.Uri>();
  let report = '';
  context.subscriptions.push(reportChanged,
    vscode.workspace.registerTextDocumentContentProvider('tex-ls-project', {
      onDidChange: reportChanged.event,
      provideTextDocumentContent: () => report,
    }));
  register('tex-ls.showProject', async () => {
    const source = sourceDocument();
    if (source.uri.scheme !== 'file') {
      throw new Error('Save this document before inspecting its project.');
    }
    const result = await server.inspectProject(source.uri);
    report = JSON.stringify(result, null, 2) + '\n';
    reportChanged.fire(reportUri);
    const document = await vscode.workspace.openTextDocument(reportUri);
    await vscode.window.showTextDocument(document, { viewColumn: vscode.ViewColumn.Beside, preview: true });
  });
  context.subscriptions.push(output, vscode.workspace.onDidChangeConfiguration((event) => {
    if (!event.affectsConfiguration('tex-ls')) return;
    const restart = ['tex-ls.server.path', 'tex-ls.texmf']
      .some((key) => event.affectsConfiguration(key));
    void server.schedule(() => restart ? server.restart() : server.configure());
  }));
  await server.schedule(() => server.restart());
}

export async function deactivate(): Promise<void> {
  await controller?.dispose();
  controller = undefined;
}
