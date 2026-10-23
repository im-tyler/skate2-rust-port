#!/usr/bin/env python3
"""The workspace clippy ratchet (AGENTS.md, Validation gate).

The tree carries a historical lint backlog (~12k diagnostics, all old
pedantic lints) that will be paid down over time, not in one mass edit.
This script enforces the one hard rule: the workspace clippy warning total
may never exceed the recorded baseline, and an intentional drop is
recorded with `--update` in the same change that caused it.

Usage:
    python3 tools/clippy_ratchet.py           # enforce against the baseline
    python3 tools/clippy_ratchet.py --update  # record the current (lower) total
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "tools" / "clippy-baseline.txt"

# A per-target summary line, not a diagnostic: `warning: `skate2-core` (lib) generated 447 warnings`
SUMMARY = re.compile(r"^warning: `[^`]+` \(.*\) generated \d+ warnings?")
PER_CRATE = re.compile(r"^warning: `([^`]+)` \(([^)]*)\) generated (\d+) warnings?")


def count_clippy() -> tuple[int, dict[str, int], str]:
    run = subprocess.run(
        ["cargo", "clippy", "--workspace", "--all-targets"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    lines = (run.stdout + run.stderr).splitlines()
    rustc = subprocess.run(
        ["rustc", "--version"], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.strip()
    total = 0
    per_crate: dict[str, int] = {}
    for line in lines:
        if not line.startswith("warning:") or SUMMARY.match(line):
            continue
        total += 1
    for line in lines:
        matched = PER_CRATE.match(line)
        if matched:
            crate, targets, generated = matched.groups()
            per_crate[f"{crate} ({targets})"] = max(
                per_crate.get(f"{crate} ({targets})", 0), int(generated)
            )
    return total, per_crate, rustc


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
        "# The total may only go down. Record an intentional drop with --update in the",
        "# same change that caused it. Per-crate lines are context, not enforced.",
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
