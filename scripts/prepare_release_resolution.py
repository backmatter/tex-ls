#!/usr/bin/env python3
"""Resolve unpublished dependencies when release-plz inspects a previous tag."""

import argparse
import json
import re
from pathlib import Path
import subprocess
import tomllib


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def configuration(tag):
    revision = git("rev-parse", f"{tag}^{{commit}}")
    root = tomllib.loads(git("show", f"{revision}:Cargo.toml"))
    repository = root["workspace"]["package"]["repository"]
    patches = {}
    manifests = ["Cargo.toml"] + [f"{member}/Cargo.toml" for member in root["workspace"]["members"]]
    for path in manifests:
        manifest = tomllib.loads(git("show", f"{revision}:{path}"))
        if path != "Cargo.toml":
            patches[manifest["package"]["name"]] = {"git": repository, "rev": revision}
        for section in ["dependencies", "dev-dependencies", "build-dependencies"]:
            for name, dependency in manifest.get(section, {}).items():
                if isinstance(dependency, dict) and "git" in dependency:
                    if "rev" not in dependency:
                        raise ValueError(f"Release dependency {name} must pin a Git revision")
                    patches[dependency.get("package", name)] = {
                        "git": dependency["git"], "rev": dependency["rev"],
                    }
    lines = ["# Used only while release-plz reconstructs the previous release.", "[patch.crates-io]"]
    for name, dependency in sorted(patches.items()):
        fields = ", ".join(f"{key} = {json.dumps(value)}" for key, value in dependency.items())
        lines.append(f"{json.dumps(name)} = {{ {fields} }}")
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    tag = next(tag for tag in git("tag", "--list", "v*", "--sort=-version:refname").splitlines()
               if re.fullmatch(r"v\d+\.\d+\.\d+", tag))
    content = configuration(tag)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    # Never replace an existing Cargo configuration.
    with args.output.open("x") as output:
        output.write(content)
    print(f"Prepared release dependency resolution for {tag}")
