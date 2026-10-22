#!/usr/bin/env python3
"""Run a backfill runbook: check every step against the warehouse before running it.

Loading a batch twice duplicates rows, so this runner asks the warehouse whether a
step's batch is already loaded *before* running that step -- for every step, every
pass, starting at step 1. Starting, continuing, and resuming after a cut-off session
are the same command.

Exit codes:
    0  every step was already loaded, or ran successfully
    2  usage / validation error (nothing was run)
    3  a step command failed; the run stopped there
    4  a step's status could not be determined; the run stopped there, step NOT run
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

STEP_RE = re.compile(r"^(\d+)\.\s+(.*\S)\s*$")
OUTPUT_LIMIT = 20


def fail(message: str) -> "NoReturn":  # noqa: F821 - runtime only
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(2)


def parse_steps(runbook: Path, begin_at: int) -> list[tuple[int, str]]:
    """Read numbered steps `1. <shell command>` from the runbook, in file order.

    `begin_at` is the 1-based index of the numbered line that is step 1, used only
    when an earlier numbered list appears in the runbook's prose.
    """
    numbered: list[tuple[int, str]] = []
    for line in runbook.read_text(encoding="utf-8").splitlines():
        match = STEP_RE.match(line)
        if match:
            numbered.append((int(match.group(1)), match.group(2)))
    if begin_at > 1:
        if begin_at > len(numbered):
            fail(f"--steps-begin-at {begin_at} but the runbook has only {len(numbered)} numbered line(s)")
        print(
            f"note: starting at numbered line {begin_at} of {len(numbered)}; "
            f"ignoring earlier numbered lines as prose. Confirm this in the dry run.",
            file=sys.stderr,
        )
    steps = numbered[begin_at - 1:]
    if not steps:
        fail(f"no numbered steps found in {runbook}")
    expected = list(range(1, len(steps) + 1))
    if [n for n, _ in steps] != expected:
        fail(
            f"{runbook} step numbers are not 1..{len(steps)} in order "
            f"(found {[n for n, _ in steps]}). Re-read the runbook; do not edit it. "
            f"If an earlier numbered list in the prose is the cause, re-run with "
            f"--steps-begin-at <line number of the real step 1>."
        )
    return steps


def load_plan(plan_path: Path, runbook: Path, step_numbers: list[int]) -> dict[str, str]:
    """Validate the check plan: one check per step, and the status command the runbook names."""
    if not plan_path.is_file():
        fail(
            f"no check plan at {plan_path}. Read the runbook, note the command it names "
            "for asking whether a batch is loaded, and write the plan (see SKILL.md)."
        )
    try:
        plan = json.loads(plan_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        fail(f"{plan_path} is not valid JSON: {exc}")
    if not isinstance(plan, dict):
        fail(f"{plan_path} must contain a JSON object")

    status_command = plan.get("status_command")
    if not isinstance(status_command, str) or not status_command.strip():
        fail(
            f"{plan_path} has no 'status_command'. Copy the command the runbook names for "
            "asking whether a batch is loaded."
        )
    if status_command.strip() not in runbook.read_text(encoding="utf-8"):
        fail(
            f"'status_command' does not appear in {runbook}. Re-read the runbook and use "
            "the command it names, copied as written. Do not invent a check."
        )

    raw_checks = plan.get("checks")
    if not isinstance(raw_checks, dict):
        fail(f"{plan_path} has no 'checks' object")
    checks: dict[str, str] = {}
    for key, value in raw_checks.items():
        if not isinstance(value, str) or not value.strip():
            fail(f"check for step {key!r} in {plan_path} is not a non-empty command")
        checks[str(key)] = value
    missing = [n for n in step_numbers if str(n) not in checks]
    if missing:
        fail(
            f"no check for step(s) {missing}. Every step needs a check that exits 0 if that "
            "step's batch is loaded and 1 if it is not."
        )
    extra = [k for k in checks if not k.isdigit() or int(k) not in step_numbers]
    if extra:
        fail(f"check(s) for unknown step(s) {sorted(extra)} in {plan_path}")
    return checks


def run(command: str, root: Path) -> tuple[int, str]:
    """Run one shell command from the workspace root. Never retries."""
    proc = subprocess.run(
        command, shell=True, cwd=root, capture_output=True, text=True, errors="replace"
    )
    output = (proc.stdout + proc.stderr).strip()
    return proc.returncode, output


def tail(text: str) -> str:
    lines = text.splitlines()
    if not lines:
        return "(no output)"
    if len(lines) > OUTPUT_LIMIT:
        lines = [f"... ({len(lines) - OUTPUT_LIMIT} earlier lines omitted)"] + lines[-OUTPUT_LIMIT:]
    return "\n".join("    " + line for line in lines)


def dry_run(steps: list[tuple[int, str]], checks: dict[str, str], root: Path) -> None:
    print(f"dry run: {len(steps)} step(s) from {root / 'runbook.md'}; nothing will run.\n")
    for number, command in steps:
        print(f"  step {number}\n    load:  {command}\n    check: {checks[str(number)]}\n")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run a backfill runbook, checking the warehouse before every step.",
    )
    parser.add_argument(
        "--root", default=".", help="workspace root holding runbook.md (default: current dir)"
    )
    parser.add_argument("--runbook", default="runbook.md", help="runbook file (default: runbook.md)")
    parser.add_argument(
        "--plan", default=".dao/backfill-plan.json", help="check plan (default: .dao/backfill-plan.json)"
    )
    parser.add_argument(
        "--steps-begin-at",
        type=int,
        default=1,
        help="1-based index of the numbered line that is step 1; use only when the "
        "runbook has an earlier numbered list in its prose (default: 1)",
    )
    parser.add_argument("--dry-run", action="store_true", help="list steps and checks, run nothing")
    args = parser.parse_args()

    root = Path(args.root).resolve()
    if not root.is_dir():
        fail(f"workspace root {root} is not a directory")
    runbook = (root / args.runbook).resolve()
    if not runbook.is_file():
        fail(f"no runbook at {runbook}. Stop and ask for it; do not write one.")
    if args.steps_begin_at < 1:
        fail("--steps-begin-at must be 1 or greater")

    steps = parse_steps(runbook, args.steps_begin_at)
    checks = load_plan((root / args.plan).resolve(), runbook, [n for n, _ in steps])

    if args.dry_run:
        dry_run(steps, checks, root)
        return 0

    skipped: list[int] = []
    ran: list[int] = []
    for number, command in steps:
        check = checks[str(number)]
        code, output = run(check, root)
        if code == 0:
            print(f"[{number}/{len(steps)}] SKIP - already loaded, not run")
            if output:
                print(tail(output))
            skipped.append(number)
            continue
        if code != 1:
            print(
                f"[{number}/{len(steps)}] STOP - cannot tell whether this batch is loaded "
                f"(check exited {code}). Step NOT run. Report which step needs checking."
            )
            print(f"  step:   {command}")
            print(f"  check:  {check}")
            if output:
                print("  check output:\n" + tail(output))
            return 4

        print(f"[{number}/{len(steps)}] RUN - not loaded")
        print(f"  $ {command}")
        code, output = run(command, root)
        if code != 0:
            print(f"[{number}/{len(steps)}] FAILED - step command exited {code}. Stopping here.")
            print("  This load may still have committed. Do not retry it and do not change "
                  "anything to make it pass; report it and let the next pass ask the warehouse.")
            if output:
                print("  output:\n" + tail(output))
            return 3
        if output:
            print(tail(output))
        ran.append(number)

    print(f"\ndone: {len(skipped)} already loaded, {len(ran)} run this pass, {len(steps)} total.")
    if skipped:
        print(f"  skipped as already loaded: {', '.join(map(str, skipped))}")
    if ran:
        print(f"  run this pass: {', '.join(map(str, ran))}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print(
            "\ninterrupted. The current step may or may not have committed. Do not assume: "
            "run this same command again and let it check the warehouse.",
            file=sys.stderr,
        )
        raise SystemExit(130)