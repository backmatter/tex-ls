#!/usr/bin/env python3
"""Check release versions, or copy release-plz's server version to the extension."""

import argparse
import json
from pathlib import Path
import re
import tomllib


def synchronize(root: Path, *, write=False, changelog=False, tag=None):
    version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError(f"Expected a stable release version, got {version}")
    lock = tomllib.loads((root / "Cargo.lock").read_text())
    server = next(package for package in lock["package"] if package["name"] == "tex-ls")
    if server["version"] != version:
        raise ValueError("Cargo.toml and Cargo.lock versions differ")
    if tag is not None and tag != f"v{version}":
        raise ValueError(f"Release tag {tag} does not match server {version}")

    extension = root / "editors/vscode"
    manifest_path = extension / "package.json"
    lock_path = extension / "package-lock.json"
    manifest = json.loads(manifest_path.read_text())
    lock = json.loads(lock_path.read_text())
    if write:
        manifest["version"] = manifest["texLsServerVersion"] = version
        lock["version"] = lock["packages"][""]["version"] = version
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        lock_path.write_text(json.dumps(lock, indent=2) + "\n")
    elif any(value != version for value in [
        manifest["version"], manifest["texLsServerVersion"],
        lock["version"], lock["packages"][""]["version"],
    ]):
        raise ValueError("Server, extension, and lockfile versions must match; run "
                         "python3 scripts/sync_release_versions.py --write")

    if changelog:
        source = (root / "CHANGELOG.md").read_text()
        heading = re.search(rf"^## \[?{re.escape(version)}\]?(?:\s|\(|$).*$", source, re.M)
        if heading is None:
            raise ValueError(f"Server changelog has no entry for {version}")
        end = re.search(r"^## ", source[heading.end():], re.M)
        entry = source[heading.start():heading.end() + end.start() if end else len(source)].strip()
        path = extension / "CHANGELOG.md"
        previous = path.read_text()
        existing = re.search(rf"^## \[?{re.escape(version)}\]?(?:\s|\(|$).*$", previous, re.M)
        if existing:
            end = re.search(r"^## ", previous[existing.end():], re.M)
            finish = existing.end() + end.start() if end else len(previous)
            updated = previous[:existing.start()] + entry + "\n\n" + previous[finish:]
        else:
            start = re.search(r"^## ", previous, re.M)
            offset = start.start() if start else len(previous)
            updated = previous[:offset].rstrip() + "\n\n" + entry + "\n\n" + previous[offset:]
        if write:
            path.write_text(updated.rstrip() + "\n")
        elif updated.rstrip() != previous.rstrip():
            raise ValueError("Extension release notes differ from server release notes")
    return version


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true")
    parser.add_argument("--changelog", action="store_true")
    parser.add_argument("--tag")
    args = parser.parse_args()
    try:
        version = synchronize(Path(__file__).resolve().parent.parent, **vars(args))
    except (ValueError, KeyError, StopIteration) as error:
        parser.exit(1, f"Release metadata error: {error}\n")
    print(f"Server and extension versions agree: {version}")
