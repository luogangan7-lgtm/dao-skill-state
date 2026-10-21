#!/usr/bin/env python3
"""Run one step of a data backfill runbook, exactly once.

The warehouse cannot be asked which batches are loaded and re-running a step
duplicates rows, so this helper owns the only record of what has been attempted:

  * a step is marked ``pending`` and flushed to disk BEFORE its command starts
  * it is marked ``done`` only after the command exits 0
  * it is marked ``failed`` when the command exits non-zero
  * a step that is already pending keeps that state forever: its outcome is
    unknown, so it is never run again and never passed over

State lives in ``<workspace>/.backfill/state.json`` -- outside the skill
directory, next to the runbook it describes.

Subcommands:
  status              print runbook, per-step state and the one next action
  plan                print the parsed runbook steps without touching state
  run [step]          run step <step> (default: the first unfinished one)
  resolve <step> --took-effect --note "..."
                      record the user's out-of-band finding that a pending or
                      failed step did load, so the run can continue past it

Exit codes: 0 ok, 1 the step's command failed, 2 refused, 3 blocked/pending.
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
from datetime import datetime, timezone

SCHEMA = "backfill-runbook-state/1"
RUNBOOK_NAME = "runbook.md"
STATE_DIR = ".backfill"
STATE_FILE = "state.json"
LOCK_FILE = "lock"
LOG_DIR = "logs"

DONE, PENDING, FAILED = "done", "pending", "failed"

EXIT_OK, EXIT_STEP_FAILED, EXIT_REFUSED, EXIT_BLOCKED = 0, 1, 2, 3

STEP_RE = re.compile(r"^\s*(\d+)\.\s+(\S.*)$")


class Refuse(Exception):
    """A refusal always says what to do instead of what was refused."""

    def __init__(self, message, code=EXIT_REFUSED):
        super().__init__(message)
        self.code = code


def now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


# --------------------------------------------------------------------------
# runbook parsing: `1. <shell command>`, run in order from the workspace root
# --------------------------------------------------------------------------


def parse_runbook(text: str):
    """Return (steps, warnings).

    A numbered line starts a step. Indented non-blank lines continue the step
    above it (honouring a trailing backslash). Fenced blocks and lines at
    column 0 that are not numbered are prose, not commands.
    """
    steps, warnings, in_fence = [], [], False
    for lineno, raw in enumerate(text.splitlines(), 1):
        stripped = raw.strip()
        if in_fence:
            if stripped.startswith("```"):
                in_fence = False
            continue
        if stripped.startswith("```"):
            in_fence = True
            continue
        if not stripped:
            continue
        m = STEP_RE.match(raw)
        if m:
            steps.append({"number": int(m.group(1)), "command": m.group(2).strip(), "line": lineno})
            continue
        if raw[:1].isspace() and steps and not stripped.startswith("#"):
            prev = steps[-1]
            if prev["command"].endswith("\\"):
                prev["command"] = prev["command"][:-1].rstrip() + " " + stripped
            else:
                prev["command"] = prev["command"] + " " + stripped
            continue
        # prose at column 0, or a comment: not a step

    if not steps:
        raise Refuse(
            f"{RUNBOOK_NAME} has no numbered steps of the form `1. <shell command>`. "
            f"Read it directly and report what form it actually uses; do not invent steps."
        )

    seen, nums = set(), []
    for s in steps:
        if s["number"] in seen:
            raise Refuse(
                f"{RUNBOOK_NAME} line {s['line']} repeats step {s['number']}. "
                f"The order is ambiguous, so nothing was run. Report this to the user."
            )
        seen.add(s["number"])
        nums.append(s["number"])
    if nums != sorted(nums):
        raise Refuse(
            f"{RUNBOOK_NAME} numbers its steps out of order ({nums}). "
            f"Steps run in written order only when the numbering agrees with it; report this."
        )
    if nums[0] != 1 or nums != list(range(nums[0], nums[0] + len(nums))):
        warnings.append(f"step numbering is not 1..{len(nums)} ({nums}); steps still run in this order")
    return steps, warnings


def load_steps(workspace: str):
    path = os.path.join(workspace, RUNBOOK_NAME)
    if not os.path.isfile(path):
        raise Refuse(f"No {RUNBOOK_NAME} in {workspace}. Run a backfill from the directory that holds it.")
    with open(path, "rb") as fh:
        raw = fh.read()
    steps, warnings = parse_runbook(raw.decode("utf-8", "replace"))
    return steps, hashlib.sha256(raw).hexdigest(), warnings


# --------------------------------------------------------------------------
# state: written atomically and fsynced, because a lost pending record would
# invite a duplicate load
# --------------------------------------------------------------------------


def atomic_write_json(path: str, obj) -> None:
    tmp = f"{path}.tmp.{os.getpid()}"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(obj, fh, indent=2, sort_keys=True)
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
    dfd = os.open(os.path.dirname(path) or ".", os.O_RDONLY)
    try:
        os.fsync(dfd)
    finally:
        os.close(dfd)


def load_state(state_dir: str, workspace: str, steps, digest: str):
    path = os.path.join(state_dir, STATE_FILE)
    if not os.path.isfile(path):
        state = {
            "schema": SCHEMA,
            "workspace": workspace,
            "runbook": {"path": RUNBOOK_NAME, "sha256": digest, "step_count": len(steps)},
            "created_at": now(),
            "updated_at": now(),
            "steps": {},
            "resolutions": [],
        }
        atomic_write_json(path, state)
        return state, True

    try:
        with open(path, encoding="utf-8") as fh:
            state = json.load(fh)
    except (OSError, ValueError) as exc:
        raise Refuse(
            f"{path} exists but could not be read ({exc}). It is the only record of which steps "
            f"have run. Do not delete or hand-edit it; report the path and show the file."
        )
    if state.get("schema") != SCHEMA:
        raise Refuse(f"{path} has schema {state.get('schema')!r}, expected {SCHEMA!r}. Report it; do not rewrite it.")
    if state.get("workspace") != workspace:
        raise Refuse(
            f"{path} belongs to workspace {state.get('workspace')!r}, not {workspace!r}. "
            f"Running from the wrong directory could double-load. Report both paths."
        )
    recorded = state.get("runbook", {}).get("sha256")
    if recorded != digest:
        raise Refuse(
            f"{RUNBOOK_NAME} has changed since this backfill started (recorded sha256 {recorded}, "
            f"current {digest}). Step numbers may no longer mean the same commands, and re-running "
            f"them could duplicate rows. Do not run anything. Report this and show which step numbers "
            f"are finished in {path}."
        )
    return state, False


def save_state(state_dir: str, state) -> None:
    state["updated_at"] = now()
    atomic_write_json(os.path.join(state_dir, STATE_FILE), state)


def status_of(state, number):
    return state.get("steps", {}).get(str(number), {}).get("status")


def record_pending(state, step):
    state.setdefault("steps", {})[str(step["number"])] = {
        "status": PENDING,
        "command": step["command"],
        "runbook_line": step["line"],
        "started_at": now(),
        "outcome": "unknown: the command was started and the result was never recorded",
    }


def record_done(state, number, exit_code, log):
    entry = state["steps"][str(number)]
    entry.update(
        {
            "status": DONE,
            "finished_at": now(),
            "exit_code": exit_code,
            "log": log,
            "outcome": "command exited 0; the load reported success",
        }
    )


def record_failed(state, number, exit_code, log):
    entry = state["steps"][str(number)]
    entry.update(
        {
            "status": FAILED,
            "finished_at": now(),
            "exit_code": exit_code,
            "log": log,
            "outcome": (
                f"command exited {exit_code}; it may or may not have committed before failing, "
                f"so this step needs an out-of-band check and must not be retried"
            ),
        }
    )


# --------------------------------------------------------------------------
# single writer: two sessions must not drive the same runbook at once
# --------------------------------------------------------------------------


def acquire_lock(state_dir: str):
    fd = os.open(os.path.join(state_dir, LOCK_FILE), os.O_CREAT | os.O_RDWR, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as exc:
        os.close(fd)
        if exc.errno in (errno.EACCES, errno.EAGAIN):
            return None
        raise
    os.ftruncate(fd, 0)
    os.write(fd, f"pid {os.getpid()} since {now()}\n".encode())
    return fd


# --------------------------------------------------------------------------
# reporting
# --------------------------------------------------------------------------


def print_plan(steps, digest, warnings) -> None:
    print(f"{RUNBOOK_NAME} sha256 {digest}, {len(steps)} steps, run from the workspace root:")
    for s in steps:
        print(f"  {s['number']:>3}. {s['command']}")
    for w in warnings:
        print(f"warning: {w}", file=sys.stderr)


def block_guidance(step, status, entry) -> str:
    n = step["number"]
    cmd = entry.get("command", step["command"])
    since = entry.get("started_at") or entry.get("finished_at") or "an earlier session"
    if not status:
        return f"Step {n} has not run yet. Run it: `run {n}` (or plain `run`)."
    if status == PENDING:
        return (
            f"Step {n} ({cmd}) was marked pending at {since} and never came back: the command may or "
            f"not have committed. The warehouse cannot be queried for it, so its outcome is unknown.\n"
            f"  Do NOT run step {n}'s command again - a second load duplicates rows.\n"
            f"  Do NOT read the warehouse's own files to infer it, and do NOT hand-edit the state file.\n"
            f"  The cause being gone (the connection, the session, the machine) does not show whether "
            f"the load committed.\n"
            f"  What to do instead: stop the run here and report that step {n} needs checking out of band. "
            f"If the user confirms it loaded, record it with\n"
            f"      backfill_state.py resolve {n} --took-effect --note \"<who checked, what they saw>\"\n"
            f"  and then continue with step {n + 1} (or the next unfinished step). If it cannot be "
            f"checked, the run stays stopped at step {n} until the user says otherwise."
        )
    if status == FAILED:
        return (
            f"Step {n} ({cmd}) already failed at {entry.get('finished_at')} with exit code "
            f"{entry.get('exit_code')} and the run stopped there.\n"
            f"  Do NOT retry it and do NOT change anything to make it pass.\n"
            f"  Report step {n} to the user and end the turn. If the user later confirms out of band "
            f"that it did load, record it with\n"
            f"      backfill_state.py resolve {n} --took-effect --note \"<who checked, what they saw>\"\n"
            f"  and continue; there is no retry path."
        )
    return f"Step {n} has not run yet. Run it: `run {n}` (or plain `run`)."


def print_status(state, state_dir, steps, created: bool) -> int:
    rb = state["runbook"]
    print(f"runbook: {rb['path']}  sha256 {rb['sha256'][:12]}…  {rb['step_count']} steps")
    print(f"state:   {os.path.join(state_dir, STATE_FILE)}")
    print(f"started: {state['created_at']}   updated: {state['updated_at']}")
    if created:
        print("(state created just now; no step has run)")
    print()
    for s in steps:
        entry = state.get("steps", {}).get(str(s["number"]), {})
        status = entry.get("status", "not started")
        when = entry.get("finished_at") or entry.get("started_at") or "-"
        extra = f"exit {entry['exit_code']}" if "exit_code" in entry else ""
        if entry.get("log"):
            extra = (extra + "  " if extra else "") + entry["log"]
        print(f"  {s['number']:>3}. {status:<12} {when}  {extra}")
        if status == PENDING:
            print(f"       command: {entry.get('command', s['command'])}")
    print()

    unfinished = [s for s in steps if status_of(state, s["number"]) != DONE]
    if not unfinished:
        print(f"Next: nothing. All {len(steps)} steps are recorded done.")
        return EXIT_OK
    first = unfinished[0]
    status = status_of(state, first["number"])
    print(f"Next: step {first['number']} is the first unfinished step.")
    print(block_guidance(first, status or "not started", state.get("steps", {}).get(str(first["number"]), {})))
    return EXIT_BLOCKED if status in (PENDING, FAILED) else EXIT_OK


# --------------------------------------------------------------------------
# execution
# --------------------------------------------------------------------------


class Interrupted(Exception):
    """Ctrl-C or SIGTERM arrived while a step's command was running."""

    def __init__(self, signame):
        super().__init__(signame)
        self.signame = signame


