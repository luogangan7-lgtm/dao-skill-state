#!/usr/bin/env python3
"""Executor for numbered backfill runbook steps that must never run twice.

The warehouse cannot be asked which batches are loaded, so this script keeps the
only durable record of that. It marks a step `running` and flushes it to disk
*before* the command starts, and marks it `done` only after the command exits 0.
A step left `running` or `failed` has an unknown effect and blocks the run.

Invoke from the workspace root that holds the runbook.

Exit codes: 0 ok · 2 blocked, needs a human · 3 step ran and failed · 4 usage error
"""

import argparse
import datetime
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

STATE_DIR = ".backfill-state"
SCHEMA_VERSION = 1
HISTORY_LIMIT = 10
TAIL_LINES = 40

TODO, RUNNING, DONE, FAILED = "todo", "running", "done", "failed"
BLOCKING = (RUNNING, FAILED)

STEP_RE = re.compile(r"^(\s*)(\d+)[.)]\s+(.*\S)\s*$")
FENCE_RE = re.compile(r"^\s*(```|~~~)")


class Usage(Exception):
    """Bad input or bad state; the caller should print it and exit 4."""


class Blocked(Exception):
    """The run cannot proceed without a human decision; exit 2."""

    def __init__(self, message):
        super().__init__(message)
        self.message = message


def now():
    return datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat()


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


# ---------------------------------------------------------------- runbook parsing


def parse_runbook(path):
    """Read the runbook as written. Never writes to it.

    Numbered items are shell commands (the documented format). Fenced blocks are
    literal text and skipped. An indented, non-blank line continues the item above
    it -- joined with a newline when the line ends in a backslash, else a space.
    A command wrapped in a single pair of backticks is unwrapped.
    """
    if not path.is_file():
        raise Usage(f"runbook not found: {path}")

    steps, in_fence = [], False
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if FENCE_RE.match(raw):
            in_fence = not in_fence
            continue
        if in_fence or not raw.strip():
            continue

        match = STEP_RE.match(raw)
        if match:
            command = _unwrap(match.group(3))
            steps.append({"n": int(match.group(2)), "command": command, "lineno": lineno})
            continue

        if raw[:1].isspace() and steps:
            tail = raw.strip()
            prior = steps[-1]["command"]
            steps[-1]["command"] = prior + ("\n" + tail if prior.endswith("\\") else " " + tail)
            continue

        if not raw[:1].isspace() and re.match(r"^\s*\d+[.)]\s*$", raw):
            raise Usage(f"{path}:{lineno}: numbered step has no command")

    if not steps:
        raise Usage(f"{path}: no numbered steps found (looking for `1. <shell command>`)")

    seen = [s["n"] for s in steps]
    if len(set(seen)) != len(seen):
        raise Usage(f"{path}: duplicate step numbers {sorted({n for n in seen if seen.count(n) > 1})}")

    for step in steps:
        step["state"] = TODO
        step["attempts"] = 0
        step["history"] = []
    return steps


def _unwrap(command):
    """Drop markdown backticks that wrap the whole command or just its ends
    (`cmd` or `exe` args). Interior backticks are left alone -- they are far more
    likely to be the author's own text than shell syntax."""
    if command.startswith("`"):
        close = command.find("`", 1)
        if close > 0:
            command = command[1:close].lstrip() + command[close + 1:]
    if command.endswith("`") and not command.endswith("``"):
        open_ = command.rfind("`", 0, len(command) - 1)
        if open_ > 0:
            command = command[:open_].rstrip() + command[open_:]
    return command.strip()


# ---------------------------------------------------------------- state


def state_path(runbook, root):
    # runbook.md -> runbook.json; any other name keeps its full name so two
    # differently-suffixed runbooks in one workspace cannot collide.
    name = runbook.name
    stem = name[:-3] if name.lower().endswith(".md") else name
    slug = re.sub(r"[^a-zA-Z0-9._-]+", "-", stem).strip("-") or "runbook"
    return root / STATE_DIR / f"{slug}.json"


