import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync, chmodSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: {
  binary: { type: 'string' }, target: { type: 'string' }, output: { type: 'string' },
} });
const targets = ['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64', 'win32-x64', 'win32-arm64'];
if (!values.binary || !targets.includes(values.target)) {
  throw new Error(`Usage: node scripts/package.mjs --binary PATH --target ${targets.join('|')} [--output DIRECTORY]`);
}
if (values.target !== `${process.platform}-${process.arch}`) {
  throw new Error('Package the extension on its target platform with the native release binary.');
}
const extension = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = resolve(extension, '../..');
const binary = resolve(values.binary);
const output = resolve(values.output || join(root, 'dist'));
const manifest = JSON.parse(readFileSync(join(extension, 'package.json'), 'utf8'));
if (manifest.version !== manifest.texLsServerVersion) {
  throw new Error('Extension and bundled server versions must match.');
}
const lock = JSON.parse(readFileSync(join(extension, 'package-lock.json'), 'utf8'));
if (lock.version !== manifest.version || lock.packages[''].version !== manifest.version) {
  throw new Error('Extension manifest and lockfile versions must match.');
}
const actual = execFileSync(binary, ['--version'], { encoding: 'utf8' }).trim();
if (actual !== `tex-ls ${manifest.texLsServerVersion}`) {
  throw new Error(`Expected bundled server ${manifest.texLsServerVersion}, got ${actual}`);
}
mkdirSync(join(extension, 'server'), { recursive: true });
mkdirSync(output, { recursive: true });
const destination = join(extension, 'server', values.target.startsWith('win32-') ? 'tex-ls.exe' : 'tex-ls');
copyFileSync(binary, destination);
chmodSync(destination, 0o755);
for (const notice of ['unicode-math.LICENSE', 'unicode-math.NOTICE']) {
  copyFileSync(join(root, 'crates/tex-ls-parser/data', notice), join(extension, 'server', notice));
}
execFileSync(process.execPath, [join(extension, 'node_modules/@vscode/vsce/vsce'), 'package',
  '--target', values.target, '--out', join(output, `tex-ls-${manifest.version}-${values.target}.vsix`)],
{ cwd: extension, stdio: 'inherit' });
