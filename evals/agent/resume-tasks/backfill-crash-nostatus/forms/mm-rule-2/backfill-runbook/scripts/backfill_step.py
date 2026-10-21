#!/usr/bin/env python3
"""Run one step of a data backfill runbook, exactly once.

Loading a batch twice duplicates rows, and the warehouse cannot be asked which
batches are loaded. So this script is the only safe way to run a step: it
records a step as pending *before* running it and marks it done only after the
command exits 0. Anything else leaves the step blocked, because a non-zero exit
does not tell you whether the load committed.

Usage:
    backfill_step.py status [--runbook PATH]
    backfill_step.py run <step-number> [--runbook PATH]

Exit codes:
    0  step completed and was recorded done, or status printed
    1  usage / runbook error (nothing was run)
    2  refused: a step is blocked, or this step is not the next one (nothing was run)
    3  the step ran and did not exit 0; its outcome is UNKNOWN (stop and report)

Standard library only.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "backfill-run-state/1"
STATE_DIRNAME = ".backfill-state"
STEP_RE = re.compile(r"^(\s*)(\d+)\.\s+(.*\S)\s*$")

STATUS_TODO = "todo"          # never attempted — safe to run
STATUS_PENDING = "pending"    # attempt claimed, no result recorded — outcome unknown
STATUS_DONE = "done"          # exited 0
STATUS_FAILED = "failed"      # ran, did not exit 0 — outcome unknown

EXIT_OK = 0
EXIT_USAGE = 1
EXIT_REFUSED = 2
EXIT_STEP_FAILED = 3

SHORTCUTS = (
    "running the step's command directly with the shell tool",
    "editing, deleting or hand-writing the state file",
    "reading or querying the warehouse's own storage to guess what loaded",
)


def now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def die(message: str, code: int) -> "None":
    print(message, file=sys.stderr)
    raise SystemExit(code)


def refusal(reason: str, blocked: dict | None = None) -> "None":
    """Refuse, and say what to do instead. Never just say no."""
    lines = [
        "",
        "REFUSED — nothing was run.",
        "",
        f"  {reason}",
        "",
        "Do this instead:",
        "  1. Report the blocked step to the user: its number, its command, when it",
        "     ran, and the exact output or error it produced.",
        "  2. Say plainly that its outcome is UNKNOWN: the load may already be",
        "     committed, and there is no way to check from here.",
        "  3. End the turn. Do not run the step, do not pick a different step, and do",
        "     not work around this.",
        "",
        "This refusal exists because re-running a committed step duplicates rows. It",
        "is defeated by:",
    ]
    lines += [f"  - {s}" for s in SHORTCUTS]
    lines += [
        "",
        "That the failure's cause is gone (the connection reset cleared, the outage",
        "ended, the credential was refreshed) is NOT evidence that the step did not",
        "take effect. Only the warehouse knows, and it cannot be asked.",
        "",
    ]
    if blocked:
        lines += [
            "Blocked step:",
            f"  step {blocked['n']}: {blocked['command']}",
            f"  status   {blocked['status']}   (last written {blocked.get('updated_at', 'unknown')})",
        ]
        if blocked.get("started_at"):
            lines.append(f"  started  {blocked['started_at']}")
        if blocked.get("exit_code") is not None:
            lines.append(f"  exit     {blocked['exit_code']}")
        lines.append("")
    die("\n".join(lines), EXIT_REFUSED)


# ---------------------------------------------------------------- runbook

class RunbookError(Exception):
    pass


def read_runbook(path: Path) -> list[dict]:
    """Parse `N. <shell command>` items. Anything ambiguous is an error, not a guess."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise RunbookError(f"cannot read runbook {path}: {exc}") from exc

    steps: dict[int, dict] = {}
    order: list[int] = []
    current: int | None = None
    base_indent: str | None = None

    for lineno, line in enumerate(text.splitlines(), 1):
        match = STEP_RE.match(line)
        if match:
            indent, num, command = match.groups()
            if base_indent is None:
                base_indent = indent
            elif len(indent) != len(base_indent):
                # A nested ordered list, not a top-level step.
                continue
            n = int(num)
            if n in steps:
                raise RunbookError(
                    f"{path}:{lineno}: step number {n} appears more than once, so the "
                    f"step order is ambiguous. Read the runbook yourself and report this."
                )
            steps[n] = {"n": n, "command": command, "line": lineno}
            order.append(n)
            current = n
            continue

        if current is not None and line.strip() and line.startswith((" ", "\t")):
            # Text belonging to the previous step: this step is not a single
            # shell command, and guessing how to join it could run the wrong load.
            raise RunbookError(
                f"{path}:{lineno}: step {current} continues onto extra lines, so its "
                f"command is not a single shell command. Read the runbook yourself and "
                f"report this instead of guessing."
            )
        current = None

    if not steps:
        raise RunbookError(f"{path}: no numbered steps found (expected lines like `1. <command>`).")
    return [steps[n] for n in sorted(steps)]