class Store:
    """The durable record of what took effect. Single writer, guarded by flock."""

    def __init__(self, path, root):
        self.path, self.root = path, path.parent
        self._lock_handle = None
        self.data = None

    def acquire(self):
        try:
            import fcntl
        except ImportError:
            return
        self.root.mkdir(parents=True, exist_ok=True)
        self._lock_handle = open(f"{self.path}.lock", "a+")
        try:
            fcntl.flock(self._lock_handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            sys.exit("another session holds the runbook lock; wait for it or stop it first")

    def load(self):
        if not self.path.is_file():
            raise Usage(f"no state at {self.path} — run `init` first")
        try:
            self.data = json.loads(self.path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            raise Usage(f"{self.path} is unreadable ({exc}) — do not hand-edit; restore or delete it deliberately")
        if self.data.get("schema_version") != SCHEMA_VERSION:
            raise Usage(f"{self.path}: unsupported schema_version {self.data.get('schema_version')!r}")
        return self.data

    def save(self):
        self.data["updated_at"] = now()
        self.root.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".json.tmp")
        with open(tmp, "w", encoding="utf-8") as handle:
            json.dump(self.data, handle, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, self.path)  # atomic: never a half-written record
        fd = os.open(self.root, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def check_drift(store, runbook):
    """The runbook is the authoritative input. If it changed, `step 7` may mean
    something else, and a step's recorded number no longer identifies its command."""
    current = sha256(runbook)
    recorded = store.data.get("runbook_sha256")
    if recorded != current:
        shown = f"{recorded[:12]}" if recorded else "unrecorded"
        raise Blocked(
            f"{runbook} changed since this run started ({shown} -> {current[:12]}).\n"
            "Recorded step numbers may no longer name the same commands. Do not guess which\n"
            "is right. Report this and have someone reconcile it against the warehouse."
        )


def blocker(store):
    """First step whose effect is unknown, if any. Its state is the whole reason
    this script exists: `running` means a previous session stopped mid-step."""
    return next((s for s in store.data["steps"] if s["state"] in BLOCKING), None)


def describe(step):
    if step["state"] == RUNNING:
        cause = (
            f"a previous session stopped while it was executing (started {step.get('started_at', '?')}).\n"
            "  The command may have finished, may still be running, or may have died mid-load."
        )
    else:
        last = step["history"][-1] if step["history"] else {}
        cause = (
            f"the command exited {last.get('detail', 'non-zero')} at {last.get('at', '?')}."
        )
    return (
        f"BLOCKED on step {step['n']}: {cause}\n"
        f"  command: {step['command']}\n"
        f"  whether the batch is in the warehouse: UNKNOWN — the warehouse has no status query.\n\n"
        "Do not run it again and do not continue to later steps. Report this to the user, who\n"
        "checks the warehouse directly and then answers: did it load, yes or no."
    )


# ---------------------------------------------------------------- commands


def cmd_init(args, root):
    runbook = root / args.runbook
    steps = parse_runbook(runbook)
    store = Store(state_path(runbook, root), root)
    store.acquire()
    if store.path.exists():
        raise Usage(f"{store.path} already exists — this run is in progress. Read it with `status`.")
    store.data = {
        "schema_version": SCHEMA_VERSION,
        "runbook": runbook.name,
        "runbook_sha256": sha256(runbook),
        "workspace": str(root.resolve()),
        "created_at": now(),
        "updated_at": now(),
        "steps": steps,
    }
    store.save()
    print(f"Recorded {len(steps)} step(s) in {store.path}\n")
    for step in steps:
        print(f"  {step['n']}. {step['command']}")
    print("\nNothing has run yet. Execute one step per `run`.")


def cmd_status(args, root):
    runbook = root / args.runbook
    store = Store(state_path(runbook, root), root)
    store.acquire()
    data = store.load()
    print(f"runbook {data['runbook']} · started {data['created_at']} · updated {data['updated_at']}")
    for step in data["steps"]:
        tries = f" ×{step['attempts']}" if step["attempts"] > 1 else ""
        print(f"  {step['n']:>3}. {step['state']:<7}{tries}  {step['command']}")
    stuck = blocker(store)
    if stuck:
        print("\n" + describe(stuck))
        return 2
    remaining = [s for s in data["steps"] if s["state"] == TODO]
    print(f"\n{len(remaining)} step(s) left; next is {remaining[0]['n']}." if remaining else "\nRunbook complete.")
    return 0


def cmd_next(args, root):
    runbook = root / args.runbook
    store = Store(state_path(runbook, root), root)
    store.acquire()
    store.load()
    check_drift(store, runbook)
    stuck = blocker(store)
    if stuck:
        print(describe(stuck))
        return 2
    upcoming = next((s for s in store.data["steps"] if s["state"] == TODO), None)
    if upcoming is None:
        print("Runbook complete: every step is recorded done.")
        return 0
    print(f"next step {upcoming['n']}: {upcoming['command']}")
    return 0


def cmd_run(args, root):
    runbook = root / args.runbook
    store = Store(state_path(runbook, root), root)
    store.acquire()
    store.load()
    check_drift(store, runbook)

    stuck = blocker(store)
    if stuck:
        print(describe(stuck))
        return 2

    step = next((s for s in store.data["steps"] if s["state"] == TODO), None)
    if step is None:
        print("Runbook complete: every step is recorded done.")
        return 0

    command = args.command or step["command"]

    # Record the intent to run BEFORE the command exists. If this session dies
    # mid-step, the record is what stops a later session from running it again.
    step["state"] = RUNNING
    step["attempts"] += 1
    step["started_at"] = now()
    step["command"] = command
    store.save()

    log = store.path.with_suffix(".log")
    print(f"-> step {step['n']}: {command}\n   log: {log}")
    with open(log, "a", encoding="utf-8") as handle:
        handle.write(f"\n===== step {step['n']} attempt {step['attempts']} at {now()} =====\n$ {command}\n")
        handle.flush()
        proc = subprocess.run(command, shell=True, cwd=root, stdout=handle, stderr=subprocess.STDOUT)

    detail = f"exit {proc.returncode}" if proc.returncode >= 0 else f"signal {-proc.returncode}"
    step["history"].append({"at": now(), "detail": detail})
    del step["history"][:-HISTORY_LIMIT]

    if proc.returncode == 0:
        step["state"] = DONE
        step["finished_at"] = now()
        store.save()
        print(f"\nstep {step['n']} done ({detail}).")
        tail(log, "log")
        return 0

    step["state"] = FAILED
    step["finished_at"] = now()
    store.save()
    print(f"\nstep {step['n']} FAILED ({detail}).\n")
    print(describe(step))
    tail(log, "last log lines")
    return 3


def cmd_resolve(args, root):
    runbook = root / args.runbook
    store = Store(state_path(runbook, root), root)
    store.acquire()
    store.load()
    check_drift(store, runbook)

    step = next((s for s in store.data["steps"] if s["n"] == args.n), None)
    if step is None:
        raise Usage(f"no step {args.n} in {runbook.name}")
    if step["state"] not in BLOCKING:
        raise Usage(f"step {args.n} is {step['state']}; only a running or failed step needs resolving")
    if not args.did_load and not args.did_not_load:
        raise Usage("say what happened: --did-load or --did-not-load")

    if args.did_load:
        step["state"] = DONE
        step["finished_at"] = now()
        print(f"step {step['n']} recorded done — the user confirmed the batch is loaded.")
    else:
        # The user says the batch is absent. The load is still owed, and it may be
        # run once more — but only because they just said so, and `attempts` keeps
        # the earlier failure visible to every later session.
        step["state"] = TODO
        print(
            f"step {step['n']} returned to todo — the user confirmed the batch did NOT load.\n"
            "   Its earlier failure stays on record. Run it again only now, on the user's say-so;"
            "\n   every later session sees this step has been attempted before."
        )
    step["history"].append({"at": now(), "detail": "user resolved: did-load" if args.did_load else "user resolved: did-not-load"})
    del step["history"][:-HISTORY_LIMIT]
    store.save()


def tail(log, label):
    if not log.is_file():
        return
    lines = log.read_bytes().decode("utf-8", "replace").splitlines()[-TAIL_LINES:]
    if lines:
        print(f"\n--- {label} ---")
        for line in lines:
            print(f"  {line}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--runbook", default="runbook.md", help="runbook file, relative to the workspace root")
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("init", help="read the runbook and create state; nothing is run")
    sub.add_parser("status", help="show every step and what blocks the run")
    sub.add_parser("next", help="print the next step's command, or why the run is blocked")
    run = sub.add_parser("run", help="record, then execute exactly one step")
    run.add_argument("--command", help="override the parsed command for this step only (shown in state)")
    resolve = sub.add_parser("resolve", help="record what the user decided about a blocked step")
    resolve.add_argument("n", type=int)
    resolve.add_argument("--did-load", action="store_true")
    resolve.add_argument("--did-not-load", action="store_true")
    args = parser.parse_args()

    root = Path.cwd()
    try:
        return {
            "init": cmd_init, "status": cmd_status, "next": cmd_next,
            "run": cmd_run, "resolve": cmd_resolve,
        }[args.cmd](args, root)
    except Usage as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 4
    except Blocked as exc:
        print(exc.message)
        return 2


if __name__ == "__main__":
    sys.exit(main())