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
    "tex-ls-parser": set(),
    "tex-ls-formatter": {"tex-ls-parser"},
    "tex-ls-analysis": {"tex-ls-parser", "tex-ls-formatter"},
    "tex-ls-protocol": {"tex-ls-parser", "tex-ls-formatter", "tex-ls-analysis"},
    "tex-ls-browser": {"tex-ls-parser", "tex-ls-formatter", "tex-ls-analysis", "tex-ls-protocol"},
    "tex-ls": {"tex-ls-parser", "tex-ls-formatter", "tex-ls-analysis", "tex-ls-protocol"},
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
        if name == "tex-ls-analysis" and target in {"lsp-types", "gen-lsp-types"}:
            failures.append("analysis feature results must not depend on wire types")
        if name == "tex-ls-protocol" and target == "salsa":
            failures.append("protocol must use the analysis read API, not Salsa directly")
        if name in {"tex-ls-analysis", "tex-ls-protocol", "tex-ls-browser"} and target in {
            "clap", "crossbeam-channel", "dirs", "ignore", "lsp-server", "rayon"
        }:
            failures.append(f"native dependency {target} leaked into {name}")
if failures:
    raise SystemExit("\n".join(failures))
print("Architecture dependency boundaries passed.")
