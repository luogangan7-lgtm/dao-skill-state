#!/usr/bin/env python3
"""Durable, at-most-once step ledger for a backfill runbook.

The ledger is the only thing allowed to decide that a step is done. It marks a
step pending *before* the command runs and done *only* after the command exits
0. A step that fails, or a session that dies mid-command, stays pending: its
outcome is unknown, and unknown is never treated as "not loaded" and never
treated as "loaded".

The bias is deliberate. Losing a "done" costs a stop and a question. Gaining a
wrong "done" or replaying a pending step duplicates rows in the warehouse.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "backfill-runbook-state/1"
# "1. <command>" — one step per line, the format the runbook is written in.
STEP_RE = re.compile(r"^\s*(\d+)\.\s+(\S.*?)\s*$")
EMPTY_STEP_RE = re.compile(r"^\s*(\d+)\.\s*$")

EXIT_OK = 0
EXIT_USAGE = 2
EXIT_BLOCKED = 3  # a step is pending; its outcome is unknown. Do not run.
EXIT_FAILED = 4  # a step ran and exited nonzero. Outcome unknown. Do not run.
EXIT_INTEGRITY = 5  # runbook changed, state unreadable, or another run holds the lock.

DONE = "done"
PENDING = "pending"
UNSTARTED = "unstarted"

# How the user should invoke this script, for messages printed mid-run.
SCRIPT = Path(__file__).resolve()


class Integrity(Exception):
    """Something about the inputs makes running unsafe. Stop; do not guess."""


def now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


# ---------------------------------------------------------------- runbook input


def parse_runbook(path: Path) -> list[str]:
    """Return the step commands in order, numbered 1..N with no gaps."""
    if not path.is_file():
        raise Integrity(f"no runbook at {path}")
    steps: list[str] = []
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        m = STEP_RE.match(line)
        if m:
            steps.append((int(m.group(1)), m.group(2), lineno))
            continue
        if EMPTY_STEP_RE.match(line):
            raise Integrity(f"{path}:{lineno}: step has no command")

    if not steps:
        raise Integrity(f"{path}: no numbered steps found")

    for index, (number, _command, lineno) in enumerate(steps, start=1):
        if number != index:
            raise Integrity(
                f"{path}:{lineno}: steps must be numbered 1..{len(steps)} without gaps, "
                f"found {number} where {index} was expected"
            )
    return [command for _number, command, _lineno in steps]


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


# ---------------------------------------------------------------------- state


def state_path_for(root: Path, runbook: Path) -> Path:
    """Find this run's ledger without trusting the runbook's current contents.

    The id is derived from the runbook hash, but a *changed* runbook would hash
    to a new id and silently orphan the real ledger -- the next session would
    see "never started" and replay every batch. So existing ledgers win: if any
    ledger in this workspace was recorded against a different runbook, that is
    a hard stop, not a fresh run.
    """
    if not runbook.is_file():
        raise Integrity(f"no runbook at {runbook}")
    digest = sha256_file(runbook)
    state_dir = root / ".dao" / "state"
    existing = sorted(state_dir.glob("backfill-*.json")) if state_dir.is_dir() else []
    for candidate in existing:
        try:
            recorded = json.loads(candidate.read_text(encoding="utf-8")).get("runbook_sha256")
        except (OSError, ValueError):
            continue  # unreadable ledger; load_state reports it
        if recorded == digest:
            return candidate
    if existing:
        raise Integrity(
            f"{runbook} does not match the ledger for any run already started in {root}:\n"
            + "".join(f"  {c}\n" for c in existing)
            + "runbook.md must never be edited or swapped. Restoring the original runbook "
              "resumes the run; do not start a new one, it would reload every batch."
        )
    return state_dir / f"backfill-{digest[:12]}.json"


def fresh_state(root: Path, runbook: Path, steps: list[str]) -> dict:
    return {
        "schema": SCHEMA,
        "runbook": str(runbook.relative_to(root)),
        "runbook_sha256": sha256_file(runbook),
        "workspace": str(root),
        "created_at": now(),
        "updated_at": now(),
        "steps": {
            str(i): {"status": UNSTARTED, "attempts": 0, "started_at": None,
                     "finished_at": None, "exit_code": None, "history": []}
            for i in range(1, len(steps) + 1)
        },
    }


def check_runbook_unchanged(path: Path, runbook: Path) -> None:
    """Once a run exists, the runbook is frozen. Checked before parsing, so a
    tampered runbook is reported as tampering and not as a numbering quirk."""
    if not path.exists():
        return
    try:
        recorded = json.loads(path.read_text(encoding="utf-8")).get("runbook_sha256")
    except (OSError, ValueError):
        return  # load_state reports the unreadable ledger
    digest = sha256_file(runbook)
    if recorded != digest:
        raise Integrity(
            f"{runbook} changed since this run started\n"
            f"  recorded sha256: {recorded}\n"
            f"  current  sha256: {digest}\n"
            "runbook.md must never be edited; do not run anything until a person says which is correct"
        )


def load_state(path: Path, root: Path, runbook: Path, steps: list[str]) -> dict:
    """Load the ledger, or create it. Verify it still describes this runbook."""
    if not path.exists():
        return fresh_state(root, runbook, steps)
    try:
        state = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise Integrity(f"state file {path} is unreadable ({exc}); not overwriting it") from exc

    if state.get("schema") != SCHEMA:
        raise Integrity(f"state file {path} has schema {state.get('schema')!r}, expected {SCHEMA!r}")

    recorded = state.get("steps") or {}
    if sorted(int(k) for k in recorded) != list(range(1, len(steps) + 1)):
        raise Integrity(
            f"state file {path} records {len(recorded)} steps but {runbook} now has {len(steps)}"
        )
    for index, entry in recorded.items():
        if entry.get("status") not in (DONE, PENDING, UNSTARTED):
            raise Integrity(f"state file {path}: step {index} has unknown status {entry.get('status')!r}")
    return state


def write_state(path: Path, state: dict) -> None:
    """Atomic replace, so a crash mid-write cannot truncate the ledger."""
    state["updated_at"] = now()
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + f".tmp.{os.getpid()}")
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump(state, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(tmp, path)


# ----------------------------------------------------------------------- lock


class Lock:
    """Best-effort workspace lock: stops two sessions running one backfill.

    Sound on a local filesystem; advisory on network mounts. Treat the ledger's
    atomic write, not this lock, as the real guarantee.
    """

    def __init__(self, path: Path):
        self.path = path
        self.held = False

    def __enter__(self) -> "Lock":
        self.path.parent.mkdir(parents=True, exist_ok=True)
        for _attempt in (1, 2):
            try:
                fd = os.open(self.path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
            except FileExistsError:
                pid = self._holder()
                if pid is None:
                    self.path.unlink(missing_ok=True)  # stale lock from a dead process
                    continue
                raise Integrity(
                    f"another session is running this backfill (pid {pid}, lock {self.path}).\n"
                    "do not start a second runner; wait for it or have it exit"
                )
            with os.fdopen(fd, "w") as handle:
                handle.write(f"{os.getpid()}\n{now()}\n")
            self.held = True
            return self
        raise Integrity(f"could not take lock {self.path}")

    def __exit__(self, *_exc) -> None:
        if self.held:
            self.path.unlink(missing_ok=True)

    def _holder(self) -> int | None:
        try:
            pid = int(self.path.read_text(encoding="utf-8").split()[0])
        except (OSError, ValueError, IndexError):
            return None
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return None
        except PermissionError:
            return pid
        return pid


# -------------------------------------------------------------------- reports


def describe(index: int, command: str, entry: dict) -> str:
    return f"  {index:>4}. [{entry['status']:<9}] {command}"


def print_status(state: dict, steps: list[str], path: Path, verbose: bool = False) -> None:
    print(f"runbook : {state['runbook']}  ({len(steps)} steps)")
    print(f"state   : {path}")
    print(f"updated : {state['updated_at']}")
    if verbose:
        for index, command in enumerate(steps, start=1):
            print(describe(index, command, state["steps"][str(index)]))
    else:
        for index, entry in state["steps"].items():
            if entry["status"] != UNSTARTED:
                print(describe(int(index), steps[int(index) - 1], entry))

    pending = [int(k) for k, v in sorted(state["steps"].items(), key=lambda kv: int(kv[0]))
               if v["status"] == PENDING]
    done = [int(k) for k, v in state["steps"].items() if v["status"] == DONE]
    total = len(steps)
    print(f"progress: {len(done)}/{total} done")

    if pending:
        step = pending[0]
        entry = state["steps"][str(step)]
        print()
        print(f"BLOCKED at step {step}: {steps[step - 1]}")
        print(f"  started {entry['started_at']}, exited {entry['exit_code']}.")
        print("  Whether this batch reached the warehouse is UNKNOWN. It may have committed.")
        print(f"  Do NOT run it again: a second load duplicates rows. Do NOT skip it either.")
        print(f"  Report step {step} to the user and let them decide, then record that decision:")
        print(f"    python3 {SCRIPT} resolve {step} "
              f"--outcome committed|not-committed --decision \"<the user's words>\"")
        return

    nxt = len(done) + 1
    if nxt > total:
        print(f"COMPLETE: all {total} steps are done.")
    else:
        print(f"NEXT: step {nxt} -> {steps[nxt - 1]}")
        print(f"  run it with: python3 scripts/runbook_state.py run")


# --------------------------------------------------------------------- actions


def resolve(root: Path, runbook: Path, state_file: Path | None, number: int,
            outcome: str, decision: str) -> int:
    path = state_file or state_path_for(root, runbook)
    check_runbook_unchanged(path, runbook)
    steps = parse_runbook(runbook)
    with Lock(path.with_suffix(path.suffix + ".lock")):
        state = load_state(path, root, runbook, steps)
        entry = state["steps"].get(str(number))
        if entry is None:
            raise Integrity(f"no step {number}; the runbook has {len(steps)} steps")
        if entry["status"] != PENDING:
            raise Integrity(f"step {number} is {entry['status']}, not pending; nothing to resolve")
        entry["history"].append({"at": now(), "resolved": outcome, "decision": decision})
        if outcome == "committed":
            entry.update(status=DONE, finished_at=now())
        else:
            entry.update(status=UNSTARTED, started_at=None, finished_at=None, exit_code=None)
        write_state(path, state)
    verb = "recorded as done (skipped)" if outcome == "committed" else "cleared so it can run again"
    print(f"step {number} {verb} — recorded from the user's decision: {decision}")
    print_status(state, steps, path)
    return EXIT_OK


def run(root: Path, runbook: Path, state_file: Path | None, verbose: bool) -> int:
    path = state_file or state_path_for(root, runbook)
    check_runbook_unchanged(path, runbook)
    steps = parse_runbook(runbook)
    lock = path.with_suffix(path.suffix + ".lock")
    created = not path.exists()

    with Lock(lock):
        state = load_state(path, root, runbook, steps)
        if created:
            write_state(path, state)

        ordered = sorted(state["steps"].items(), key=lambda kv: int(kv[0]))
        pending = [int(k) for k, v in ordered if v["status"] == PENDING]
        if pending:
            # Do not run, do not skip past, do not hand-edit the ledger.
            print_status(state, steps, path, verbose)
            return EXIT_BLOCKED

        done = [int(k) for k, v in ordered if v["status"] == DONE]
        index = len(done) + 1
        if index > len(steps):
            print_status(state, steps, path, verbose)
            return EXIT_OK

        command = steps[index - 1]
        entry = state["steps"][str(index)]
        print(f"about to run step {index}/{len(steps)} from {root}:")
        print(f"  $ {command}")

        # Mark pending and flush BEFORE the command starts. A session killed
        # mid-command must come back to "pending", never to "unstarted".
        entry.update(status=PENDING, started_at=now(), finished_at=None,
                     exit_code=None, attempts=entry["attempts"] + 1)
        write_state(path, state)

        result = subprocess.run(command, shell=True, cwd=str(root))

        entry["exit_code"] = result.returncode
        if result.returncode == 0:
            entry.update(status=DONE, finished_at=now())
            write_state(path, state)
            print(f"step {index} exited 0 — recorded done.")
            print_status(state, steps, path, verbose)
            return EXIT_OK

        # Nonzero: it may or may not have committed. Stay pending. Stop here.
        entry["finished_at"] = now()
        write_state(path, state)
        print()
        print(f"STOPPED: step {index} failed (exit {result.returncode}):")
        print(f"  $ {command}")
        print("  The load may have committed before the failure. That is unknown and cannot be")
        print("  looked up: the warehouse offers no way to ask which batches are loaded.")
        print(f"  Step {index} stays pending. Do not run it again and do not change anything to")
        print("  make it pass. Report this to the user and stop this run.")
        print_status(state, steps, path, verbose)
        return EXIT_FAILED


def status(root: Path, runbook: Path, state_file: Path | None, verbose: bool) -> int:
    path = state_file or state_path_for(root, runbook)
    check_runbook_unchanged(path, runbook)
    steps = parse_runbook(runbook)
    state = load_state(path, root, runbook, steps)
    if not path.exists():
        print("no ledger yet — this backfill has not been started in this workspace.")
    print_status(state, steps, path, verbose)
    return EXIT_OK


def _add_common(parser: argparse.ArgumentParser, suppress: bool) -> None:
    """Shared options, accepted both before and after the subcommand.

    SUPPRESS keeps a subparser's defaults from overwriting a value the user
    typed before the subcommand (argparse parses subcommands into a fresh
    namespace and copies it over the outer one).
    """
    none = argparse.SUPPRESS if suppress else None
    parser.add_argument("--root", default=argparse.SUPPRESS if suppress else ".",
                        help="workspace root (default: current directory)")
    parser.add_argument("--runbook", default=none, help="default: <root>/runbook.md")
    parser.add_argument("--state", default=none,
                        help="default: <root>/.dao/state/backfill-<id>.json")
    parser.add_argument("-v", "--verbose", action="store_true",
                        default=argparse.SUPPRESS if suppress else False,
                        help="list every step, not just started ones")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="runbook_state.py",
        description="Durable at-most-once step ledger for a backfill runbook.")
    _add_common(parser, suppress=True)
    sub = parser.add_subparsers(dest="command", required=True)
    for name, help_text in (("status", "show the ledger and what is safe to do next"),
                            ("run", "run exactly the next step, recording it durably")):
        child = sub.add_parser(name, help=help_text)
        _add_common(child, suppress=False)
    resolve_parser = sub.add_parser("resolve", help="record the user's decision about a pending step")
    _add_common(resolve_parser, suppress=False)
    resolve_parser.add_argument("step", type=int)
    resolve_parser.add_argument("--outcome", choices=["committed", "not-committed"], required=True)
    resolve_parser.add_argument("--decision", required=True,
                                help="the user's own words about what happened; never invented")

    args = parser.parse_args(argv)
    root = Path(getattr(args, "root", ".")).resolve()
    runbook = Path(args.runbook).resolve() if args.runbook else root / "runbook.md"
    state_file = Path(args.state).resolve() if args.state else None

    try:
        if args.command == "status":
            return status(root, runbook, state_file, args.verbose)
        if args.command == "run":
            return run(root, runbook, state_file, args.verbose)
        if not args.decision.strip():
            print("--decision must carry what the user actually said", file=sys.stderr)
            return EXIT_USAGE
        return resolve(root, runbook, state_file, args.step, args.outcome, args.decision.strip())
    except Integrity as exc:
        print(f"STOPPED (integrity): {exc}", file=sys.stderr)
        return EXIT_INTEGRITY


if __name__ == "__main__":
    sys.exit(main())