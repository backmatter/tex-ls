#!/usr/bin/env python3
"""Download pinned real LSP benchmark sources and verify their content hashes."""
import argparse
import hashlib
import json
import pathlib
import urllib.request


def fetch(destination):
    manifest = pathlib.Path(__file__).resolve().parent.parent / "benches/lsp-projects.json"
    for source in json.loads(manifest.read_text()):
        path = destination / source["path"]
        if path.exists() and hashlib.sha256(path.read_bytes()).hexdigest() == source["sha256"]:
            continue
        data = urllib.request.urlopen(source["url"], timeout=30).read()
        if hashlib.sha256(data).hexdigest() != source["sha256"]:
            raise ValueError(f"Content hash mismatch: {source['url']}")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    print(f"Verified benchmark inputs in {destination}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", nargs="?", type=pathlib.Path, default=pathlib.Path("target/lsp-release-inputs"))
    fetch(parser.parse_args().destination)