def stop_process(proc) -> None:
    """Stop the command and everything it spawned, then reap it."""
    if proc.poll() is not None:
        return
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
    except (ProcessLookupError, PermissionError, OSError):
        proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError, OSError):
            proc.kill()
        proc.wait(timeout=10)


def run_command(command: str, workspace: str, log_path: str) -> int:
    """Run the step's shell command, teeing output to the log and the terminal.

    SIGINT and SIGTERM stop the command instead of being ignored while this
    reads its output. The step is left pending either way: whether the load
    committed is not something this process can see.
    """
    os.makedirs(os.path.dirname(log_path), exist_ok=True)
    proc = subprocess.Popen(
        command,
        shell=True,
        cwd=workspace,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        bufsize=1,
        start_new_session=True,
    )

    def on_signal(signum, _frame):
        raise Interrupted(signal.Signals(signum).name)

    previous = {sig: signal.signal(sig, on_signal) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        with open(log_path, "a", encoding="utf-8") as log:
            log.write(f"\n===== {now()} running: {command}\n")
            log.flush()
            for line in proc.stdout:
                sys.stdout.write(line)
                sys.stdout.flush()
                log.write(line)
            log.flush()
            os.fsync(log.fileno())
        return proc.wait()
    except KeyboardInterrupt:
        stop_process(proc)
        raise Interrupted("SIGINT") from None
    except Interrupted:
        stop_process(proc)
        raise
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        if proc.stdout:
            proc.stdout.close()


def cmd_run(args, workspace: str, state_dir: str) -> int:
    steps, digest, warnings = load_steps(workspace)
    state, created = load_state(state_dir, workspace, steps, digest)
    for w in warnings:
        print(f"warning: {w}", file=sys.stderr)

    if created:
        save_state(state_dir, state)

    unfinished = [s for s in steps if status_of(state, s["number"]) != DONE]
    if not unfinished:
        print(f"All {len(steps)} steps are recorded done. The backfill is finished; nothing to run.")
        return EXIT_OK

    first = unfinished[0]
    first_status = status_of(state, first["number"])
    target = args.step if args.step is not None else first["number"]
    step = next((s for s in steps if s["number"] == target), None)
    if step is None:
        raise Refuse(
            f"Step {target} is not in {RUNBOOK_NAME}. It has {len(steps)} steps: "
            f"{[s['number'] for s in steps]}."
        )
    if target != first["number"]:
        raise Refuse(
            f"Step {target} is not the next one to run. Step {first['number']} is unfinished"
            + (f" and is {first_status}" if first_status else "")
            + f". Run `run {first['number']}` (or just `run`) instead of guessing which steps a "
            f"previous session already finished."
        )
    if first_status:
        raise Refuse(block_guidance(step, first_status, state["steps"][str(target)]))

    log_rel = os.path.join(STATE_DIR, LOG_DIR, f"step-{target:02d}.log")
    record_pending(state, step)
    save_state(state_dir, state)
    print(f"[step {target}] marked pending in {os.path.join(state_dir, STATE_FILE)}")
    print(f"[step {target}] running: {step['command']}")

    try:
        exit_code = run_command(step["command"], workspace, os.path.join(workspace, log_rel))
    except (Interrupted, KeyboardInterrupt) as exc:
        print(
            f"\n[step {target}] interrupted ({getattr(exc, 'signame', 'SIGINT')}); its command was "
            f"stopped. It stays PENDING: the load may or may not have committed, so it must not be "
            f"run again. Report step {target} as needing an out-of-band check.",
            file=sys.stderr,
        )
        return EXIT_BLOCKED

    if exit_code == 0:
        record_done(state, target, exit_code, log_rel)
        save_state(state_dir, state)
        print(f"[step {target}] completed (exit 0); recorded done. log: {log_rel}")
        remaining = [s for s in steps if status_of(state, s["number"]) != DONE]
        if remaining:
            print(f"Next unfinished step: {remaining[0]['number']} — `run {remaining[0]['number']}`")
        else:
            print(f"All {len(steps)} steps are recorded done. The backfill is finished.")
        return EXIT_OK

    record_failed(state, target, exit_code, log_rel)
    save_state(state_dir, state)
    print(f"[step {target}] FAILED (exit {exit_code}); recorded failed. log: {log_rel}", file=sys.stderr)
    print(block_guidance(step, FAILED, state["steps"][str(target)]), file=sys.stderr)
    return EXIT_STEP_FAILED


def cmd_resolve(args, workspace: str, state_dir: str) -> int:
    steps, digest, warnings = load_steps(workspace)
    state, _ = load_state(state_dir, workspace, steps, digest)
    step = next((s for s in steps if s["number"] == args.step), None)
    if step is None:
        raise Refuse(f"Step {args.step} is not in {RUNBOOK_NAME}. It has steps {[s['number'] for s in steps]}.")
    status = status_of(state, args.step)
    if status is None:
        raise Refuse(f"Step {args.step} has not run, so there is nothing to resolve. Run it instead.")
    if status == DONE:
        print(f"Step {args.step} is already done; nothing to resolve.")
        return EXIT_OK
    if not args.note:
        raise Refuse("--note is required: it records who checked and what they saw, for the next session.")

    entry = state["steps"][str(args.step)]
    earlier = [s for s in steps if s["number"] < args.step and status_of(state, s["number"]) != DONE]
    if earlier:
        raise Refuse(
            f"Step {args.step} cannot be resolved while step {earlier[0]['number']} is unfinished "
            f"({status_of(state, earlier[0]['number']) or 'not started'}). Resolve steps in order so "
            f"the record stays a faithful sequence; do not mark later steps done to get past a block."
        )
    entry["status"] = DONE
    entry["status_before_resolution"] = status
    entry["outcome"] = "recorded as loaded by the user, out of band; not verified by this helper"
    entry["finished_at"] = now()
    entry["log"] = entry.get("log") or os.path.join(STATE_DIR, LOG_DIR, f"step-{args.step:02d}.log")
    state["steps"][str(args.step)] = entry
    state.setdefault("resolutions", []).append(
        {"step": args.step, "previous_status": status, "decided_at": now(), "note": args.note}
    )
    save_state(state_dir, state)
    print(f"Step {args.step} recorded done on the user's say-so: {args.note}")
    remaining = [s for s in steps if status_of(state, s["number"]) != DONE]
    if remaining:
        print(f"Next unfinished step: {remaining[0]['number']} — `run {remaining[0]['number']}`")
    else:
        print("All steps are recorded done. The backfill is finished.")
    return EXIT_OK


def cmd_status(args, workspace: str, state_dir: str) -> int:
    steps, digest, warnings = load_steps(workspace)
    state, created = load_state(state_dir, workspace, steps, digest)
    for w in warnings:
        print(f"warning: {w}", file=sys.stderr)
    return print_status(state, state_dir, steps, created)


def cmd_plan(args, workspace: str, state_dir: str) -> int:
    steps, digest, warnings = load_steps(workspace)
    print_plan(steps, digest, warnings)
    return EXIT_OK


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        prog="backfill_state.py",
        description="Run a backfill runbook's steps exactly once, across sessions.",
    )
    parser.add_argument(
        "--workspace",
        default=os.getcwd(),
        help="directory holding runbook.md (default: current directory)",
    )
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("status", help="show runbook, per-step state and the one next action")
    sub.add_parser("plan", help="print the parsed runbook steps; writes no state")
    run_p = sub.add_parser("run", help="run one step; refuses anything that could load a batch twice")
    run_p.add_argument("step", nargs="?", type=int, help="step number (default: the first unfinished)")
    res_p = sub.add_parser("resolve", help="record the user's out-of-band finding that a step did load")
    res_p.add_argument("step", type=int)
    res_p.add_argument("--took-effect", action="store_true", required=True,
                       help="the only resolution: the user confirmed out of band that this step loaded")
    res_p.add_argument("--note", help="who checked what, for whoever resumes next")

    args = parser.parse_args(argv)
    workspace = os.path.abspath(args.workspace)
    if not os.path.isdir(workspace):
        print(f"No such workspace: {workspace}", file=sys.stderr)
        return EXIT_REFUSED

    state_dir = os.path.join(workspace, STATE_DIR)
    handlers = {"status": cmd_status, "plan": cmd_plan, "resolve": cmd_resolve}
    try:
        if args.command in handlers:
            if args.command != "plan":
                os.makedirs(state_dir, exist_ok=True)
            return handlers[args.command](args, workspace, state_dir)

        os.makedirs(state_dir, exist_ok=True)
        lock = acquire_lock(state_dir)
        if lock is None:
            raise Refuse(
                f"Another backfill session is running this runbook (lock held on "
                f"{os.path.join(state_dir, LOCK_FILE)}). Two sessions driving one runbook can "
                f"double-load a batch. Run `status` to see where the run stands, or wait for the "
                f"other session to finish."
            )
        try:
            return cmd_run(args, workspace, state_dir)
        finally:
            os.close(lock)
    except Refuse as exc:
        print(f"\nREFUSED: {exc}", file=sys.stderr)
        return exc.code


if __name__ == "__main__":
    sys.exit(main())