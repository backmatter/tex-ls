const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { runTests } = require('@vscode/test-electron');

async function main() {
  delete process.env.ELECTRON_RUN_AS_NODE;
  const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'tex-ls-vscode-'));
  try {
    for (const folder of ['first', 'second']) {
      await fs.mkdir(path.join(temp, folder));
      await fs.mkdir(path.join(temp, folder, '.vscode'));
      await fs.writeFile(path.join(temp, folder, '.vscode', 'settings.json'), JSON.stringify({
        'tex-ls.lineWidth': folder === 'first' ? 40 : 100,
      }));
      await fs.writeFile(path.join(temp, folder, 'main.tex'),
        '\\documentclass{article}\n\\begin{document}\n\\section{Introduction}\\label{sec:intro}\nSee 🌻 \\ref{sec:intro}.\n\\end{document}\n');
    }
    const workspace = path.join(temp, 'test.code-workspace');
    await fs.writeFile(workspace, JSON.stringify({ folders: [{ path: 'first' }, { path: 'second' }] }));
    const userData = path.join(temp, 'user');
    await fs.mkdir(path.join(userData, 'User'), { recursive: true });
    await fs.writeFile(path.join(userData, 'User', 'settings.json'), JSON.stringify({
      'tex-ls.texmf.enabled': false,
      'tex-ls.server.path': process.env.TEX_LS_TEST_SERVER || '',
      'editor.semanticHighlighting.enabled': true,
      'security.workspace.trust.enabled': false,
      'workbench.startupEditor': 'none',
      'extensions.autoUpdate': false,
      'update.mode': 'none',
    }));
    await runTests({
      extensionDevelopmentPath: process.env.TEX_LS_TEST_EXTENSION || path.resolve(__dirname, '..'),
      extensionTestsPath: path.resolve(__dirname, 'integration.cjs'),
      vscodeExecutablePath: process.env.VSCODE_EXECUTABLE_PATH,
      version: process.env.VSCODE_TEST_VERSION || 'stable',
      extensionTestsEnv: { TEX_LS_TEST_WORKSPACE: temp },
      launchArgs: [workspace, '--user-data-dir', userData, '--extensions-dir', path.join(temp, 'extensions'),
        ...(process.platform === 'linux' ? ['--ozone-platform=x11'] : []),
        '--log', 'trace', '--disable-extensions', '--skip-welcome', '--skip-release-notes', '--disable-gpu', '--no-sandbox'],
    });
  } catch (error) {
    const logs = path.join(temp, 'user', 'logs');
    for (const name of await fs.readdir(logs, { recursive: true }).catch(() => [])) {
      if (/tex-ls.*\.log$|exthost\.log$/.test(name)) {
        const contents = await fs.readFile(path.join(logs, name), 'utf8');
        console.error(`VS Code test log: ${name}\n${contents.slice(-30000)}`);
      }
    }
    throw error;
  } finally {
    await fs.rm(temp, { recursive: true, force: true });
  }
}

main().catch((error) => { console.error(error); process.exitCode = 1; });