# ---------------------------------------------------------------- state

def state_path(runbook: Path) -> Path:
    return runbook.parent / STATE_DIRNAME / f"{runbook.stem}.json"


def save(path: Path, state: dict) -> None:
    """Atomic write: a cut-off session can never leave a half-written state file."""
    state["updated_at"] = now()
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=str(path.parent), prefix=".tmp-", suffix=".json")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(state, handle, indent=2, sort_keys=False)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise


def load_state(path: Path, runbook: Path, steps: list[dict]) -> dict:
    existing = path.exists()
    if not existing:
        state = {
            "schema": SCHEMA,
            "runbook": str(runbook),
            "runbook_sha256": hashlib.sha256(runbook.read_bytes()).hexdigest(),
            "workspace": str(runbook.parent),
            "created_at": now(),
            "updated_at": now(),
            "notes": [],
            "steps": [],
        }
    else:
        try:
            state = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            die(
                f"\nERROR — state file {path} is unreadable ({exc}).\n"
                "Nothing was run. Report this to the user; do not delete or rewrite the "
                "state file, because it is the only record of which steps already loaded.",
                EXIT_USAGE,
            )

        if state.get("schema") != SCHEMA:
            die(f"\nERROR — unknown state schema {state.get('schema')!r} in {path}. Nothing was run.",
                EXIT_USAGE)

    recorded = {s["n"]: s for s in state.get("steps", [])}
    for step in steps:
        if step["n"] in recorded:
            recorded[step["n"]]["command"] = step["command"]
        else:
            recorded[step["n"]] = {"n": step["n"], "command": step["command"], "status": STATUS_TODO}
            # Only meaningful on an existing state: a step that was not there before.
            if existing:
                state.setdefault("notes", []).append(
                    f"step {step['n']} appeared in the runbook with no record; treated as not started"
                )
    state["steps"] = [recorded[step["n"]] for step in steps]
    return state


def blocked_step(state: dict) -> dict | None:
    for step in state["steps"]:
        if step.get("status") in (STATUS_PENDING, STATUS_FAILED):
            return step
    return None


def check_runbook_unchanged(state: dict) -> None:
    if "runbook_sha256" not in state:
        return
    current = hashlib.sha256(Path(state["runbook"]).read_bytes()).hexdigest()
    if current != state["runbook_sha256"]:
        print(
            f"\nWARNING: {state['runbook']} has changed since this run started.\n"
            "Runbooks are never edited. Steps recorded as done were run from the old\n"
            "text. Report this to the user before running anything else.\n",
            file=sys.stderr,
        )


# ---------------------------------------------------------------- commands

def cmd_status(args) -> int:
    runbook = Path(args.runbook).resolve()
    steps = read_runbook(runbook)
    spath = state_path(runbook)
    state = load_state(spath, runbook, steps)
    check_runbook_unchanged(state)

    print(f"Runbook : {runbook}")
    print(f"State   : {spath}" + ("" if spath.exists() else "   (not created yet)"))
    print(f"Updated : {state.get('updated_at')}")
    print()

    for step in state["steps"]:
        status = step.get("status", STATUS_TODO)
        if status == STATUS_DONE:
            detail = f"finished {step.get('finished_at', '?')} (exit 0)"
        elif status == STATUS_FAILED:
            detail = (f"ran {step.get('started_at', '?')}, exited {step.get('exit_code')} "
                      f"— OUTCOME UNKNOWN")
        elif status == STATUS_PENDING:
            detail = (f"started {step.get('started_at', '?')}, no result recorded "
                      f"— OUTCOME UNKNOWN")
        else:
            detail = "not started"
        print(f"  step {step['n']:>3}  {status:<7}  {step['command']}")
        print(f"              {detail}")

    done = sum(1 for s in state["steps"] if s.get("status") == STATUS_DONE)
    print(f"\n{done} of {len(state['steps'])} steps done.")
    block = blocked_step(state)
    if block:
        print(f"\nBLOCKED at step {block['n']} ({block['status']}). Do not run step "
              f"{block['n']} or anything after it.")
        print("Report it to the user and end the turn.")
    else:
        nxt = next((s for s in state["steps"] if s.get("status") == STATUS_TODO), None)
        if nxt:
            print(f"Next step is {nxt['n']}. Run it with: "
                  f"backfill_step.py run {nxt['n']}")
        else:
            print("Every step in the runbook is done.")
    return EXIT_OK


