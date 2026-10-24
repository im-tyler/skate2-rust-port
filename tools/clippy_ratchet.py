#!/usr/bin/env python3
"""The workspace clippy ratchet (AGENTS.md, Validation gate).

The tree carries a historical lint backlog (~12k diagnostics, all old
pedantic lints) that will be paid down over time, not in one mass edit.
This script enforces the one hard rule: the workspace clippy warning total
may never exceed the recorded baseline, and an intentional drop is
recorded with `--update` in the same change that caused it.

The total is the number of distinct warning diagnostics across the
workspace: each `--message-format=json` warning keyed by its crate, primary
span (file, line, column), lint code and message, deduplicated across
targets. A lib's warning reported again by its test target is one warning.
Cargo's own `generated N warnings (M duplicates)` summaries are not used:
which target is charged the duplicates depends on the order cargo compiles
and replays targets, which shifts whenever the build graph changes (a new
dependency edge moved ~2k warnings between `skate2-audio`'s lib and test
targets on an unchanged tree). A set of keyed diagnostics has no order.

Usage:
    python3 tools/clippy_ratchet.py           # enforce against the baseline
    python3 tools/clippy_ratchet.py --update  # record the current (lower) total
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "tools" / "clippy-baseline.txt"

def count_clippy() -> tuple[int, dict[str, int], str]:
    run = subprocess.run(
        ["cargo", "clippy", "--workspace", "--all-targets", "--message-format=json"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    rustc = subprocess.run(
        ["rustc", "--version"], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.strip()
    seen: set[tuple] = set()
    per_crate: dict[str, int] = {}
    for line in run.stdout.splitlines():
        if not line.startswith("{"):
            continue
        record = json.loads(line)
        if record.get("reason") != "compiler-message":
            continue
        message = record["message"]
        if message.get("level") != "warning":
            continue
        spans = [span for span in message.get("spans", []) if span.get("is_primary")]
        # The summary lines (`N warnings emitted`) carry no span and no code.
        if not spans and message.get("code") is None:
            continue
        # `path+file:///…/skate2-core#0.1.0`, or `…/dir#name@0.1.0` when the
        # package name differs from its directory.
        url, _, fragment = record["package_id"].partition("#")
        crate = fragment.split("@", 1)[0] if "@" in fragment else url.rsplit("/", 1)[-1]
        primary = spans[0] if spans else {}
        key = (
            crate,
            primary.get("file_name"),
            primary.get("line_start"),
            primary.get("column_start"),
            (message.get("code") or {}).get("code"),
            message.get("message"),
        )
        if key in seen:
            continue
        seen.add(key)
        per_crate[crate] = per_crate.get(crate, 0) + 1
    if run.returncode != 0 and not seen:
        sys.exit(run.stderr[-2000:])
    return len(seen), per_crate, rustc


def read_baseline() -> tuple[int, str]:
    total = None
    rustc = ""
    for line in BASELINE.read_text(encoding="utf8").splitlines():
        if line.startswith("total:"):
            total = int(line.split(":", 1)[1].strip())
        elif line.startswith("rustc:"):
            rustc = line.split(":", 1)[1].strip()
    if total is None:
        sys.exit(f"{BASELINE} has no `total:` line; regenerate with --update")
    return total, rustc


def write_baseline(total: int, per_crate: dict[str, int], rustc: str) -> None:
    body = [
        "# The workspace clippy ratchet baseline, enforced by tools/clippy_ratchet.py.",
        "# The total is the number of distinct warning diagnostics (crate, primary span,",
        "# code, message) across every target — independent of cargo's compile and",
        "# replay order. It may only go down; record an intentional drop with --update",
        "# in the same change. Per-crate lines are context, not enforced.",
        f"rustc: {rustc.split()[1] if len(rustc.split()) > 1 else rustc}",
        f"total: {total}",
    ]
    body += [f"#   {name}: {count}" for name, count in sorted(per_crate.items())]
    BASELINE.write_text("\n".join(body) + "\n", encoding="utf8")


def main() -> int:
    update = "--update" in sys.argv[1:]
    total, per_crate, rustc = count_clippy()
    if update:
        old, _ = read_baseline() if BASELINE.exists() else (None, "")
        write_baseline(total, per_crate, rustc)
        print(f"clippy ratchet: baseline recorded at {total}" + (f" (was {old})" if old else ""))
        return 0
    baseline, baseline_rustc = read_baseline()
    if total > baseline:
        print(
            f"clippy ratchet: FAIL — {total} warnings exceeds the baseline {baseline}.\n"
            "Fix the new diagnostics in the files you touched, or record an "
            "intentional drop with --update (only downward)."
        )
        return 1
    label = rustc.split()[1] if len(rustc.split()) > 1 else rustc
    note = "" if baseline_rustc in rustc else (
        f"\nnote: baseline was recorded on rustc {baseline_rustc}, this run is {label}; "
        "review any jump before re-baselining."
    )
    print(f"clippy ratchet: OK — {total} of {baseline} allowed warnings.{note}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
