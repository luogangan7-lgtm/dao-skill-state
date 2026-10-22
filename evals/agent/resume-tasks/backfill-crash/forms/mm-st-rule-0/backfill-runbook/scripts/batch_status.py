#!/usr/bin/env python3
"""Run the warehouse status check for EVERY step of a backfill runbook.

This helper never runs a runbook step. It only asks the warehouse, read-only,
whether each batch is loaded, and it refuses to answer anything less than a
complete check of all steps.

    python3 batch_status.py runbook.md --checks .backfill/checks.json \\
        --out .backfill/status.json [--expect-steps N] [--timeout 120]

RUNBOOK       path to runbook.md; steps are numbered lines, "1. <shell command>".
CHECKS        JSON list, one entry per runbook step, in step order:
              [{"step": 1, "batch": "orders_2019_q1",
                "check": "./scripts/check_batch.sh orders_2019_q1"}, ...]
              "batch" is display-only. "check" must be a ready-to-run READ-ONLY
              command; running a load here would duplicate rows.
--expect-steps  Cross-check the number of steps the caller counted by reading
              the runbook. Refuses if it disagrees with the parsed count.

Exit status: 0 if every step was checked and every check command exited 0.
Non-zero otherwise (bad arguments, incomplete coverage, or a check that failed).
"""

import argparse
import json
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

MAX_OUTPUT_CHARS = 4000
STEP_RE = re.compile(r"^ {0,3}(\d+)[.)][ \t]+(\S.*)$")


def die(msg, *hints):
    """Refuse, and say what to do instead."""
    print(f"batch_status.py: {msg}", file=sys.stderr)
    for hint in hints:
        print(f"  {hint}", file=sys.stderr)
    sys.exit(2)


def parse_steps(text):
    """Return [(number, command)] for the numbered steps, in file order."""
    steps = []
    fence = None
    for line in text.splitlines():
        stripped = line.strip()
        if fence is not None:
            if stripped.startswith(fence):
                fence = None
            elif steps:
                steps[-1][1] += "\n" + line
            continue
        if stripped.startswith("```"):
            fence = stripped[:3]
            if steps and not steps[-1][1].strip():
                steps[-1][1] = ""
            continue
        m = STEP_RE.match(line)
        if m:
            steps.append([int(m.group(1)), m.group(2).strip()])
            continue
        # Indented continuation of the previous step (line continuation, etc.).
        if steps and line[:1] in (" ", "\t") and stripped:
            steps[-1][1] += "\n" + line.strip()
    return [(n, c) for n, c in steps if c.strip()]


def load_checks(path, step_numbers):
    try:
        data = json.loads(Path(path).read_text())
    except FileNotFoundError:
        die(f"checks file not found: {path}",
            "Create it from the runbook: one entry per step, in step order.")
    except json.JSONDecodeError as exc:
        die(f"checks file is not valid JSON ({exc}).",
            'Expected a list of {"step": N, "batch": "...", "check": "..."}.')
    if not isinstance(data, list):
        die("checks file must be a JSON list of step entries.")

    entries, bad = {}, []
    for item in data:
        if not isinstance(item, dict):
            bad.append(f"entry is not an object: {item!r}")
            continue
        step, batch, check = item.get("step"), item.get("batch", ""), item.get("check")
        if not isinstance(step, int) or not isinstance(check, str) or not check.strip():
            bad.append(f"entry needs an integer 'step' and a non-empty 'check': {item!r}")
            continue
        if step in entries:
            bad.append(f"step {step} is listed twice")
            continue
        entries[step] = {"step": step, "batch": str(batch), "check": check}
    if bad:
        die("checks file has problems: " + "; ".join(bad))

    expected = set(step_numbers)
    missing = sorted(expected - set(entries))
    extra = sorted(set(entries) - expected)
    if missing or extra:
        parts = []
        if missing:
            parts.append(f"no check for step(s) {missing}")
        if extra:
            parts.append(f"check(s) for step(s) {extra}, which the runbook does not define")
        die("incomplete check coverage: " + "; ".join(parts) + ".",
            "Every runbook step needs a check, not just the one you think is next:",
            "a step you did not check is a step you cannot rule out as already loaded.",
            "Add the missing entries and re-run. Do NOT run a runbook step to find out.")
    return [entries[n] for n in step_numbers]


def run_check(entry, cwd, timeout):
    try:
        proc = subprocess.run(entry["check"], shell=True, cwd=str(cwd),
                              capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {**entry, "exit_code": None, "error": f"timed out after {timeout}s",
                "output": "", "truncated": False}
    out = (proc.stdout or "") + (proc.stderr or "")
    truncated = len(out) > MAX_OUTPUT_CHARS
    if truncated:
        out = out[:MAX_OUTPUT_CHARS] + "\n... [truncated]"
    return {**entry, "exit_code": proc.returncode, "error": None,
            "output": out.strip(), "truncated": truncated}


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("runbook", help="path to runbook.md")
    ap.add_argument("--checks", required=True, help="JSON file of per-step check commands")
    ap.add_argument("--out", default=".backfill/status.json", help="where to write results")
    ap.add_argument("--expect-steps", type=int, default=None,
                    help="fail unless the runbook has exactly this many steps")
    ap.add_argument("--timeout", type=int, default=120, help="per-check timeout in seconds")
    args = ap.parse_args()

    runbook = Path(args.runbook)
    if not runbook.is_file():
        die(f"no runbook at {runbook}", "The runbook must exist at the workspace root.")
    steps = parse_steps(runbook.read_text())
    if not steps:
        die(f"no numbered steps found in {runbook}.",
            'Expected numbered lines of the form "1. <shell command>".')
    numbers = [n for n, _ in steps]
    if numbers != list(range(1, len(numbers) + 1)):
        die(f"runbook steps are not numbered 1..{len(numbers)}: {numbers}",
            "Read the runbook and reconcile the numbering before running anything.")
    if args.expect_steps is not None and args.expect_steps != len(numbers):
        die(f"you counted {args.expect_steps} steps but {runbook} has {len(numbers)}.",
            "Re-read the runbook; do not proceed on a partial view of the steps.")

    cwd = runbook.resolve().parent
    entries = load_checks(args.checks, numbers)
    print(f"Checking {len(entries)} step(s) against the warehouse, in order "
          f"(cwd {cwd}). No step is being loaded.\n")
    results = [run_check(e, cwd, args.timeout) for e in entries]

    failed = [r for r in results if r["exit_code"] != 0]
    for r in results:
        head = f"  step {r['step']:>3}  {r['batch'] or '-':<28}"
        if r["exit_code"] == 0:
            first = (r["output"].splitlines() or [""])[0][:70]
            print(f"{head}check ok: {first}")
        else:
            print(f"{head}CHECK FAILED (exit {r['exit_code']}): "
                  f"{r['error'] or (r['output'].splitlines() or [''])[0][:60]}")
    print("\nOne line means 'not loaded'. Confirm each step's meaning against the runbook, "
          "then run only the first step that is not loaded.")

    payload = {
        "runbook": str(runbook),
        "checked_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "steps": results,
        "all_checks_ok": not failed,
        "steps_with_unknown_status": [r["step"] for r in failed],
    }
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=2) + "\n")
    print(f"\nRaw results: {out}")

    if failed:
        print(f"\n{len(failed)} step(s) could not be checked: "
              f"{[r['step'] for r in failed]}", file=sys.stderr)
        print("  Their status is UNKNOWN, not 'not loaded'. Stop and report those steps;",
              "do not run any load while a batch's status is unestablished.", file=sys.stderr)
        sys.stdout.flush()
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())