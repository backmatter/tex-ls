#!/usr/bin/env python3
"""Measure multi-cursor selections, safe fixes, and BibTeX range formatting."""
import argparse
import hashlib
import json
import pathlib
import statistics
import tempfile
import time

from benchmark_lsp_rpc import Client, memory


def measure(binary, workload, size, encoding, repeats):
    bib = workload.startswith("bib_")
    unit = {
        "selection": r"é😀 \textbf{text}",
        "bib_selection": "@book{k,title={é😀 text}}",
        "fix_all": "é😀 $x^{2}$ ",
        "quickfix": "é😀 $x^{2}$ ",
        "unknown_actions": "\\notacommand{} é😀\n",
        "table_column": r"é😀 \\",
        "bib_format": "@book{k,title={é😀 text}}",
        "colors": r"é😀 \definecolor{demo}{HTML}{FF0000}",
        "color_presentations": r"é😀 \definecolor{demo}{HTML}{FF0000}",
        "file_rename": r"é😀 \input{child}",
    }[workload]
    source = unit * size
    if workload == "table_column":
        source = r"\begin{tabular}{c}" + source + r"\end{tabular}"
    width = len(unit.encode()) if encoding == "utf-8" else len(unit.encode("utf-16-le")) // 2
    positions = [{"line": 0, "character": i * width + 1} for i in range(size)]
    # Unsorted cursors and duplicates exercise the output-order contract.
    positions = positions[::2] + list(reversed(positions[1::2])) + positions[:2]
    last_line = source.rsplit("\n", 1)[-1]
    end = {"line": source.count("\n"), "character": len(last_line.encode()) if encoding == "utf-8" else len(last_line.encode("utf-16-le")) // 2}
    with tempfile.TemporaryDirectory(prefix="tex-ls-bulk-") as directory:
        path = pathlib.Path(directory) / ("main.bib" if bib else "main.tex")
        path.write_text(source)
        if workload in {"fix_all", "quickfix"}:
            (path.parent / "tex-ls.toml").write_text('[lint]\nselect = ["redundant-script-braces"]\n')
        if workload == "unknown_actions":
            (path.parent / "tex-ls.toml").write_text('[lint]\nselect = ["unknown-command"]\n')
        if workload == "file_rename":
            (path.parent / "child.tex").write_text("Child text")
        client = Client(binary)
        try:
            client.request("initialize", {"rootUri": path.parent.as_uri(),
                "initializationOptions": {"texmf": {"enabled": False}},
                "capabilities": {"general": {"positionEncodings": [encoding]},
                    "textDocument": {"codeAction": {"codeActionLiteralSupport": {
                        "codeActionKind": {"valueSet": ["source.fixAll.tex-ls", "quickfix", "refactor.rewrite"]}}}}}})
            client.send("initialized", {})
            client.send("textDocument/didOpen", {"textDocument": {"uri": path.as_uri(),
                "languageId": "bibtex" if bib else "latex", "version": 1, "text": source}})
            client.ready(quiet_seconds=0.1)
            params = {"textDocument": {"uri": path.as_uri()}}
            if workload == "file_rename":
                method = "workspace/willRenameFiles"
                params = {"files": [{"oldUri": (path.parent / "child.tex").as_uri(),
                                    "newUri": (path.parent / "renamed.tex").as_uri()}]}
            elif workload in {"colors", "color_presentations"}:
                method = "textDocument/documentColor"
                if workload == "color_presentations":
                    colors = client.request(method, params)
                    method = "textDocument/colorPresentation"
                    params.update(range=colors[-1]["range"], color=colors[-1]["color"])
            elif workload.endswith("selection"):
                method = "textDocument/selectionRange"
                params["positions"] = positions
            elif workload in {"fix_all", "quickfix", "table_column", "unknown_actions"}:
                method = "textDocument/codeAction"
                kind = {"fix_all": "source.fixAll.tex-ls", "quickfix": "quickfix", "table_column": "refactor.rewrite", "unknown_actions": "quickfix"}[workload]
                params.update(range={"start": {"line": 0, "character": 0}, "end": end},
                              context={"diagnostics": [], "only": [kind]})
            else:
                method = "textDocument/rangeFormatting"
                params.update(range={"start": {"line": 0, "character": 0}, "end": end},
                              options={"tabSize": 2, "insertSpaces": True})
            expected = client.request(method, params)
            assert expected, (workload, "empty response")
            if workload.endswith("selection"):
                assert len(expected) == len(positions)
            samples = []
            for _ in range(repeats):
                started = time.perf_counter()
                actual = client.request(method, params)
                samples.append((time.perf_counter() - started) * 1000)
                assert actual == expected, (workload, "unstable response")
            canonical = json.dumps(expected, sort_keys=True).replace(path.parent.as_uri(), "file:///project")
            return {"workload": workload, "size": size, "encoding": encoding,
                    "source_sha256": hashlib.sha256(source.encode()).hexdigest(),
                    "result_sha256": hashlib.sha256(canonical.encode()).hexdigest(),
                    "samples_ms": [round(value, 3) for value in samples],
                    "p50_ms": round(statistics.median(samples), 3),
                    "process_memory_kb": memory(client)}
        finally:
            client.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", default="target/release/tex-ls")
    parser.add_argument("--sizes", nargs="+", type=int, default=[1000, 4000])
    workloads = ["selection", "bib_selection", "fix_all", "quickfix", "table_column", "bib_format", "colors", "color_presentations", "file_rename", "unknown_actions"]
    parser.add_argument("--workloads", nargs="+", choices=workloads, default=workloads[:-1])
    parser.add_argument("--encodings", nargs="+", choices=["utf-8", "utf-16"], default=["utf-8", "utf-16"])
    parser.add_argument("--repeats", type=int, default=5)
    args = parser.parse_args()
    if args.repeats < 1 or any(size < 1 for size in args.sizes):
        parser.error("sizes and repeats must be positive")
    for workload in args.workloads:
        for encoding in args.encodings:
            for size in args.sizes:
                print(json.dumps(measure(str(pathlib.Path(args.binary).resolve()), workload, size,
                                         encoding, args.repeats)), flush=True)
