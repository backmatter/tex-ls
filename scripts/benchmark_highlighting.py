#!/usr/bin/env python3
"""Measure full/range semantic tokens, outlines, and tokens after a prose edit."""
import argparse
import hashlib
import json
import pathlib
import statistics
import tempfile
import time

from benchmark_lsp_rpc import Client, memory, position


TYPES = ["macro", "type", "variable", "property", "keyword", "string", "number",
         "namespace", "texText", "texLiteral", "texPath", "texMath", "comment",
         "function", "parameter", "operator"]


def identifier(index):
    result = ""
    while True:
        result = chr(ord("a") + index % 26) + result
        index = index // 26 - 1
        if index < 0:
            return result


def source_for(workload, size):
    if workload == "unicode_bibliography":
        return " ".join(f"@article{{key{i}, title={{Printed text é 😀}}, year={{2026}}, "
                        f"author={{Example Author}}}}" for i in range(size))
    if workload == "bibliography":
        return "".join(f"@article{{key{i}, title={{Printed text 2026}}, year={{2026}}, "
                       f"author={{Example Author}}, doi={{10.1234/example{i}}}}}\n"
                       for i in range(size))
    if workload == "definitions":
        body = "".join(f"\\newcommand{{\\macro{identifier(i)}}}[1]{{\\textbf{{#1}}}}\n"
                       f"\\macro{identifier(i)}{{Printed text}}\n" for i in range(size))
    elif workload == "csnames":
        body = "".join(f"\\csname item{identifier(i)}\\endcsname\n" for i in range(size))
    elif workload == "math":
        body = "\\begin{align}\nx_i + \\alpha &= 42 \\\\\n\\end{align}\n" * size
    elif workload == "unicode_math":
        body = "$" + "α + \\beta + 42 " * size + "$\n"
    elif workload == "python":
        body = "\\begin{minted}{python}\n" + "def example(x):\n    return x + 42 # printed code\n" * size + "\\end{minted}\n"
    elif workload == "literal":
        body = "\\detokenize{\\label{fake}\\ref{fake} Printed text 42}\n" * size
    elif workload == "keyvals":
        body = "\\includegraphics[width=2cm,height=3cm,clip=true]{image.pdf}\n" * size
    else:
        body = "Ordinary prose with printed numbers 42 and \\emph{some emphasis}.\n" * size
    return "\\documentclass{article}\n\\begin{document}\n\\section{Probe}\n" + body + "\\end{document}\n"


def decode(data):
    assert len(data) % 5 == 0
    result = []
    line = column = 0
    for offset in range(0, len(data), 5):
        delta, start, length, kind, modifiers = data[offset:offset + 5]
        assert delta >= 0 and start >= 0 and length > 0
        line += delta
        column = column + start if delta == 0 else start
        if result:
            previous = result[-1]
            assert line > previous[0] or column >= previous[1] + previous[2]
        result.append((line, column, length, kind, modifiers))
    return result


def purposes_digest(tokens):
    """Ignore harmless segmentation changes while preserving exact source roles."""
    merged = []
    for line, column, length, kind, modifiers in tokens:
        if merged and merged[-1][0] == line and merged[-1][1] + merged[-1][2] == column and merged[-1][3:] == [kind, modifiers]:
            merged[-1][2] += length
        else:
            merged.append([line, column, length, kind, modifiers])
    return hashlib.sha256(json.dumps(merged).encode()).hexdigest()


