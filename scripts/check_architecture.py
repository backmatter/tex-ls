#!/usr/bin/env python3
"""Check workspace dependency direction without building or starting services."""
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"], cwd=ROOT
))
allowed = {
    "meaning-parser": set(),
    "meaning-formatter": {"meaning-parser"},
    "meaning-analysis": {"meaning-parser", "meaning-formatter"},
    "meaning-protocol": {"meaning-parser", "meaning-formatter", "meaning-analysis"},
    "meaning-browser": {"meaning-parser", "meaning-formatter", "meaning-analysis", "meaning-protocol"},
    "meaning-wasm": {"meaning-parser", "meaning-formatter"},
    "meaning": {"meaning-parser", "meaning-formatter", "meaning-analysis", "meaning-protocol"},
}
failures = []
for package in metadata["packages"]:
    name = package["name"]
    for dependency in package["dependencies"]:
        if dependency["kind"] == "dev":
            continue
        target = dependency["name"]
        if target in allowed and target not in allowed[name]:
            failures.append(f"{name} must not depend on {target}")
        if name == "meaning-protocol" and target == "salsa":
            failures.append("protocol must use the analysis read API, not Salsa directly")
        if name in {"meaning-analysis", "meaning-protocol", "meaning-browser"} and target in {
            "clap", "crossbeam-channel", "dirs", "ignore", "lsp-server", "rayon"
        }:
            failures.append(f"native dependency {target} leaked into {name}")
if failures:
    raise SystemExit("\n".join(failures))
print("Architecture dependency boundaries passed.")