def cmd_run(args) -> int:
    runbook = Path(args.runbook).resolve()
    steps = read_runbook(runbook)

    by_n = {s["n"]: s for s in steps}
    target = args.step
    if target not in by_n:
        die(f"\nERROR — step {target} is not in {runbook} (steps: "
            f"{', '.join(str(s['n']) for s in steps)}). Nothing was run.", EXIT_USAGE)

    spath = state_path(runbook)
    state = load_state(spath, runbook, steps)
    check_runbook_unchanged(state)

    # 1. Never run past a step whose outcome is unknown.
    block = blocked_step(state)
    if block:
        refusal(
            f"step {block['n']} has no known outcome, so step {target} would run with "
            f"an unknown gap in the runbook behind it.",
            block,
        )

    # 2. Never re-run a step that already loaded.
    done = [s for s in state["steps"] if s.get("status") == STATUS_DONE]
    if any(s["n"] == target for s in done):
        refusal(
            f"step {target} is already recorded done. Running it again would load its "
            f"batch a second time and duplicate rows.",
            next(s for s in done if s["n"] == target),
        )

    # 3. Steps run in order.
    for step in state["steps"]:
        if step["n"] < target and step.get("status") != STATUS_DONE:
            refusal(
                f"step {target} cannot run before step {step['n']}, which is "
                f"{'not done' if step.get('status') == STATUS_TODO else 'blocked'}.",
                step,
            )

    # 4. Record the step as pending BEFORE running it, so a cut-off is visible.
    record = next(s for s in state["steps"] if s["n"] == target)
    record.update({
        "status": STATUS_PENDING,
        "started_at": now(),
        "updated_at": now(),
        "finished_at": None,
        "exit_code": None,
        "outcome": "unknown",
    })
    save(spath, state)

    command = record["command"]
    print(f"Running step {target} of {len(steps)} from {runbook.parent}:")
    print(f"  $ {command}\n")
    sys.stdout.flush()  # keep our banner ahead of the child's output

    interrupted: list[int] = []

    def on_signal(signum, _frame):
        interrupted.append(signum)

    previous = {}
    for sig in (signal.SIGINT, signal.SIGTERM):
        previous[sig] = signal.signal(sig, on_signal)

    started = time.monotonic()
    try:
        completed = subprocess.run(command, shell=True, cwd=str(runbook.parent))
        exit_code = completed.returncode
    except KeyboardInterrupt:
        exit_code = 130
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)

    duration = round(time.monotonic() - started, 1)
    record["finished_at"] = now()
    record["duration_s"] = duration

    # 5. Done ONLY on a clean exit 0.
    if exit_code == 0 and not interrupted:
        record["status"] = STATUS_DONE
        record["exit_code"] = 0
        record["outcome"] = "committed"
        record["updated_at"] = now()
        save(spath, state)
        print(f"\nStep {target} exited 0 in {duration}s. Recorded done.")
        remaining = [s for s in state["steps"] if s["n"] > target and s.get("status") != STATUS_DONE]
        if remaining:
            print(f"Next step is {remaining[0]['n']}.")
        else:
            print("That was the last step in the runbook.")
        return EXIT_OK

    # 6. Anything else: the load may already be committed. Block, do not guess.
    record["status"] = STATUS_FAILED
    record["exit_code"] = exit_code
    record["outcome"] = "unknown"
    record["updated_at"] = now()
    save(spath, state)

    reason = (
        f"the session was interrupted (signal {interrupted[0]}) while step {target} was running"
        if interrupted else f"step {target} exited {exit_code}"
    )
    refusal(
        f"{reason} after {duration}s. The load may already be committed — a command can "
        f"fail after the warehouse accepted the batch — and there is no way to check.",
        record,
    )


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run one backfill runbook step, exactly once.",
    )
    parser.add_argument("--runbook", default="runbook.md",
                        help="path to the runbook (default: runbook.md in the workspace root)")
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("status", help="print the recorded state of every step")

    runner = sub.add_parser("run", help="run one step, recording it as pending first")
    runner.add_argument("step", type=int, help="the step number to run")

    args = parser.parse_args()
    try:
        return cmd_status(args) if args.command == "status" else cmd_run(args)
    except RunbookError as exc:
        die(f"\nERROR — {exc}\nNothing was run.", EXIT_USAGE)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)