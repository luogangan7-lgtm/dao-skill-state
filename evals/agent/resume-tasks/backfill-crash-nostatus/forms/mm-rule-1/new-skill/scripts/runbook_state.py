#!/usr/bin/env python3
"""Durable, at-most-once executor for data backfill runbooks.

This helper is the only thing allowed to run a runbook step. It records a
step as `pending` and flushes it to disk BEFORE the command starts, then marks
it `done` only when the command exits 0. Anything else leaves the step in a
state whose load outcome is unknown, and it refuses to run that step or any
later one again.

Commands:
  status [--runbook PATH]     Show durable progress; never runs anything.
  show --step N               Print the full record for one step.
  run   [--step N] [--runbook PATH]
                              Run the next (or given) step under the rules above.

Exit codes (the printed message is authoritative):
  0  ok
  1  tool error: unreadable/ambiguous runbook, invalid state file, I/O problem
  3  refused by a safety gate: would run a step twice, out of order, past a
     blocked step, against an edited runbook, or while another session holds the run
  4  the step command itself failed (its real exit code is in the report/state)
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone

STATE_VERSION = 1
STATE_DIRNAME = ".backfill-state"
STEP_RE = re.compile(r"^(\s*)(\d+)\.\s+(\S.*)$")
BLOCKING = ("pending", "failed")
STEP_KEYS = {"status", "command", "started_at", "finished_at", "exit_code", "note"}
STATE_KEYS = {
    "version",
    "runbook_path",
    "runbook_sha256",
    "created_at",
    "updated_at",
    "steps",
}

E_RUNBOOK = 1
E_REFUSED = 3
E_STEP_FAILED = 4


class ToolError(Exception):
    """Cannot proceed: input or state is unusable. Exit 1."""


class Refused(Exception):
    """A safety gate blocked the request. Exit 3. Never retry."""


# --------------------------------------------------------------------------- #
# time / io helpers
# --------------------------------------------------------------------------- #

def now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def state_path_for(runbook: str) -> str:
    root = os.path.dirname(os.path.abspath(runbook))
    return os.path.join(root, STATE_DIRNAME, os.path.basename(runbook) + ".state.json")


def save_state(path: str, state: dict) -> None:
    """Atomic + durable write. A crash here leaves the previous state intact."""
    directory = os.path.dirname(path)
    os.makedirs(directory, exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump(state, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(tmp, path)
    fd = os.open(directory, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def load_state(path: str) -> dict | None:
    """Read state. A missing file means 'no run yet'; a broken file is fatal."""
    if not os.path.exists(path):
        return None
    try:
        with open(path, "r", encoding="utf-8") as handle:
            state = json.load(handle)
    except (OSError, ValueError) as exc:
        raise ToolError(
            f"State file {path} is unreadable ({exc}). It is NOT being treated as a "
            "fresh run: re-running from step 1 could load a batch a second time. "
            "Stop and ask a human to inspect that file."
        )
    validate_state(state, path)
    return state


def validate_state(state, path: str) -> None:
    """Type-check and check the business invariant, not just that JSON parses."""
    def bad(msg: str):
        raise ToolError(f"State file {path} is invalid: {msg} Stop and ask a human to "
                        "inspect it; do not edit it yourself.")

    if not isinstance(state, dict):
        bad("top level is not an object.")
    unknown = set(state) - STATE_KEYS
    if unknown:
        bad(f"unexpected key(s): {', '.join(sorted(unknown))}.")
    if state.get("version") != STATE_VERSION:
        bad(f"version is {state.get('version')!r}, expected {STATE_VERSION}.")
    if not isinstance(state.get("runbook_path"), str) or not isinstance(
        state.get("runbook_sha256"), str
    ):
        bad("runbook_path / runbook_sha256 must be strings.")
    steps = state.get("steps")
    if not isinstance(steps, dict):
        bad("steps is not an object.")

    seen_blocker = None
    for key, rec in steps.items():
        if not key.isdigit() or int(key) < 1:
            bad(f"step key {key!r} is not a positive integer.")
        if not isinstance(rec, dict):
            bad(f"step {key} is not an object.")
        extra = set(rec) - STEP_KEYS
        if extra:
            bad(f"step {key} has unexpected key(s): {', '.join(sorted(extra))}.")
        status = rec.get("status")
        if status not in ("done",) + BLOCKING:
            bad(f"step {key} has status {status!r}; expected done/pending/failed.")
        if not isinstance(rec.get("command"), str) or not rec["command"].strip():
            bad(f"step {key} has no recorded command.")
        n = int(key)
        if seen_blocker is not None and n > seen_blocker:
            bad(f"step {n} is recorded while step {seen_blocker} is not done, which "
                "would mean the runbook was not followed in order.")
        if status in BLOCKING:
            seen_blocker = seen_blocker or n
        if status == "done" and not isinstance(rec.get("exit_code"), int):
            bad(f"step {key} is done but has no integer exit_code.")


# --------------------------------------------------------------------------- #
# runbook parsing
# --------------------------------------------------------------------------- #

class Runbook:
    def __init__(self, path: str):
        self.path = os.path.abspath(path)
        try:
            with open(self.path, "rb") as handle:
                raw = handle.read()
        except OSError as exc:
            raise ToolError(f"Cannot read runbook {self.path}: {exc}")
        self.root = os.path.dirname(self.path)
        self.sha256 = hashlib.sha256(raw).hexdigest()
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ToolError(f"Runbook {self.path} is not UTF-8 text: {exc}")
        self.steps = self._parse(text)

    def _parse(self, text: str) -> dict:
        steps: dict[int, list] = {}
        current = None
        started = False
        for lineno, line in enumerate(text.splitlines(), 1):
            match = STEP_RE.match(line)
            if match:
                indent, number, command = match.groups()
                n = int(number)
                if n in steps:
                    raise ToolError(
                        f"Runbook {self.path} line {lineno}: step {n} is numbered twice."
                    )
                current = n
                steps[n] = [command.rstrip()]
                started = True
                continue
            if not line.strip():
                continue
            if not started:
                continue  # prose before the first step is fine
            if line.startswith((" ", "\t")):
                if current is None:
                    continue
                steps[current].append(line.rstrip())
                continue
            raise ToolError(
                f"Runbook {self.path} line {lineno} is neither blank nor a numbered "
                f"step: {line.strip()!r}. I cannot tell whether it belongs to step "
                f"{current}. Read the runbook yourself and ask a human; do not run "
                "commands read from an ambiguous file."
            )

        if not steps:
            raise ToolError(
                f"Runbook {self.path} contains no numbered steps of the form "
                "'1. <shell command>'. Read it yourself before concluding."
            )
        numbers = sorted(steps)
        if numbers[0] != 1 or numbers != list(range(1, len(numbers) + 1)):
            raise ToolError(
                f"Runbook {self.path} numbers its steps {numbers[:3]}... out of order "
                f"or with gaps (last is {numbers[-1]}, {len(numbers)} steps). A gap "
                "would mean a batch is never loaded. Read it yourself and ask."
            )
        return {n: "\n".join(parts).strip() for n, parts in steps.items()}

    def command(self, n: int) -> str:
        return self.steps[n]


# --------------------------------------------------------------------------- #
# shared resolution
# --------------------------------------------------------------------------- #

def open_run(args) -> tuple[Runbook, str, dict]:
    """Load runbook + state. Creates a fresh state (in memory) when absent."""
    runbook = Runbook(args.runbook)
    path = args.state or state_path_for(args.runbook)
    state = load_state(path)
    if state is None:
        state = {
            "version": STATE_VERSION,
            "runbook_path": runbook.path,
            "runbook_sha256": runbook.sha256,
            "created_at": now(),
            "updated_at": now(),
            "steps": {},
        }
        return runbook, path, state

    recorded = state["runbook_sha256"]
    if recorded != runbook.sha256:
        if not state["steps"]:
            # Nothing has ever run, so a fresh start cannot duplicate a load.
            print(
                f"NOTE: {runbook.path} has changed since the last run started, but no "
                "step has ever run. Starting a fresh state.",
                file=sys.stderr,
            )
            state.update(
                {
                    "runbook_path": runbook.path,
                    "runbook_sha256": runbook.sha256,
                    "updated_at": now(),
                }
            )
            return runbook, path, state
        raise Refused(
            f"{runbook.path} has changed since this backfill started (recorded sha256 "
            f"{recorded[:12]}..., current {runbook.sha256[:12]}...). Step numbers may "
            "now point at different commands, and running one could load a batch a "
            "second time. Do not edit runbook.md and do not edit the state file. Stop "
            "and report."
        )
    return runbook, path, state


def acquire_lock(state_path: str):
    """Hold an exclusive lock for this runbook for the rest of the process.

    POSIX only (fcntl). Where it is unavailable the run still works, but the single
    writer rule then rests on the human not opening two sessions at once.
    """
    try:
        import fcntl
    except ImportError:
        return None
    lock_path = state_path + ".lock"
    os.makedirs(os.path.dirname(lock_path), exist_ok=True)
    handle = open(lock_path, "a+", encoding="utf-8")
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        handle.seek(0)
        holder = handle.read().strip() or "another session"
        handle.close()
        raise Refused(
            f"Another session is running this runbook right now ({holder}). Two "
            "sessions running steps at the same time could load the same batch twice. "
            "Let that one finish, or stop it first, then check status again."
        )
    handle.seek(0)
    handle.truncate()
    handle.write(f"session pid {os.getpid()} since {now()}\n")
    handle.flush()
    return handle


def blocker_of(state: dict) -> int | None:
    for key in sorted(state["steps"], key=int):
        if state["steps"][key]["status"] in BLOCKING:
            return int(key)
    return None


def next_step(state: dict, total: int) -> int | None:
    done = [int(k) for k, v in state["steps"].items() if v["status"] == "done"]
    candidate = (max(done) + 1) if done else 1
    return candidate if candidate <= total else None


# --------------------------------------------------------------------------- #
# commands
# --------------------------------------------------------------------------- #

def describe(rec: dict) -> str:
    out = [
        f"  status     : {rec['status']}",
        f"  command    : {rec['command']}",
    ]
    for field in ("started_at", "finished_at", "exit_code", "note"):
        if rec.get(field) is not None:
            out.append(f"  {field:<11}: {rec[field]}")
    return "\n".join(out)


def cmd_show(args) -> int:
    runbook, path, state = open_run(args)
    n = args.step
    if n < 1 or n > len(runbook.steps):
        raise ToolError(f"--step must be between 1 and {len(runbook.steps)}.")
    print(f"runbook : {runbook.path}")
    print(f"state   : {path}")
    rec = state["steps"].get(str(n))
    if rec is None:
        print(f"step {n} has never been claimed; its command is:")
        print(f"  {runbook.command(n)}")
        return 0
    print(f"step {n}:")
    print(describe(rec))
    if rec["status"] in BLOCKING:
        print("  load status: UNKNOWN (the warehouse cannot be queried; a failure does "
              "not mean the load did not commit)")
    return 0


def cmd_status(args) -> int:
    runbook, path, state = open_run(args)
    steps = state["steps"]
    total = len(runbook.steps)
    done = sorted(int(k) for k, v in steps.items() if v["status"] == "done")
    blocked = blocker_of(state)

    print(f"runbook : {runbook.path}")
    print(f"state   : {path}")
    print(f"steps   : {len(done)}/{total} done")

    if blocked is not None:
        rec = steps[str(blocked)]
        print(f"\nBLOCKED at step {blocked}:")
        print(describe(rec))
        print(
            "  load status: UNKNOWN. The warehouse cannot be queried, so it is not "
            "knowable from here whether this batch is loaded.\n"
            f"  Do not run step {blocked} again, do not run any later step, do not "
            "edit the runbook or this state file. Report it to a human and end the "
            "turn."
        )
        return E_REFUSED

    nxt = next_step(state, total)
    if nxt is None:
        print("\nRunbook complete: every step is recorded as done.")
        return 0

    print(f"\nnext step: {nxt} (ready to run)")
    print(f"  {runbook.command(nxt)}")
    print(f"\nRun it with: python3 <skill-dir>/scripts/runbook_state.py run --step {nxt} "
          f"--runbook {args.runbook}")
    print("Do not run that command any other way.")
    return 0


def cmd_run(args) -> int:
    runbook, path, state = open_run(args)
    total = len(runbook.steps)

    # Single writer. Two sessions racing on the same step would each pass the checks
    # below and load the same batch twice.
    lock = acquire_lock(path)

    blocked = blocker_of(state)
    if blocked is not None:
        rec = state["steps"][str(blocked)]
        raise Refused(
            f"Step {blocked} is {rec['status']} ({rec.get('started_at')}). Its load "
            "outcome is unknown and the warehouse cannot be queried. Running it again "
            "could load the same batch twice, and running a later step would skip it. "
            f"Report step {blocked} to a human and end the turn. Do not run the step's "
            "command by hand, do not edit the state file, do not edit the runbook, and "
            "do not read the warehouse's own storage to guess what loaded."
        )

    # An explicit --step is validated before the "complete" short-circuit, so asking
    # to re-run a finished step is never answered with "nothing to run".
    if args.step is not None:
        if args.step < 1 or args.step > total:
            raise ToolError(f"--step must be between 1 and {total}.")
        if state["steps"].get(str(args.step), {}).get("status") == "done":
            raise Refused(
                f"Step {args.step} is already recorded as done. It has run before; "
                "running it again would load its batch twice. Do not clear the state "
                "file to get around this. Stop and report."
            )

    nxt = next_step(state, total)
    if nxt is None:
        print("Runbook complete: every step is recorded as done. Nothing to run.")
        return 0

    if args.step is None or args.step == nxt:
        n = nxt
    elif args.step > nxt:
        raise Refused(
            f"Step {args.step} cannot run yet: the runbook is executed in order and "
            f"step {nxt} has never run."
        )
    else:
        raise Refused(
            f"Step {args.step} is behind the runbook position: step {nxt} is the next "
            "one to run."
        )

    command = runbook.command(n)
    print(f"running step {n}/{total} from {runbook.root}")
    print(f"  {command}\n")

    # Durable claim BEFORE the command starts. If this session dies from here on,
    # the step stays `pending` and no later session may re-run it.
    state["steps"][str(n)] = {
        "status": "pending",
        "command": command,
        "started_at": now(),
    }
    state["updated_at"] = now()
    save_state(path, state)
    print(f"  [claimed] step {n} is pending and written to {path}")

    proc = subprocess.run(command, shell=True, cwd=runbook.root)
    code = proc.returncode

    rec = state["steps"][str(n)]
    rec["finished_at"] = now()
    rec["exit_code"] = code
    if code == 0:
        rec["status"] = "done"
    else:
        rec["status"] = "failed"
        rec["note"] = (
            "Command exited non-zero. Whether the load committed is unknown: the "
            "warehouse cannot be queried, and a failure can happen after the commit. "
            "Never re-run."
        )
    state["updated_at"] = now()
    save_state(path, state)

    if code == 0:
        print(f"\n  [done] step {n} exited 0.")
        following = next_step(state, total)
        if following is None:
            print("Runbook complete: every step is recorded as done.")
        else:
            print(f"Next: step {following}. Keep going; do not stop here.")
        return 0

    print(
        f"\n  [STOP] step {n} exited {code}.\n"
        f"  Its load outcome is UNKNOWN: the warehouse cannot be queried, and this "
        f"command can fail after the load committed. A failed load and a committed "
        f"load look identical from here.\n"
        f"  Do not run step {n} again, do not run any later step, do not edit the "
        f"command or the runbook to make it pass. Report step {n} to a human and end "
        f"the turn."
    )
    return E_STEP_FAILED


class Parser(argparse.ArgumentParser):
    """A usage mistake is a tool error (exit 1), never a safety refusal (exit 3)."""

    def error(self, message):  # noqa: D102 - argparse hook
        self.print_usage(sys.stderr)
        print(f"ERROR: {message}", file=sys.stderr)
        sys.exit(E_RUNBOOK)


def main(argv: list[str]) -> int:
    # --runbook/--state are accepted on either side of the subcommand: SUPPRESS keeps
    # an omitted post-subcommand flag from clobbering a value given before it.
    common = Parser(add_help=False)
    common.add_argument("--runbook", default=argparse.SUPPRESS,
                        help="path to the runbook (default: runbook.md here)")
    common.add_argument("--state", default=argparse.SUPPRESS,
                        help="override the state file path (default: <runbook dir>/"
                             f"{STATE_DIRNAME}/<runbook name>.state.json)")

    parser = Parser(
        prog="runbook_state.py",
        description="At-most-once, resumable executor for backfill runbooks.",
    )
    parser.add_argument("--runbook", default="runbook.md",
                        help="path to the runbook (default: runbook.md in the current directory)")
    parser.add_argument("--state", default=None,
                        help="override the state file path (default: <runbook dir>/"
                             f"{STATE_DIRNAME}/<runbook name>.state.json)")
    sub = parser.add_subparsers(dest="cmd", required=True)

    sub.add_parser("status", parents=[common], help="show durable progress (runs nothing)")
    show = sub.add_parser("show", parents=[common], help="print one step's full record")
    show.add_argument("--step", type=int, required=True)
    run = sub.add_parser("run", parents=[common],
                         help="run the next step under the at-most-once rules")
    run.add_argument("--step", type=int, default=None,
                     help="optional; must equal the next step to run")

    args = parser.parse_args(argv)
    handlers = {"status": cmd_status, "show": cmd_show, "run": cmd_run}
    try:
        return handlers[args.cmd](args)
    except Refused as exc:
        print(f"REFUSED: {exc}", file=sys.stderr)
        return E_REFUSED
    except ToolError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return E_RUNBOOK


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))