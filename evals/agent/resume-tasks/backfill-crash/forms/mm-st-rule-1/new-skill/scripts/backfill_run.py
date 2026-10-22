#!/usr/bin/env python3
"""Durable, exactly-once runner for data backfill runbooks.

Every step loads a batch into the warehouse, so running a step twice duplicates
rows. This tool keeps the record of what is known, marks a step `running` before
its command is launched and `done` only after it exits 0, and refuses to launch
any step that is not provably un-run. The warehouse -- not this file -- decides
whether a step took effect; a step whose outcome is unknown stays `running` or
`failed` until it is resolved with `resolve`.

State lives outside the skill directory, at <workspace>/.dao/state/backfill/.
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import hashlib
import json
import os
import re
import socket
import subprocess
import sys
import tempfile
from datetime import datetime, timezone

STATE_VERSION = 1
MAX_LOG_TAIL = 2000
DEFAULT_LOCK_WAIT = 15.0

STATUS_TODO = "todo"          # not yet attempted in any session
STATUS_RUNNING = "running"    # launched; outcome unknown until resolved
STATUS_DONE = "done"          # confirmed to have taken effect
STATUS_FAILED = "failed"      # command exited nonzero; outcome still unknown

# A step in one of these states has an unknown or confirmed effect on the
# warehouse. It must never be launched again on the strength of local state.
UNKNOWN_STATES = (STATUS_RUNNING, STATUS_FAILED)

EXIT_OK = 0
EXIT_USAGE = 2
EXIT_REFUSED = 3
EXIT_STEP_FAILED = 4


class Refuse(Exception):
    """A refusal the agent must act on. The message is the instruction."""

    def __init__(self, message, code=EXIT_REFUSED):
        super().__init__(message)
        self.code = code


# --------------------------------------------------------------------------
# helpers
# --------------------------------------------------------------------------

def now():
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def atomic_write_json(path, obj):
    """Write durably: temp file in the same dir, fsync, then rename."""
    directory = os.path.dirname(os.path.abspath(path))
    os.makedirs(directory, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=directory, prefix=".tmp-", suffix=".json")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(obj, fh, indent=2, sort_keys=False)
            fh.write("\n")
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, path)
        dir_fd = os.open(directory, os.O_DIRECTORY)
        try:
            os.fsync(dir_fd)
        finally:
            os.close(dir_fd)
    except BaseException:
        if os.path.exists(tmp):
            os.unlink(tmp)
        raise


# --------------------------------------------------------------------------
# state
# --------------------------------------------------------------------------

TOP_LEVEL_KEYS = {
    "version", "run_id", "runbook", "runbook_sha256", "workspace", "mode",
    "check", "steps", "updated_at", "notes",
}
CHECK_KEYS = {"cmd", "exit_is_loaded", "loaded_regex", "not_loaded_regex"}
STEP_KEYS = {
    "n", "command", "batch", "status", "checked_at", "claimed_at",
    "finished_at", "exit_code", "log", "pid", "last_check",
}
CHECK_RECORD_KEYS = {"at", "exit_code", "verdict", "output_tail"}


def new_step(n, command, batch=None):
    return {
        "n": n,
        "command": command,
        "batch": batch,
        "status": STATUS_TODO,
        "checked_at": None,
        "claimed_at": None,
        "finished_at": None,
        "exit_code": None,
        "log": None,
        "last_check": None,
    }


def new_state(run_id, runbook, digest, workspace, mode, check, steps):
    return {
        "version": STATE_VERSION,
        "run_id": run_id,
        "runbook": runbook,
        "runbook_sha256": digest,
        "workspace": workspace,
        "mode": mode,
        "check": check,
        "steps": steps,
        "updated_at": now(),
        "notes": [],
    }


def load_state(path):
    if not os.path.exists(path):
        raise Refuse(
            "No state file at %s.\n"
            "There is no record of what earlier sessions did, so nothing can be "
            "assumed unloaded. Run `init` first; `init` defaults to verify mode, "
            "which requires checking each step against the warehouse before it "
            "may run. Do not run step commands directly to find out -- a direct "
            "run of an already-committed step duplicates rows." % path,
            EXIT_USAGE)
    with open(path, encoding="utf-8") as fh:
        try:
            state = json.load(fh)
        except json.JSONDecodeError as exc:
            raise Refuse(
                "State file %s is not valid JSON (%s).\n"
                "Refusing to guess: overwriting it would erase the only record "
                "of which batches are loaded. Inspect it, repair it, or "
                "re-derive it from the warehouse with `init` into a fresh "
                "run id." % (path, exc), EXIT_USAGE)
    validate_state(state, path)
    return state


def validate_state(state, path):
    if not isinstance(state, dict):
        raise Refuse("State file %s is not a JSON object." % path, EXIT_USAGE)
    version = state.get("version")
    if version != STATE_VERSION:
        raise Refuse(
            "State file %s has version %r, this tool understands version %d.\n"
            "Refusing to touch it. Use a matching tool version, or read the "
            "file and reconcile by hand against the warehouse." % (path, version, STATE_VERSION),
            EXIT_USAGE)
    unknown = set(state) - TOP_LEVEL_KEYS
    if unknown:
        raise Refuse(
            "State file %s has unexpected keys: %s.\n"
            "Refusing to overwrite keys this tool does not understand."
            % (path, ", ".join(sorted(unknown))), EXIT_USAGE)
    for key in ("run_id", "runbook", "workspace", "mode", "steps"):
        if key not in state:
            raise Refuse("State file %s is missing %r." % (path, key), EXIT_USAGE)
    if state["mode"] not in ("verify", "fresh"):
        raise Refuse("State file %s has invalid mode %r." % (path, state["mode"]), EXIT_USAGE)
    check = state.get("check") or {}
    unknown = set(check) - CHECK_KEYS
    if unknown:
        raise Refuse("State file %s: unexpected check keys %s." % (path, ", ".join(sorted(unknown))), EXIT_USAGE)
    if not isinstance(state["steps"], list) or not state["steps"]:
        raise Refuse("State file %s has no steps." % path, EXIT_USAGE)
    seen = set()
    for step in state["steps"]:
        if not isinstance(step, dict):
            raise Refuse("State file %s: a step is not an object." % path, EXIT_USAGE)
        unknown = set(step) - STEP_KEYS
        if unknown:
            raise Refuse(
                "State file %s: step has unexpected keys %s."
                % (path, ", ".join(sorted(unknown))), EXIT_USAGE)
        if step.get("status") not in (STATUS_TODO, STATUS_RUNNING, STATUS_DONE, STATUS_FAILED):
            raise Refuse(
                "State file %s: step %r has invalid status %r."
                % (path, step.get("n"), step.get("status")), EXIT_USAGE)
        if step["n"] in seen:
            raise Refuse("State file %s: duplicate step %r." % (path, step["n"]), EXIT_USAGE)
        seen.add(step["n"])


def save_state(path, state):
    state["updated_at"] = now()
    atomic_write_json(path, state)


# --------------------------------------------------------------------------
# locking: one writer per runbook on this host
# --------------------------------------------------------------------------

def acquire_lock(path, wait):
    """Hold an exclusive lock for the whole invocation, including the step.

    Two sessions racing on the same step are the other way a batch loads
    twice. The lock serializes them; the `running` status is what stops the
    loser once the winner is gone.
    """
    lock_path = path + ".lock"
    os.makedirs(os.path.dirname(os.path.abspath(lock_path)), exist_ok=True)
    fh = open(lock_path, "a+")
    deadline = now_monotonic() + wait
    while True:
        try:
            fcntl.flock(fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            fh.seek(0)
            fh.truncate()
            fh.write("pid=%d host=%s at=%s\n" % (os.getpid(), socket.gethostname(), now()))
            fh.flush()
            return fh
        except OSError as exc:
            if exc.errno not in (errno.EACCES, errno.EAGAIN):
                raise
            if now_monotonic() >= deadline:
                fh.close()
                raise Refuse(
                    "Another session is working on this runbook (lock held on %s).\n"
                    "Wait for it to finish, or report the conflict to the user. Do "
                    "not remove the lock file and proceed: two sessions running "
                    "steps of the same backfill is how a batch loads twice."
                    % lock_path)
            sleep_short()


def now_monotonic():
    import time
    return time.monotonic()


def sleep_short():
    import time
    time.sleep(0.25)


# --------------------------------------------------------------------------
# runbook parsing
# --------------------------------------------------------------------------

STEP_RE = re.compile(r"^\s{0,3}(\d+)[.)]\s+(\S.*?)\s*$")


def parse_runbook(path):
    """Read numbered steps `N. <shell command>` in file order.

    Only the numbered-step form is fixed by the runbook contract. Anything
    else in the file (prose about prerequisites, notes, the command that checks
    whether a batch is loaded) is deliberately not interpreted here -- the
    agent reads the runbook itself and passes those in.
    """
    steps = []
    seen = set()
    with open(path, encoding="utf-8") as fh:
        for lineno, line in enumerate(fh, 1):
            if line.lstrip().startswith(("```", "#", "<!--")):
                continue
            m = STEP_RE.match(line)
            if not m:
                continue
            n = int(m.group(1))
            command = m.group(2).strip()
            if not command or n in seen:
                continue
            seen.add(n)
            steps.append(new_step(n, command))
    if not steps:
        raise Refuse(
            "No numbered steps found in %s.\n"
            "The runbook is expected to contain `N. <shell command>` lines. "
            "Read the runbook yourself; if its steps are written differently, "
            "pass them with --steps-file as a JSON list of "
            '[{"n": 1, "command": "..."}]. Do not guess step commands from '
            "anything else." % path, EXIT_USAGE)
    steps.sort(key=lambda s: s["n"])
    return steps


def load_steps_file(path):
    with open(path, encoding="utf-8") as fh:
        data = json.load(fh)
    if not isinstance(data, list) or not data:
        raise Refuse("--steps-file must be a non-empty JSON list.", EXIT_USAGE)
    steps = []
    for item in data:
        if not isinstance(item, dict) or "n" not in item or "command" not in item:
            raise Refuse('--steps-file items need "n" and "command".', EXIT_USAGE)
        step = new_step(item["n"], item["command"], item.get("batch"))
        steps.append(step)
    steps.sort(key=lambda s: s["n"])
    return steps


# --------------------------------------------------------------------------
# check command: the warehouse is the authority on what took effect
# --------------------------------------------------------------------------

def render(template, step):
    """Substitute without str.format -- step commands contain braces."""
    return (template
            .replace("{n}", str(step["n"]))
            .replace("{step}", str(step["n"]))
            .replace("{batch}", str(step.get("batch") if step.get("batch") is not None else "")))


def run_check(state, step):
    check = state.get("check") or {}
    template = check.get("cmd")
    if not template:
        raise Refuse(
            "This run has no warehouse check command configured.\n"
            "Read the runbook: it names the command that reports whether a "
            "batch is loaded. Set it once with `init --check-cmd \"...\"` "
            "(add --loaded-regex / --not-loaded-regex if it reports by output "
            "rather than exit code). Without a check, a step whose outcome is "
            "unknown cannot be resolved -- the only safe answer is to report "
            "which step needs checking and stop.", EXIT_USAGE)
    command = render(template, step)
    if not step.get("batch") and "{batch}" in template:
        raise Refuse(
            "The check command needs a batch id for step %d but none is "
            "recorded. Set it at init time with --batch-map. Checking the "
            "wrong batch id is worse than not checking." % step["n"], EXIT_USAGE)
    result = subprocess.run(
        command, shell=True, cwd=state["workspace"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    output = result.stdout.decode("utf-8", "replace")
    verdict = classify(check, result.returncode, output)
    return {
        "at": now(),
        "exit_code": result.returncode,
        "verdict": verdict,
        "output_tail": output[-MAX_LOG_TAIL:],
    }


def classify(check, exit_code, output):
    """Map a check result to loaded / not-loaded / unknown.

    `unknown` is a real answer: it means the check did not clearly say, and
    the step must not be run on the strength of it.
    """
    if check.get("not_loaded_regex") and re.search(check["not_loaded_regex"], output, re.I | re.M):
        return "not-loaded"
    if check.get("loaded_regex") and re.search(check["loaded_regex"], output, re.I | re.M):
        return "loaded"
    if check.get("loaded_regex") or check.get("not_loaded_regex"):
        return "unknown"
    expected = check.get("exit_is_loaded", 0)
    return "loaded" if exit_code == expected else "not-loaded"


def find_step(state, n):
    for step in state["steps"]:
        if step["n"] == n:
            return step
    raise Refuse("Step %d is not in this runbook (steps: %s)."
                 % (n, ", ".join(str(s["n"]) for s in state["steps"])), EXIT_USAGE)


# --------------------------------------------------------------------------
# commands
# --------------------------------------------------------------------------

def cmd_init(args, state_path):
    runbook = os.path.abspath(args.runbook)
    if not os.path.isfile(runbook):
        raise Refuse("No runbook at %s." % runbook, EXIT_USAGE)
    workspace = os.path.abspath(args.workspace or os.path.dirname(runbook))

    if os.path.exists(state_path) and not args.reset:
        raise Refuse(
            "State already exists at %s.\n"
            "`init` does not overwrite it: the existing record is the only "
            "thing saying which batches are loaded. Use `status` to continue "
            "the run, or --reset only when you have verified against the "
            "warehouse that nothing from this runbook is loaded."
            % state_path, EXIT_USAGE)

    steps = load_steps_file(args.steps_file) if args.steps_file else parse_runbook(runbook)

    if args.batch_map:
        with open(args.batch_map, encoding="utf-8") as fh:
            mapping = {int(k): v for k, v in json.load(fh).items()}
        for step in steps:
            step["batch"] = mapping.get(step["n"])

    check = {
        "cmd": args.check_cmd,
        "exit_is_loaded": args.check_exit,
        "loaded_regex": args.loaded_regex,
        "not_loaded_regex": args.not_loaded_regex,
    }
    mode = "fresh" if args.assume_fresh else "verify"
    run_id = args.run or os.path.splitext(os.path.basename(runbook))[0]
    state = new_state(run_id, runbook, sha256_file(runbook), workspace, mode, check, steps)

    if args.mark_loaded:
        for n in args.mark_loaded:
            step = find_step(state, n)
            if step["status"] != STATUS_TODO:
                raise Refuse("Step %d is %s, not todo." % (n, step["status"]), EXIT_USAGE)
            step["status"] = STATUS_DONE
            step["checked_at"] = now()
            step["finished_at"] = now()
            step["last_check"] = {"at": now(), "exit_code": None,
                                  "verdict": "loaded", "output_tail": ""}
            state["notes"].append(
                "Step %d marked loaded at init from a warehouse check performed "
                "before this state file existed." % n)
    save_state(state_path, state)

    print("Initialized run %r over %s" % (run_id, runbook))
    print("  workspace : %s" % workspace)
    print("  mode      : %s%s" % (
        mode,
        "" if mode == "verify" else "  (steps run without a prior check)"))
    print("  check cmd : %s" % (check["cmd"] or "NOT CONFIGURED -- see below"))
    print("  state     : %s" % state_path)
    print("\nSteps read from the runbook:")
    for step in state["steps"]:
        print("  %2d. %s" % (step["n"], step["command"]))
    if mode == "verify":
        print("\nVerify mode: each step must be checked against the warehouse "
              "before it may run.\nRead the runbook for the command that "
              "reports whether a batch is loaded, then re-run `init` with "
              "--check-cmd.")
    if not check["cmd"]:
        print("\nNo check command set. Steps that fail or get cut off cannot be "
              "resolved safely. Read the runbook and set one.")
    return EXIT_OK


def describe(state, state_path):
    print("Run %r -- %s" % (state["run_id"], state["runbook"]))
    print("  workspace : %s" % state["workspace"])
    print("  mode      : %s" % state["mode"])
    print("  state     : %s (updated %s)" % (state_path, state.get("updated_at")))
    print("  runbook   : sha256 %s" % (state["runbook_sha256"] or "unknown")[:16])
    print("\n  %-4s %-8s %-12s %-8s %s" % ("step", "status", "warehouse",
                                            "checked", "command"))
    for step in state["steps"]:
        check = step.get("last_check") or {}
        print("  %-4d %-8s %-12s %-8s %s" % (
            step["n"], step["status"], check.get("verdict") or "-",
            "yes" if step.get("checked_at") else "no", step["command"]))
    for note in state.get("notes") or []:
        print("\nnote: %s" % note)


def cmd_status(args, state_path):
    state = load_state(state_path)
    describe(state, state_path)
    return next_action(state)


def next_action(state):
    steps = state["steps"]
    done = [s for s in steps if s["status"] == STATUS_DONE]
    for step in steps:
        if step["status"] in UNKNOWN_STATES:
            print("\nNEXT: step %d (%s) has an unknown outcome." % (step["n"], step["status"]))
            print("  Resolve it against the warehouse -- do not run it:")
            print("    python3 backfill_run.py resolve %d" % step["n"])
            print("  If that check says the batch is loaded, it becomes done. If "
                  "it says not loaded, report that and stop: the runbook's rule "
                  "is to stop and report on failure, not to retry.")
            return EXIT_REFUSED
    remaining = [s for s in steps if s["status"] == STATUS_TODO]
    if remaining:
        first = remaining[0]
        if state["mode"] == "verify" and not first.get("checked_at"):
            print("\nNEXT: verify step %d against the warehouse before running "
                  "it (verify mode -- no state file records who ran what):"
                  % first["n"])
            print("    python3 backfill_run.py resolve %d" % first["n"])
            print("  If it reports loaded, the step is recorded done and never "
                  "run. If it reports not loaded, it is cleared to run.")
        else:
            print("\nNEXT: step %d:  python3 backfill_run.py run %d"
                  % (first["n"], first["n"]))
        return EXIT_OK
    print("\nAll %d steps are confirmed loaded." % len(done))
    return EXIT_OK


def guard_runbook(state):
    """A runbook edited mid-backfill can renumber steps into a double load."""
    if not os.path.isfile(state["runbook"]):
        raise Refuse("Runbook %s no longer exists." % state["runbook"], EXIT_USAGE)
    current = sha256_file(state["runbook"])
    if current != state.get("runbook_sha256"):
        raise Refuse(
            "runbook.md has changed since this run started (sha256 %s, "
            "recorded %s).\n"
            "Steps may have been renumbered, so the recorded status of step N "
            "may describe a different batch now. Stop and report this to the "
            "user. Do not re-run steps and do not edit the runbook; the "
            "warehouse is the only authority on what is loaded."
            % (current[:16], (state.get("runbook_sha256") or "?")[:16]))


def cmd_run(args, state_path):
    state = load_state(state_path)
    guard_runbook(state)
    step = find_step(state, args.step)
    n = step["n"]

    if step["status"] == STATUS_DONE:
        raise Refuse(
            "Step %d is already recorded as loaded. Refusing to run it -- a "
            "second load duplicates rows.\nIf you believe this is wrong, check "
            "the warehouse with `resolve %d` and report; do not run it." % (n, n))
    if step["status"] in UNKNOWN_STATES:
        raise Refuse(
            "Step %d is %s, meaning a previous session may have run it and the "
            "outcome was never established.\n"
            "Running it now is exactly the case that duplicates rows. Resolve "
            "it against the warehouse first:\n"
            "    python3 backfill_run.py resolve %d\n"
            "and act on what the warehouse says. A failure that is gone now "
            "does not mean the load did not happen." % (n, step["status"], n))
    if state["mode"] == "verify" and step["status"] == STATUS_TODO \
            and not step.get("checked_at"):
        raise Refuse(
            "This run is in verify mode: step %d has no warehouse check on "
            "record, and there may be a state file from an earlier session that "
            "no longer exists.\nCheck it before running:\n"
            "    python3 backfill_run.py resolve %d\n"
            "If the check says loaded, it is recorded as done and never run. "
            "If it says not loaded, `resolve %d --not-loaded` clears it for "
            "running." % (n, n, n))

    # Claim before launch. If this process dies here, the step stays `running`
    # and the next session resolves it instead of guessing.
    log_dir = os.path.join(state["workspace"], ".dao", "logs", "backfill", state["run_id"])
    os.makedirs(log_dir, exist_ok=True)
    log_path = os.path.join(log_dir, "step-%d.log" % n)

    step["status"] = STATUS_RUNNING
    step["claimed_at"] = now()
    step["finished_at"] = None
    step["exit_code"] = None
    step["log"] = os.path.relpath(log_path, state["workspace"])
    save_state(state_path, state)

    print("== step %d/%d ==  %s" % (n, len(state["steps"]), step["command"]))
    print("   marked running at %s; log: %s" % (step["claimed_at"], log_path))
    sys.stdout.flush()

    try:
        with open(log_path, "wb") as logfh:
            proc = subprocess.Popen(
                step["command"], shell=True, cwd=state["workspace"],
                stdout=logfh, stderr=subprocess.STDOUT)
            if args.timeout:
                try:
                    code = proc.wait(timeout=args.timeout)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
                    raise Refuse(
                        "Step %d exceeded --timeout %ss and was killed. It stays "
                        "`running`; resolve it against the warehouse."
                        % (n, args.timeout))
            else:
                code = proc.wait()
    except BaseException as exc:
        step["status"] = STATUS_FAILED
        step["finished_at"] = now()
        save_state(state_path, state)
        raise Refuse("Step %d could not be launched (%s). It stays `running`; "
                     "resolve it against the warehouse before doing anything "
                     "else." % (n, exc))

    step["exit_code"] = code
    step["finished_at"] = now()
    if code == EXIT_OK:
        step["status"] = STATUS_DONE
        save_state(state_path, state)
        print("== step %d done (exit 0) ==" % n)
        return next_action(state)
    step["status"] = STATUS_FAILED
    save_state(state_path, state)
    tail = read_tail(log_path)
    print("== step %d FAILED (exit %d) ==" % (n, code), file=sys.stderr)
    if tail:
        print("\nlast lines of %s:\n%s" % (log_path, tail), file=sys.stderr)
    print(
        "\nSTOP HERE. The load may or may not have committed before the command "
        "failed, so the warehouse is the only authority.\n"
        "  1. Resolve it:   python3 backfill_run.py resolve %d\n"
        "  2. If it reports loaded  -> the batch is in; the run continues.\n"
        "  3. If it reports not loaded -> report this step and stop. Do not "
        "retry it and do not change anything to make it pass.\n"
        "Do not edit %s to move past this step, and do not run the step's "
        "command directly to 'see if it works' -- that is the double load."
        % (n, state_path), file=sys.stderr)
    return EXIT_STEP_FAILED


def read_tail(path, limit=40):
    try:
        with open(path, "rb") as fh:
            fh.seek(0, os.SEEK_END)
            size = fh.tell()
            fh.seek(max(0, size - 8192))
            text = fh.read().decode("utf-8", "replace")
    except OSError:
        return ""
    return "\n".join(text.splitlines()[-limit:])


def cmd_resolve(args, state_path):
    state = load_state(state_path)
    guard_runbook(state)
    step = find_step(state, args.step)
    n = step["n"]

    if step["status"] == STATUS_DONE and not args.not_loaded:
        raise Refuse(
            "Step %d is already confirmed loaded and is not re-checked. Leave "
            "it done." % n)

    record = run_check(state, step)
    record["command"] = render(state["check"]["cmd"], step)
    step["last_check"] = record
    verdict = record["verdict"]

    print("check: %s" % record["command"])
    print("exit  : %s" % record["exit_code"])
    print("output:\n%s" % (record["output_tail"] or "(empty)"))
    print("verdict: %s" % verdict)

    explicit = args.loaded or args.not_loaded
    if args.loaded and args.not_loaded:
        raise Refuse("--loaded and --not-loaded are mutually exclusive.", EXIT_USAGE)
    if explicit:
        print("verdict: overridden to %s by explicit flag" % ("loaded" if args.loaded else "not-loaded"))
        verdict = "loaded" if args.loaded else "not-loaded"

    if verdict == "unknown":
        save_state(state_path, state)
        raise Refuse(
            "The check did not clearly say whether batch for step %d is "
            "loaded.\nStep %d stays %s. Decide from the output above (or "
            "re-run the check by hand and pass --loaded / --not-loaded), or "
            "report this step to the user and stop. Do not run the step on an "
            "unclear answer." % (n, n, step["status"]))

    if verdict == "loaded":
        previous = step["status"]
        step["status"] = STATUS_DONE
        step["checked_at"] = record["at"]
        step["finished_at"] = now()
        save_state(state_path, state)
        print("\nstep %d: %s -> done (the warehouse has these rows)" % (n, previous))
        return next_action(state)

    # not-loaded
    if step["status"] == STATUS_FAILED:
        save_state(state_path, state)
        raise Refuse(
            "Step %d is NOT loaded. It previously failed and stays `failed`.\n"
            "The runbook's rule is: stop and report a failed step; do not retry "
            "it and do not change anything to make it pass.\n"
            "Report step %d to the user with the reason it failed and what you "
            "found here. If the user decides to re-run this exact step, they "
            "will say so; only then run:\n"
            "    python3 backfill_run.py retry %d --authorized-by \"<what they "
            "said>\"\n"
            "Until then, end the turn." % (n, n, n))
    if step["status"] == STATUS_RUNNING:
        save_state(state_path, state)
        raise Refuse(
            "Step %d is NOT loaded, and it is marked `running`: a previous "
            "session launched it and never learned the outcome.\n"
            "The warehouse now says the rows are not there, so it is safe to "
            "run -- but the runbook says not to retry a failed step without "
            "the user. Report this to the user and stop; if they authorize "
            "re-running this exact step:\n"
            "    python3 backfill_run.py retry %d --authorized-by \"<what they "
            "said>\"" % (n, n))
    step["status"] = STATUS_TODO
    step["checked_at"] = record["at"]
    save_state(state_path, state)
    print("\nstep %d: confirmed not loaded; cleared to run." % n)
    return next_action(state)


def cmd_retry(args, state_path):
    state = load_state(state_path)
    guard_runbook(state)
    step = find_step(state, args.step)
    n = step["n"]

    if step["status"] != STATUS_FAILED:
        raise Refuse(
            "`retry` is for a step that failed and was then confirmed not "
            "loaded. Step %d is %s.\nUse `run` for a todo step; use `resolve` "
            "to settle one whose outcome is unknown." % (n, step["status"]),
            EXIT_USAGE)
    check = step.get("last_check") or {}
    if check.get("verdict") != "not-loaded":
        raise Refuse(
            "Refusing to retry step %d: no warehouse check on record showing "
            "the batch is absent (last verdict: %s).\nRetry is only safe once "
            "the warehouse has confirmed the rows are not there. Run "
            "`resolve %d` first." % (n, check.get("verdict"), n))
    if not args.authorized_by:
        raise Refuse(
            "Retrying a failed step requires --authorized-by with who "
            "authorized it. The runbook says a failed step is reported, not "
            "retried; this flag is the record that a person decided "
            "otherwise.", EXIT_USAGE)

    state["notes"].append(
        "Step %d re-run at user request: %s" % (n, args.authorized_by))
    state["notes"] = state["notes"][-50:]
    # Cleared to todo so `run` will accept it; `checked_at` and the not-loaded
    # verdict on record are what made re-running it safe in the first place.
    step["status"] = STATUS_TODO
    save_state(state_path, state)
    args.step = n
    return cmd_run(args, state_path)


# --------------------------------------------------------------------------
# cli
# --------------------------------------------------------------------------

def default_state_path(workspace, run_id):
    return os.path.join(workspace, ".dao", "state", "backfill", "%s.json" % run_id)


def build_parser():
    p = argparse.ArgumentParser(
        prog="backfill_run.py",
        description="Exactly-once runner for data backfill runbooks.")
    p.add_argument("--state", help="state file (default: <workspace>/.dao/state/backfill/<run>.json)")
    p.add_argument("--workspace", help="directory steps are run from (default: the runbook's directory)")
    p.add_argument("--run", help="run id, used for the default state filename")
    p.add_argument("--lock-wait", type=float, default=DEFAULT_LOCK_WAIT,
                   help="seconds to wait for another session's lock (default %s)" % DEFAULT_LOCK_WAIT)
    sub = p.add_subparsers(dest="cmd", required=True)

    i = sub.add_parser("init", help="read the runbook and create state")
    i.add_argument("--runbook", default="runbook.md")
    i.add_argument("--steps-file", help="JSON list of {n, command} if the runbook is not plain numbered lines")
    i.add_argument("--check-cmd", help="runbook's command that reports whether a batch is loaded; "
                                       "may use {n}, {step}, {batch}")
    i.add_argument("--check-exit", type=int, default=0,
                   help="check exit code meaning 'loaded' (default 0)")
    i.add_argument("--loaded-regex", help="regex in check output meaning loaded (e.g. 'not found' for a probe that exits 0 either way)")
    i.add_argument("--not-loaded-regex", help="regex in check output meaning not loaded; checked first")
    i.add_argument("--batch-map", help="JSON object mapping step number to batch id")
    i.add_argument("--mark-loaded", type=int, nargs="+", metavar="N",
                   help="steps already confirmed loaded by the warehouse before state existed")
    i.add_argument("--assume-fresh",
                   help="every batch is known unloaded; skip the pre-run check (use only when nothing from this runbook has ever been loaded)")
    i.add_argument("--reset", help="overwrite existing state (destructive; only after verifying the warehouse has nothing from this runbook)")
    i.set_defaults(func=cmd_init)

    s = sub.add_parser("status", help="show the run and what to do next")
    s.set_defaults(func=cmd_status)

    r = sub.add_parser("run", help="run one step, marking it running first")
    r.add_argument("step", type=int)
    r.add_argument("--timeout", type=float, help="seconds before the step is killed; it stays running and must be resolved")
    r.set_defaults(func=cmd_run)

    v = sub.add_parser("resolve", help="ask the warehouse whether a step took effect")
    v.add_argument("step", type=int)
    v.add_argument("--loaded", action="store_true", help="you read the check output and it says loaded")
    v.add_argument("--not-loaded", action="store_true", help="you read the check output and it says not loaded")
    v.set_defaults(func=cmd_resolve)

    t = sub.add_parser("retry", help="re-run a failed step the user explicitly authorized")
    t.add_argument("step", type=int)
    t.add_argument("--authorized-by", help="who authorized the re-run")
    t.add_argument("--timeout", type=float, help="seconds before the step is killed; it stays running and must be resolved")
    t.set_defaults(func=cmd_retry)
    return p


def main(argv=None):
    args = build_parser().parse_args(argv)

    # init decides its own state path (the run id comes from it); everything
    # else resolves it from the runbook/workspace before taking the lock.
    if args.state:
        state_path = os.path.abspath(args.state)
    elif args.cmd == "init":
        workspace = os.path.abspath(args.workspace or os.path.dirname(
            os.path.abspath(args.runbook)) or ".")
        run_id = args.run or os.path.splitext(os.path.basename(args.runbook))[0]
        state_path = default_state_path(workspace, run_id)
    else:
        workspace = os.path.abspath(args.workspace or ".")
        run_id = args.run
        state_path = default_state_path(workspace, run_id or "runbook")

    lock = None
    try:
        if args.cmd != "init" or os.path.exists(state_path):
            lock = acquire_lock(state_path, args.lock_wait)
        return args.func(args, state_path)
    except Refuse as exc:
        print("\nREFUSED\n%s" % exc, file=sys.stderr)
        return exc.code
    except KeyboardInterrupt:
        print("\nInterrupted.\nIf this was during `run`, that step is marked "
              "`running`: resolve it against the warehouse, do not re-run it.",
              file=sys.stderr)
        return 130
    finally:
        if lock is not None:
            lock.close()


if __name__ == "__main__":
    sys.exit(main())