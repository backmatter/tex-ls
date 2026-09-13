"""Smoke-test and archive a native release executable."""
import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument('--source', type=pathlib.Path, required=True)
parser.add_argument('--target', required=True)
parser.add_argument('--tag', required=True)
parser.add_argument('--output', type=pathlib.Path, default=pathlib.Path('dist'))
args = parser.parse_args()
name = 'tex-ls.exe' if 'windows' in args.target else 'tex-ls'
binary = args.source / 'target' / args.target / 'release' / name
version = subprocess.check_output([str(binary), '--version'], text=True).strip()
assert version == f'tex-ls {args.tag.removeprefix("v")}', version
with tempfile.TemporaryDirectory() as directory:
    stage = pathlib.Path(directory)
    paper = stage / 'smoke.tex'
    paper.write_text('\\section{Hello}\nA short paragraph.\n', encoding='utf-8')
    for command in [('format', str(paper)), ('format', '--check', str(paper)), ('lint', str(paper))]:
        subprocess.run([str(binary), '--no-config', *command], check=True)
    messages = [
        {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {'capabilities': {}}},
        {'jsonrpc': '2.0', 'method': 'initialized', 'params': {}},
        {'jsonrpc': '2.0', 'id': 2, 'method': 'shutdown', 'params': None},
        {'jsonrpc': '2.0', 'method': 'exit', 'params': None},
    ]
    wire = b''
    for message in messages:
        body = json.dumps(message).encode()
        wire += f'Content-Length: {len(body)}\r\n\r\n'.encode() + body
    response = subprocess.run([str(binary), '--no-config', 'lsp'], input=wire,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30, check=True)
    replies = []
    output = response.stdout
    while output:
        header, output = output.split(b'\r\n\r\n', 1)
        length = int(header.split(b':', 1)[1])
        replies.append(json.loads(output[:length]))
        output = output[length:]
    assert any(item.get('id') == 1 and 'capabilities' in item.get('result', {}) for item in replies), replies
    assert any(item.get('id') == 2 and item.get('result', 'missing') is None for item in replies), replies
    paper.unlink()
    shutil.copy2(binary, stage / name)
    shutil.copy2(args.source / 'LICENSE', stage / 'LICENSE')
    for notice in ['unicode-math.LICENSE', 'unicode-math.NOTICE']:
        shutil.copy2(args.source / 'crates/tex-ls-parser/data' / notice, stage / notice)
    args.output.mkdir(parents=True, exist_ok=True)
    suffix = '.zip' if 'windows' in args.target else '.tar.gz'
    archive = args.output / f'tex-ls-{args.target}{suffix}'
    if suffix == '.zip':
        with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as target:
            for file in sorted(stage.iterdir()):
                target.write(file, file.name)
    else:
        with tarfile.open(archive, 'w:gz') as target:
            for file in sorted(stage.iterdir()):
                target.add(file, arcname=file.name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    print(f'{digest}  {archive.name}')
