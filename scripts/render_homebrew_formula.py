"""Render the Homebrew formula from published archive checksums."""
import pathlib
import re
import sys

tag = sys.argv[1]
if not re.fullmatch(r'v\d+\.\d+\.\d+', tag):
    raise SystemExit('Expected a stable version tag')
checksums = {name: digest for digest, name in
             (line.split() for line in pathlib.Path(sys.argv[2]).read_text().splitlines())}
print('''class TexLs < Formula
  desc "LaTeX and BibTeX language server, formatter, and linter"
  homepage "https://github.com/backmatter/tex-ls"
  version "%s"
  license "MIT"
''' % tag[1:])
for platform, suffix in [('macos', 'apple-darwin'), ('linux', 'unknown-linux-musl')]:
    print(f'  on_{platform} do')
    for cpu, arch in [('arm', 'aarch64'), ('intel', 'x86_64')]:
        name = f'tex-ls-{arch}-{suffix}.tar.gz'
        digest = checksums[name]
        if not re.fullmatch('[0-9a-f]{64}', digest):
            raise SystemExit(f'Invalid checksum for {name}')
        print(f'''    on_{cpu} do
      url "https://github.com/backmatter/tex-ls/releases/download/{tag}/{name}"
      sha256 "{digest}"
    end''')
    print('  end\n')
print('''  def install
    bin.install "tex-ls"
    pkgshare.install "unicode-math.LICENSE", "unicode-math.NOTICE"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/tex-ls --version")
    (testpath/"main.tex").write("\\\\section{Hello}\\nA short paragraph.\\n")
    system bin/"tex-ls", "--no-config", "format", "main.tex"
    system bin/"tex-ls", "--no-config", "format", "--check", "main.tex"
    system bin/"tex-ls", "--no-config", "lint", "main.tex"
  end
end''')
