"""Add versioned installers and a checksum manifest to release archives."""
import hashlib
import pathlib
import re
import sys

TAG = sys.argv[1]
DEST = pathlib.Path(sys.argv[2])
if not re.fullmatch(r'v\d+\.\d+\.\d+', TAG):
    raise SystemExit('Expected a stable version tag such as v0.1.0')
ROOT = pathlib.Path(__file__).resolve().parent.parent
for name in ['install.sh', 'install.ps1']:
    (DEST / name).write_text((ROOT / 'distribution' / name).read_text().replace('@TAG@', TAG))
files = sorted(path for path in DEST.iterdir() if path.is_file() and path.name != 'SHA256SUMS')
(DEST / 'SHA256SUMS').write_text(''.join(
    f'{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n' for path in files
))