def measure(binary, workload, size, repeats, hierarchical_symbols=True):
    source = source_for(workload, size)
    is_bib = workload in {"bibliography", "unicode_bibliography"}
    with tempfile.TemporaryDirectory(prefix="tex-ls-highlighting-") as directory:
        path = pathlib.Path(directory) / ("refs.bib" if is_bib else "main.tex")
        path.write_text(source)
        client = Client(binary)
        try:
            init = client.request("initialize", {"rootUri": path.parent.as_uri(),
                "initializationOptions": {"texmf": {"enabled": False}},
                "capabilities": {"textDocument": {"diagnostic": {},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": hierarchical_symbols}, "semanticTokens": {
                    "requests": {"full": True, "range": True}, "formats": ["relative"],
                    "tokenTypes": TYPES, "tokenModifiers": ["declaration", "texBold", "texItalic"]}}}})
            legend = init["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"]
            client.send("initialized", {})
            document = {"textDocument": {"uri": path.as_uri()}}
            started = time.perf_counter()
            client.send("textDocument/didOpen", {"textDocument": {"uri": path.as_uri(),
                "languageId": "bibtex" if is_bib else "latex", "version": 1, "text": source}})
            full = client.request("textDocument/semanticTokens/full", document)["data"]
            cold_ms = (time.perf_counter() - started) * 1000
            decoded = decode(full)
            assert decoded and all(token[3] < len(legend) for token in decoded)
            client.ready(quiet_seconds=0.1)
            start_line = source.count("\n") // 2
            selection = {"start": {"line": start_line, "character": 0},
                         "end": {"line": start_line + 10, "character": 0}}
            expected = [token for token in decoded if start_line <= token[0] < start_line + 10]
            symbols = client.request("textDocument/documentSymbol", document)
            diagnostics = client.request("textDocument/diagnostic", document)["items"]
            full_samples, range_samples, edit_samples, symbol_samples, diagnostic_samples = [], [], [], [], []
            for _ in range(repeats):
                started = time.perf_counter()
                result = client.request("textDocument/semanticTokens/full", document)["data"]
                full_samples.append((time.perf_counter() - started) * 1000)
                assert result == full
                started = time.perf_counter()
                result = client.request("textDocument/semanticTokens/range", {**document, "range": selection})["data"]
                range_samples.append((time.perf_counter() - started) * 1000)
                assert decode(result) == expected
                started = time.perf_counter()
                result = client.request("textDocument/documentSymbol", document)
                symbol_samples.append((time.perf_counter() - started) * 1000)
                assert result == symbols
                started = time.perf_counter()
                result = client.request("textDocument/diagnostic", document)["items"]
                diagnostic_samples.append((time.perf_counter() - started) * 1000)
                assert result == diagnostics
            edit_at = source.index("Printed text") if is_bib else source.index("Probe")
            edit_position = position(source, edit_at)
            for version in range(2, repeats + 2):
                client.send("textDocument/didChange", {"textDocument": {"uri": path.as_uri(), "version": version},
                    "contentChanges": [{"range": {"start": edit_position, "end": edit_position}, "text": "x"}]})
                started = time.perf_counter()
                result = client.request("textDocument/semanticTokens/full", document)["data"]
                edit_samples.append((time.perf_counter() - started) * 1000)
                assert decode(result)
            return {"workload": workload, "size": size, "bytes": len(source.encode()),
                    "source_sha256": hashlib.sha256(source.encode()).hexdigest(),
                    "tokens_sha256": hashlib.sha256(json.dumps(full).encode()).hexdigest(),
                    "purposes_sha256": purposes_digest(decoded),
                    "edited_purposes_sha256": purposes_digest(decode(result)),
                    "cold_ms": round(cold_ms, 3), "tokens": len(full) // 5,
                    "full_ms": [round(x, 3) for x in full_samples],
                    "range_ms": [round(x, 3) for x in range_samples],
                    "edit_ms": [round(x, 3) for x in edit_samples],
                    "symbol_ms": [round(x, 3) for x in symbol_samples],
                    "symbol_p50_ms": round(statistics.median(symbol_samples), 3),
                    "diagnostic_ms": [round(x, 3) for x in diagnostic_samples],
                    "diagnostic_p50_ms": round(statistics.median(diagnostic_samples), 3),
                    "diagnostics_sha256": hashlib.sha256(json.dumps(diagnostics, sort_keys=True).replace(path.as_uri(), "file:///project/" + path.name).encode()).hexdigest(),
                    "hierarchical_symbols": hierarchical_symbols,
                    "symbols_sha256": hashlib.sha256(json.dumps(symbols, sort_keys=True).replace(path.as_uri(), "file:///project/" + path.name).encode()).hexdigest(),
                    "full_p50_ms": round(statistics.median(full_samples), 3),
                    "range_p50_ms": round(statistics.median(range_samples), 3),
                    "edit_p50_ms": round(statistics.median(edit_samples), 3),
                    "process_memory_kb": memory(client)}
        finally:
            client.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", default="target/release/tex-ls")
    parser.add_argument("--sizes", nargs="+", type=int, default=[250, 1000, 4000])
    parser.add_argument("--workloads", nargs="+", choices=["prose", "definitions", "csnames", "math", "unicode_math", "python", "literal", "keyvals", "bibliography", "unicode_bibliography"],
                        default=["prose", "definitions", "csnames", "math", "unicode_math", "python", "literal", "keyvals", "bibliography", "unicode_bibliography"])
    parser.add_argument("--flat-symbols", action="store_true", help="Measure clients without hierarchical outline support")
    parser.add_argument("--repeats", type=int, default=5)
    args = parser.parse_args()
    if args.repeats < 1 or any(size < 1 for size in args.sizes):
        parser.error("sizes and repeats must be positive")
    binary = str(pathlib.Path(args.binary).resolve())
    for workload in args.workloads:
        for size in args.sizes:
            print(json.dumps(measure(binary, workload, size, args.repeats, not args.flat_symbols)), flush=True)
