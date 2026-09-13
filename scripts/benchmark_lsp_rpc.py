#!/usr/bin/env python3
"""Native LSP measurements on synthetic or supplied real projects (no disk edits)."""
import argparse
import hashlib
import json
import os
import pathlib
import queue
import re
import statistics
import subprocess
import tempfile
import threading
import time


class Client:
    def __init__(self, binary):
        self.process = subprocess.Popen([binary, "lsp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        self.messages = queue.Queue()
        self.next_id = 0
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        while True:
            headers = {}
            while line := self.process.stdout.readline():
                if line == b"\r\n":
                    break
                key, value = line.decode().split(":", 1)
                headers[key.lower()] = value.strip()
            if not headers:
                return
            self.messages.put(json.loads(self.process.stdout.read(int(headers["content-length"]))))

    def send(self, method=None, params=None, **extra):
        body = {"jsonrpc": "2.0", **extra}
        if method is not None:
            body.update(method=method, params=params)
        data = json.dumps(body).encode()
        self.process.stdin.write(f"Content-Length: {len(data)}\r\n\r\n".encode() + data)
        self.process.stdin.flush()

    def request(self, method, params):
        for _ in range(20):
            self.next_id += 1
            self.send(method, params, id=self.next_id)
            while True:
                message = self.messages.get(timeout=30)
                if "method" in message:
                    if "id" in message:
                        self.send(id=message["id"], result=None)
                    continue
                if message.get("id") == self.next_id:
                    break
            error = message.get("error", {})
            diagnostic_retry = (method in {"textDocument/diagnostic", "workspace/diagnostic"}
                                and error.get("code") == -32802
                                and (error.get("data") or {}).get("retriggerRequest") is not False)
            if error.get("code") == -32801 or diagnostic_retry:
                continue  # A source write cancelled this captured revision.
            if "error" in message:
                raise RuntimeError(message["error"])
            return message["result"]
        raise RuntimeError("Request repeatedly superseded")

    def status(self):
        return self.request("workspace/executeCommand", {"command": "tex-ls.inspectAcquisition", "arguments": []})

    def ready(self):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            status = self.status()
            if status["pendingFileAcquisitions"] == 0 and status["pendingCompilerAcquisitions"] == 0 and not status["installationLoading"]:
                return status
            time.sleep(0.01)
        raise TimeoutError("Acquisition did not settle")

    def close(self):
        try:
            self.request("shutdown", None)
            self.send("exit", None)
            self.process.wait(timeout=10)
        finally:
            if self.process.poll() is None:
                self.process.kill()
            self.process.wait()


def measure(binary, count, idle_seconds=3):
    with tempfile.TemporaryDirectory(prefix="tex-ls-rpc-") as temporary:
        root = pathlib.Path(temporary)
        includes = []
        for index in range(count - 1):
            name = f"chapter{index:04}"
            (root / f"{name}.tex").write_text("Prose in a chapter.\n" * 32)
            includes.append(f"\\input{{{name}}}\n")
        text = "\\documentclass{article}\n" + "".join(includes) + "\\sect"
        path = root / "main.tex"
        path.write_text(text)
        client = Client(binary)
        try:
            client.request("initialize", {"rootUri": root.as_uri(), "capabilities": {"textDocument": {"diagnostic": {}}}, "initializationOptions": {"texmf": {"enabled": False}}})
            client.send("initialized", {})
            started = time.perf_counter()
            client.send("textDocument/didOpen", {"textDocument": {"uri": path.as_uri(), "languageId": "latex", "version": 1, "text": text}})
            params = {"textDocument": {"uri": path.as_uri()}, "position": {"line": text.count("\n"), "character": 5}}
            result = client.request("textDocument/completion", params)
            cold = (time.perf_counter() - started) * 1000
            assert any(item["label"] == "section" for item in result["items"])
            initial = client.ready()
            before = initial["sourceReadAttempts"]
            compiler_before = initial["compilerProbeAttempts"]
            warm = []
            for version in range(2, 7):
                changed = f"Prose edit {version}.\n" + text
                client.send("textDocument/didChange", {"textDocument": {"uri": path.as_uri(), "version": version}, "contentChanges": [{"text": changed}]})
                started = time.perf_counter()
                result = client.request("textDocument/completion", {**params, "position": {"line": changed.count("\n"), "character": 5}})
                warm.append((time.perf_counter() - started) * 1000)
                assert any(item["label"] == "section" for item in result["items"])
            settled = client.ready()
            reads = settled["sourceReadAttempts"] - before
            compiler_probes = settled["compilerProbeAttempts"] - compiler_before
            assert compiler_probes == 0, f"Prose edits caused {compiler_probes} compiler artifact probes"
            assert reads == 0, f"Prose edits caused {reads} dependency reads"
            return {"idle_process_activity": idle_activity(client, idle_seconds), "files": count, "cold_ms": round(cold, 3), "warm_p50_ms": round(statistics.median(warm), 3), "warm_max_ms": round(max(warm), 3), "cold_source_reads": before, "warm_source_reads": reads, "cold_compiler_probes": compiler_before, "warm_compiler_probes": compiler_probes}
        finally:
            client.close()


def position(text, offset):
    preceding = text[:offset]
    return {"line": preceding.count("\n"), "character": len(preceding.rsplit("\n", 1)[-1].encode("utf-16-le")) // 2}


def process_activity(client):
    """Linux process counters, including fallback watching and background jobs."""
    root = pathlib.Path(f"/proc/{client.process.pid}")
    try:
        fields = (root / "stat").read_text().rpartition(")")[2].split()
        io = dict(line.split(": ", 1) for line in (root / "io").read_text().splitlines())
        return {"cpu_ms": (int(fields[11]) + int(fields[12])) * 1000 / os.sysconf("SC_CLK_TCK"),
                "read_syscalls": int(io["syscr"]), "read_chars": int(io["rchar"])}
    except (OSError, ValueError, KeyError, IndexError):
        return None


def idle_activity(client, seconds):
    if seconds <= 0:
        return None
    client.ready()
    before = process_activity(client)
    start = time.perf_counter()
    time.sleep(seconds)
    after = process_activity(client)
    return {"seconds": round(time.perf_counter() - start, 3),
            "delta": None if before is None or after is None else {
                key: round(after[key] - value, 3) for key, value in before.items()}}


def memory(client):
    """Linux process RSS and high-water mark; other hosts report unavailable."""
    status = pathlib.Path(f"/proc/{client.process.pid}/status")
    if not status.exists():
        return None
    values = dict(re.findall(r"^(VmRSS|VmHWM):\s+(\d+) kB", status.read_text(), re.M))
    return {name: int(value) for name, value in values.items()}


def measure_project(binary, path, idle_seconds=3):
    path = pathlib.Path(path).resolve()
    text = path.read_text(encoding="utf-8")
    is_bib = path.suffix == ".bib"
    if is_bib:
        probe = re.search(r"(?im)^\s*@((?!comment\b|string\b|preamble\b)[a-z]+)\s*[{(]", text)
        field = re.search(r"(?i)\btitle\s*=\s*[{\"]", text)
        edit_at = field.end() if field else 0
        insertion = "x" if field else "@comment{Benchmark edit}\n"
    else:
        probe = re.search(r"(?m)^\s*\\(documentclass|section|input)\b", text)
        begin = text.find(r"\begin{document}")
        edit_at = begin + len(r"\begin{document}") if begin >= 0 else 0
        insertion = "\nBenchmark prose.\n" if begin >= 0 else "% Benchmark edit\n"
    if probe is None:
        raise ValueError(f"No supported completion probe in {path}")
    # An exact existing name gives every workload the same observable assertion;
    # a two-letter fuzzy prefix can legitimately truncate this candidate.
    probe_at = probe.end(1)
    expected = probe.group(1).lower()
    document = {"uri": path.as_uri()}
    client = Client(binary)
    def timed(method, params):
        start = time.perf_counter()
        result = client.request(method, params)
        return result, round((time.perf_counter() - start) * 1000, 3)
    try:
        client.request("initialize", {"rootUri": path.parent.as_uri(), "capabilities": {"textDocument": {"diagnostic": {}}}, "initializationOptions": {"texmf": {"enabled": False}}})
        client.send("initialized", {})
        started = time.perf_counter()
        client.send("textDocument/didOpen", {"textDocument": {**document, "languageId": "bibtex" if is_bib else "latex", "version": 1, "text": text}})
        symbols = client.request("textDocument/documentSymbol", {"textDocument": document})
        first_structure = (time.perf_counter() - started) * 1000
        initial = client.ready()
        ready_ms = (time.perf_counter() - started) * 1000
        inspection = client.request("workspace/executeCommand", {"command":"tex-ls.inspectProject", "arguments":[document]})
        report, diagnostic_ms = timed("textDocument/diagnostic", {"textDocument": document})
        unchanged, unchanged_ms = timed("textDocument/diagnostic", {"textDocument": document, "previousResultId": report["resultId"]})
        assert unchanged["kind"] == "unchanged"
        workspace, workspace_ms = timed("workspace/diagnostic", {"previousResultIds": []})
        previous = [{"uri": item["uri"], "value": item["resultId"]} for item in workspace["items"]]
        unchanged_workspace, workspace_unchanged_ms = timed("workspace/diagnostic", {"previousResultIds": previous})
        assert all(item["kind"] == "unchanged" for item in unchanged_workspace["items"])
        baseline = client.ready()
        initial_memory = memory(client)
        samples = []
        previous_id = report["resultId"]
        for version in range(2, 7):
            where = position(text, edit_at)
            started = time.perf_counter()
            client.send("textDocument/didChange", {"textDocument": {**document, "version":version}, "contentChanges":[{"range":{"start":where,"end":where},"text":insertion}]})
            text = text[:edit_at] + insertion + text[edit_at:]
            if edit_at <= probe_at:
                probe_at += len(insertion)
            completion = client.request("textDocument/completion", {"textDocument":document,"position":position(text, probe_at)})
            edit_completion_ms = (time.perf_counter() - started) * 1000
            assert any(item["label"].lower() == expected for item in completion["items"]), (path, expected)
            report, edited_diagnostic_ms = timed("textDocument/diagnostic", {"textDocument":document,"previousResultId":previous_id})
            assert report["kind"] == "full", "editing source must invalidate diagnostics"
            previous_id = report["resultId"]
            samples.append({"edit_completion_ms":round(edit_completion_ms, 3), "diagnostic_ms":edited_diagnostic_ms})
        settled = client.ready()
        reads = settled["sourceReadAttempts"] - baseline["sourceReadAttempts"]
        probes = settled["compilerProbeAttempts"] - baseline["compilerProbeAttempts"]
        assert reads == 0, f"Body edits caused {reads} dependency reads"
        assert probes == 0, f"Body edits caused {probes} compiler probes"
        return {"idle_process_activity":idle_activity(client, idle_seconds), "project":str(path), "sha256":hashlib.sha256(path.read_bytes()).hexdigest(),
                "source_bytes":path.stat().st_size,"structure_items":len(symbols),
                "cold_structure_ms":round(first_structure,3),"cold_ready_ms":round(ready_ms,3),
                "document_diagnostic_ms":diagnostic_ms,"unchanged_document_ms":unchanged_ms,
                "workspace_diagnostic_ms":workspace_ms,"unchanged_workspace_ms":workspace_unchanged_ms,
                "tracked_documents":len(workspace["items"]),"diagnostics":sum(len(item.get("items",[])) for item in workspace["items"]),
                "samples":samples,"warm_source_reads":reads,"warm_compiler_probes":probes,
                "memory_after_diagnostics_kib":initial_memory,"memory_after_edits_kib":memory(client),
                "acquisition":initial,"project_inspection":inspection}
    finally:
        client.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", default="target/release/tex-ls")
    parser.add_argument("--project", action="append", help="Real root .tex or .bib; repeat for multiple workloads")
    parser.add_argument("--idle-seconds", type=float, default=3, help="Idle observation per workload; 0 disables it")
    args = parser.parse_args()
    if args.idle_seconds < 0:
        parser.error("--idle-seconds must be nonnegative")
    binary = str(pathlib.Path(args.binary).resolve())
    if args.project:
        for project in args.project:
            print(json.dumps(measure_project(binary, project, args.idle_seconds)), flush=True)
    else:
        for size in [1, 100, 1000]:
            print(json.dumps(measure(binary, size, args.idle_seconds)), flush=True)
