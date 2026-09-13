"""Check Unix installer routing, checksums, PATH quoting, and failed upgrades."""
import hashlib
import os
import pathlib
import subprocess
import tarfile
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent

class InstallerTests(unittest.TestCase):
    def install(self, system='Linux', arch='x86_64', corrupt=False):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            home = root / "home with 'quotes"
            home.mkdir()
            mocks = root / 'mocks'
            mocks.mkdir()
            (mocks / 'uname').write_text('#!/bin/sh\ncase "$1" in -s) echo "$TEST_OS";; -m) echo "$TEST_ARCH";; esac\n')
            (mocks / 'curl').write_text('#!/bin/sh\ncp "$TEST_ASSETS/${2##*/}" "$4"\n')
            for path in mocks.iterdir():
                path.chmod(0o755)
            stage = root / 'stage'
            stage.mkdir()
            (stage / 'tex-ls').write_text('#!/bin/sh\necho "tex-ls 0.1.0"\n')
            (stage / 'tex-ls').chmod(0o755)
            for name in ['LICENSE', 'unicode-math.LICENSE', 'unicode-math.NOTICE']:
                (stage / name).write_text('notice\n')
            cpu = 'aarch64' if arch in ['arm64', 'aarch64'] else 'x86_64'
            platform = 'apple-darwin' if system == 'Darwin' else 'unknown-linux-musl'
            archive = root / f'tex-ls-{cpu}-{platform}.tar.gz'
            with tarfile.open(archive, 'w:gz') as tar:
                for path in stage.iterdir():
                    tar.add(path, arcname=path.name)
            digest = '0' * 64 if corrupt else hashlib.sha256(archive.read_bytes()).hexdigest()
            (root / 'SHA256SUMS').write_text(f'{digest}  {archive.name}\n')
            binary = home / '.local/bin/tex-ls'
            binary.parent.mkdir(parents=True)
            binary.write_text('old binary')
            env = dict(os.environ, HOME=str(home), SHELL='/bin/sh',
                       PATH=f'{mocks}:{os.environ["PATH"]}', TEST_OS=system,
                       TEST_ARCH=arch, TEST_ASSETS=str(root), XDG_DATA_HOME=str(home / 'data'))
            env.pop('TEX_LS_INSTALL_DIR', None)
            env.pop('TEX_LS_NO_MODIFY_PATH', None)
            result = subprocess.run(['sh', str(ROOT / 'distribution/install.sh')], env=env,
                                    capture_output=True, text=True)
            if corrupt or arch == 'unsupported':
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(binary.read_text(), 'old binary')
                self.assertFalse((home / '.profile').exists())
            else:
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(subprocess.check_output([str(binary), '--version'], text=True).strip(), 'tex-ls 0.1.0')
                subprocess.run(['sh', '-n', str(home / '.profile')], check=True)
                subprocess.run(['sh', '-c', '. "$HOME/.profile"; tex-ls --version'], env=env, check=True, capture_output=True)
                self.assertTrue((home / 'data/tex-ls/LICENSE').exists())

    def test_platforms_and_shell_quoting(self):
        for system, arch in [('Linux', 'x86_64'), ('Linux', 'aarch64'), ('Darwin', 'x86_64'), ('Darwin', 'arm64')]:
            with self.subTest(system=system, arch=arch):
                self.install(system, arch)

    def test_checksum_failure_preserves_existing_install(self):
        self.install(corrupt=True)

    def test_unsupported_arch_preserves_existing_install(self):
        self.install(arch='unsupported')

if __name__ == '__main__':
    unittest.main()
