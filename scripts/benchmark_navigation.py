#!/usr/bin/env python3
"""Measure bulk navigation on long ASCII and Unicode lines without disk edits."""
import argparse
import hashlib
import json
import pathlib
import statistics
import tempfile
import time

from benchmark_lsp_rpc import Client, memory, position
from benchmark_highlighting import identifier


def fixture(workload, size):
    prefix = "é😀 " if workload != "ascii_labels" else "text "
    if workload == "bib_strings":
        body = "".join(f"@article{{k{i},title=brand,note={{{prefix}}}}}" for i in range(size))
        source = body + "@string{brand={Example}}"
        return source, source.index("brand"), True
    if workload == "bib_links":
        source = "".join(f"@article{{k{i},title={{{prefix}}},url={{https://example.org/{i}}}}}" for i in range(size))
        return source, 0, True
    if workload == "bib_folding":
        source = "".join(f"@article{{k{i},title={{{prefix}}}}}" for i in range(size))
        return source, 0, True
    identity = "commands" if workload in {"many_definitions", "redefinitions", "many_files"} else workload
    definition, use, target = {
        "labels": (r"\label{demo}", r"\ref{demo}", "demo"),
        "ascii_labels": (r"\label{demo}", r"\ref{demo}", "demo"),
        "citations": (r"\bibitem{demo} Example", r"\cite{demo}", "demo"),
        "commands": (r"\newcommand{\demo}{Example}", r"\demo{}", "demo"),
        "environments": (r"\newenvironment{demo}{}{}", r"\begin{demo}x\end{demo}", "demo"),
        "glossary": (r"\newglossaryentry{demo}{name=Example,description=Example}", r"\gls{demo}", "demo"),
        "links": ("", r"\input{child}", "child"),
    }[identity]
    extra = "".join(f"\\newcommand{{\\macro{identifier(i)}}}{{Example}}" for i in range(size)) if workload == "many_definitions" else ""
    if workload == "redefinitions":
        extra = definition + (r"\renewcommand{\demo}{é😀}" * size)
        definition = ""
    if workload == "many_files":
        extra = "".join(f"\\input{{child{i}}}" for i in range(size))
    source = r"\documentclass{article}\begin{document}" + extra + (prefix + use) * (1 if workload == "many_files" else size) + definition + r"\end{document}"
    return source, source.index(use) + use.index(target), False


def measure(binary, workload, size, repeats):
    source, offset, bib = fixture(workload, size)
    with tempfile.TemporaryDirectory(prefix="tex-ls-navigation-") as directory:
        path = pathlib.Path(directory) / ("refs.bib" if bib else "main.tex")
        path.write_text(source)
        if workload == "links":
            (path.parent / "child.tex").write_text("Child text")
        if workload == "many_files":
            unrelated = "".join(f"\\newcommand{{\\other{identifier(i)}}}{{Example}}" for i in range(16))
            for i in range(size):
                (path.parent / f"child{i}.tex").write_text(unrelated)
        client = Client(binary)
        try:
            client.request("initialize", {"rootUri": path.parent.as_uri(),
                "initializationOptions": {"texmf": {"enabled": False}},
                "capabilities": {"general": {"positionEncodings": ["utf-16"]},
                                 "textDocument": {"definition": {"linkSupport": True}}}})
            client.send("initialized", {})
            client.send("textDocument/didOpen", {"textDocument": {"uri": path.as_uri(),
                "languageId": "bibtex" if bib else "latex", "version": 1, "text": source}})
            client.ready(quiet_seconds=0.1)
            document = {"textDocument": {"uri": path.as_uri()}}
            cursor = {**document, "position": position(source, offset)}
            if workload in {"links", "bib_links"}:
                requests = [("links", "textDocument/documentLink", document)]
            elif workload == "bib_folding":
                requests = [("folding", "textDocument/foldingRange", document)]
            else:
                requests = [("references", "textDocument/references", {**cursor, "context": {"includeDeclaration": True}}),
                            ("references_uses", "textDocument/references", {**cursor, "context": {"includeDeclaration": False}}),
                            ("definition", "textDocument/definition", cursor),
                            ("rename", "textDocument/rename", {**cursor, "newName": "replacement"}),
                            ("highlight", "textDocument/documentHighlight", cursor)]
            result = {"workload": workload, "size": size, "source_bytes": len(source.encode()),
                      "source_sha256": hashlib.sha256(source.encode()).hexdigest()}
            for name, method, params in requests:
                expected = client.request(method, params)
                assert expected is not None, (workload, method)
                if name != "folding":
                    assert expected, (workload, method)
                samples = []
                for _ in range(repeats):
                    started = time.perf_counter()
                    actual = client.request(method, params)
                    samples.append((time.perf_counter() - started) * 1000)
                    assert actual == expected, (workload, method, "unstable response")
                canonical = json.dumps(expected, sort_keys=True).replace(path.parent.as_uri(), "file:///project")
                count = sum(map(len, expected["changes"].values())) if name == "rename" else len(expected)
                result[name] = {"count": count, "samples_ms": [round(value, 3) for value in samples],
                                "p50_ms": round(statistics.median(samples), 3),
                                "sha256": hashlib.sha256(canonical.encode()).hexdigest()}
            result["process_memory_kb"] = memory(client)
            return result
        finally:
            client.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", default="target/release/tex-ls")
    parser.add_argument("--sizes", nargs="+", type=int, default=[1000, 4000])
    parser.add_argument("--workloads", nargs="+", choices=["ascii_labels", "labels", "citations", "commands", "many_definitions", "redefinitions", "many_files", "environments", "glossary", "bib_strings", "links", "bib_links", "bib_folding"],
                        default=["ascii_labels", "labels", "citations", "commands", "many_definitions", "redefinitions", "environments", "glossary", "bib_strings", "links", "bib_links", "bib_folding"])
    parser.add_argument("--repeats", type=int, default=5)
    args = parser.parse_args()
    if args.repeats < 1 or any(size < 1 for size in args.sizes):
        parser.error("sizes and repeats must be positive")
    for workload in args.workloads:
        for size in args.sizes:
            print(json.dumps(measure(str(pathlib.Path(args.binary).resolve()), workload, size, args.repeats)), flush=True)
