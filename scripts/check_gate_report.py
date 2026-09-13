#!/usr/bin/env python3
"""Validate a debug-format JSON report and emit sorted baseline rows."""
import json
import sys


def require(condition):
    if not condition:
        raise ValueError("inconsistent coverage, failure entries or exit status")


def distill(report, mode, status):
    require(type(report["schema_version"]) is int and report["schema_version"] == 1)
    require(report["checks"] == mode)
    require(report["execution_failed"] is False)
    require(type(report["files_checked"]) is int and report["files_checked"] > 0)
    require(type(report["files_skipped"]) is int and report["files_skipped"] >= 0)
    failures = report["failures"]
    require(isinstance(failures, list))
    require(type(report["failure_count"]) is int)
    require(report["failure_count"] == len(failures))
    require(status == int(bool(failures)))
    rows = set()
    kinds = {"losslessness", "idempotency", "content-change", "comment-change", "format-error"}
    classes = {"content-change", "non-fixed-point", "parse-error", "lossless-error", "format-error"}
    for failure in failures:
        path, kind, category = (failure[key] for key in ("path", "kind", "class"))
        require(isinstance(path, str) and path and not any(c in path for c in "\t\r\n"))
        if mode == "all":
            require(kind in kinds and category == kind)
            rows.add(f"{path}\t{kind}")
        else:
            require(mode == "trivia" and kind in {"trivia", "format-error"})
            require(category in classes)
            require(kind != "format-error" or category == "format-error")
            rows.add(f"{path}\t{kind}\t{category}")
    return sorted(rows)


if __name__ == "__main__":
    try:
        rows = distill(json.load(sys.stdin), sys.argv[1], int(sys.argv[2]))
    except (KeyError, TypeError, ValueError) as error:
        sys.exit(f"invalid debug-format report: {error}")
    for row in rows:
        print(row)
