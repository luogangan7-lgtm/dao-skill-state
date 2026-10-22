#!/usr/bin/env python3
"""Crash-safe runner for data backfill runbooks.

Loads a batch at most once, across days, sessions, crashes and mid-load
connection resets.

The warehouse is the authority for *effect*: every step's check command is run
on every pass, before anything is loaded. A step the warehouse reports as loaded
is skipped, whether this run loaded it or a previous session did.

The local ledger (.backfill/<runbook>.json) records only what the warehouse
cannot answer: which step a run stopped at, and that a step failed and was not
retried. It never overrides the warehouse. If the two disagree the run stops.

Nothing outside the .backfill/ directory is ever written. runbook.md is read
only, and never modified.
"""

from __future__ import annotations

import argparse
import errno
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "backfill-run/1"
STATE_DIRNAME = ".backfill"
LOG_DIRNAME = "logs"
DEFAULT_CHECK_TIMEOUT = 120  # a check that hangs has not answered
TAIL_LINES = 40

EXIT_OK = 0
EXIT_USAGE = 1
EXIT_STEP_FAILED = 2
EXIT_UNDETERMINED = 3


class DuplicateStep(Exception):
    def __init__(self, number: int, path: Path):
        super().__init__(number)
        self.number = number
        self.path = path

LOADED = "LOADED"
NOT_LOADED = "NOT_LOADED"
UNKNOWN = "UNKNOWN"

# "1. command", "1) command", optionally indented. Deliberately narrow: the
# runbook format is fixed, so parsing it here is deterministic. Prose, headings
# and fenced blocks are ignored, not treated as steps.
STEP_RE = re.compile(r"^[ \t]*(?P<n>\d+)[.)][ \t]+(?P<cmd>\S.*?)[ \t]*$")
FENCE_RE = re.compile(r"^[ \t]*(```|~~~)")


def now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def die(msg: str, code: int = EXIT_USAGE) -> "int":
    print(f"ERROR: {msg}", file=sys.stderr)
    return code


class Reporter:
    def __init__(self) -> None:
        self.lines: list[str] = []

    def say(self, text: str = "") -> None:
        self.lines.append(text)
        print(text, flush=True)


# --------------------------------------------------------------------------
# runbook
# --------------------------------------------------------------------------


def parse_runbook(path: Path) -> list[tuple[int, str]]:
    """Return [(number, command)] in file order, from numbered list items.

    Raises on a repeated step number: a step is identified by its number in the
    ledger, so two steps called "3" cannot be tracked, and a runbook that
    cannot be tracked must not be run.
    """
    steps: list[tuple[int, str]] = []
    seen: set[int] = set()
    in_fence = False
    for raw in path.read_text(encoding="utf-8").splitlines():
        if FENCE_RE.match(raw):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        m = STEP_RE.match(raw)
        if not m:
            continue
        n = int(m.group("n"))
        cmd = m.group("cmd").strip()
        if not cmd:
            continue
        if n in seen:
            raise DuplicateStep(n, path)
        seen.add(n)
        steps.append((n, cmd))
    return steps


def file_digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


# --------------------------------------------------------------------------
# checks
# --------------------------------------------------------------------------


def substitute(template: str, step_no: int, batch: str | None, command: str) -> str:
    # Plain replace, not str.format: runbook commands contain ${VAR} and braces.
    out = template
    for key, val in (("{step}", str(step_no)), ("{batch}", batch or ""), ("{command}", command)):
        out = out.replace(key, val)
    return out


def check_template_for(
    step_no: int,
    checks_file: dict,
    uniform: str | None,
) -> tuple[str | None, str | None]:
    """Return (check_template, problem) for a step. Exactly one is meaningful."""
    entry = checks_file.get(str(step_no), checks_file.get(step_no))
    if entry is not None:
        if isinstance(entry, dict):
            return entry.get("check"), None
        return str(entry), None
    if uniform:
        return uniform, None
    return None, "no check command given for this step"


def run_check(cmd: str, workspace: Path, timeout: int) -> tuple[str, str]:
    """Ask the warehouse about one batch.

    Convention: the command must print LOADED or NOT_LOADED on stdout and exit 0.
    Anything else -- a hang, a traceback, a table, an unexpected token, a
    non-zero exit -- is UNKNOWN. UNKNOWN never means "not loaded": an
    unanswered question must not turn into a second load.
    """
    shell = shutil.which("bash") or shutil.which("sh")
    if shell is None:
        return UNKNOWN, "no shell found to run the check command"
    try:
        p = subprocess.run(
            [shell, "-c", cmd],
            cwd=str(workspace),
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return UNKNOWN, f"check timed out after {timeout}s"
    except OSError as exc:
        return UNKNOWN, f"check could not start: {exc}"

    if p.returncode != 0:
        tail = (p.stderr or p.stdout or "").strip().splitlines()
        return UNKNOWN, f"check exited {p.returncode}: {tail[-1] if tail else 'no output'}"

    token = ""
    for line in reversed((p.stdout or "").splitlines()):
        if line.strip():
            token = line.strip().upper()
            break
    if token in (LOADED, NOT_LOADED):
        return token, None
    if token == UNKNOWN:
        return UNKNOWN, "check reported UNKNOWN"
    return UNKNOWN, f"check did not answer LOADED/NOT_LOADED (got {token!r})"


# --------------------------------------------------------------------------
# ledger
# --------------------------------------------------------------------------


def atomic_write_json(path: Path, obj: dict) -> None:
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(obj, fh, indent=2, sort_keys=True)
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
    dfd = os.open(str(path.parent), os.O_RDONLY)
    try:
        os.fsync(dfd)
    finally:
        os.close(dfd)


def load_state(path: Path, runbook: str, digest: str, started: str) -> dict:
    if path.exists():
        try:
            state = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            raise SystemExit(
                f"ERROR: ledger {path} is unreadable ({exc}). Stop. Resolve it with a human; "
                "do not delete it to make the run proceed."
            )
        if state.get("schema") != SCHEMA:
            raise SystemExit(
                f"ERROR: ledger {path} has schema {state.get('schema')!r}, expected {SCHEMA}. "
                "Stop and report."
            )
        state.setdefault("steps", {})
        return state
    return {
        "schema": SCHEMA,
        "runbook": runbook,
        "runbook_sha256": digest,
        "started_at": started,
        "updated_at": started,
        "steps": {},
    }


def save_state(path: Path, state: dict) -> None:
    state["updated_at"] = now_iso()
    atomic_write_json(path, state)


def record(state: dict, step_no: int, **fields) -> None:
    entry = state["steps"].setdefault(str(step_no), {})
    entry.update(fields)
    entry["updated_at"] = now_iso()


# --------------------------------------------------------------------------
# lock
# --------------------------------------------------------------------------


def acquire_lock(lock_path: Path) -> str | None:
    """One writer per runbook. Returns None if held by a live process."""
    payload = json.dumps({"pid": os.getpid(), "host": os.uname().nodename, "at": now_iso()})
    for _ in range(2):
        try:
            fd = os.open(str(lock_path), os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
        except OSError as exc:
            if exc.errno != errno.EEXIST:
                raise
            try:
                held = json.loads(lock_path.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                held = {}
            pid = held.get("pid")
            alive = False
            if isinstance(pid, int):
                try:
                    os.kill(pid, 0)
                    alive = True
                except ProcessLookupError:
                    alive = False
                except PermissionError:
                    alive = True
            if alive:
                return (
                    f"another process (pid {pid} on {held.get('host')}, started {held.get('at')}) "
                    "holds this backfill's lock. Two writers would load the same batches twice. "
                    "Wait for it, or find out what it is before removing the lock by hand."
                )
            print(
                f"WARNING: taking over a stale lock from pid {pid} (no such process). "
                "Proceeding; the warehouse check still decides what has been loaded.",
                file=sys.stderr,
            )
            try:
                lock_path.unlink()
            except OSError:
                pass
            continue
        with os.fdopen(fd, "w") as fh:
            fh.write(payload)
        return None
    return "could not take the lock; it keeps being recreated by another process"


def release_lock(lock_path: Path) -> None:
    try:
        lock_path.unlink()
    except OSError:
        pass


# --------------------------------------------------------------------------
# running a step
# --------------------------------------------------------------------------


def tail(text: str, lines: int = TAIL_LINES) -> str:
    return "\n".join(text.splitlines()[-lines:])


def run_step(
    command: str,
    workspace: Path,
    log_dir: Path,
    step_no: int,
    attempt: int,
    timeout: int,
) -> tuple[int, str, str]:
    shell = shutil.which("bash") or shutil.which("sh")
    if shell is None:
        return 127, "", "no shell found to run step commands"
    started = now_iso()
    try:
        p = subprocess.run(
            [shell, "-c", command],
            cwd=str(workspace),
            capture_output=True,
            text=True,
            timeout=timeout if timeout > 0 else None,
        )
        out, err, code = p.stdout or "", p.stderr or "", p.returncode
    except subprocess.TimeoutExpired as exc:
        out = (exc.stdout or b"").decode("utf-8", "replace") if isinstance(exc.stdout, bytes) else (exc.stdout or "")
        err = "TIMEOUT: step exceeded its timeout and was killed."
        code = 124
    except OSError as exc:
        out, err, code = "", f"could not start: {exc}", 126

    log_dir.mkdir(parents=True, exist_ok=True)
    stamp = started.replace(":", "").replace("-", "")
    log_path = log_dir / f"step-{step_no:03d}-attempt{attempt}-{stamp}.log"
    try:
        with open(log_path, "w", encoding="utf-8") as fh:
            fh.write(f"$ {command}\n# started {started}\n# exit {code}\n\n{out}\n{err}\n")
    except OSError:
        pass  # the ledger still holds the tail; a missing log file must not fail the run

    return code, tail(out), tail(err, 10)


# --------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------


class _Parser(argparse.ArgumentParser):
    """A bad command line is EXIT_USAGE, never EXIT_STEP_FAILED.

    Exit 2 means a load failed and a person has to look at it; a typo must not
    be mistaken for that.
    """

    def error(self, message: str):  # noqa: D102 - argparse hook
        self.print_usage(sys.stderr)
        print(f"{self.prog}: error: {message}", file=sys.stderr)
        raise SystemExit(EXIT_USAGE)


def build_parser() -> argparse.ArgumentParser:
    p = _Parser(
        prog="backfill_run.py",
        description=(
            "Run a data backfill runbook so no batch is loaded twice, across days, "
            "sessions, crashes and mid-load connection resets."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "The check command must print LOADED or NOT_LOADED on stdout and exit 0.\n"
            "Placeholders: {step}, {batch}, {command}.\n"
            "Exit codes: 0 complete, 1 usage, 2 a step failed (not retried), 3 undetermined.\n"
        ),
    )
    p.add_argument("--workspace", default=".", help="workspace root; steps run from here (default: cwd)")
    p.add_argument("--runbook", default="runbook.md", help="runbook path, relative to workspace")
    p.add_argument("--check-cmd", help="the check command the runbook names; may use {step}/{batch}")
    p.add_argument("--checks-file", help='JSON mapping step -> check command, e.g. {"1": "..."}')
    p.add_argument("--batch", action="append", default=[], metavar="N=ID",
                   help="batch id a step loads, when it differs from the step number (repeatable)")
    p.add_argument("--state-dir", default=None, help="ledger directory (default: <workspace>/.backfill)")
    p.add_argument("--check-timeout", type=int, default=DEFAULT_CHECK_TIMEOUT,
                   help=f"seconds a check may take before it counts as UNKNOWN (default: {DEFAULT_CHECK_TIMEOUT})")
    p.add_argument("--step-timeout", type=int, default=0, help="seconds a step may take; 0 = no limit")
    p.add_argument("--dry-run", action="store_true",
                   help="ask the warehouse about every step, run nothing, print the plan")
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    r = Reporter()

    workspace = Path(args.workspace).resolve()
    if not workspace.is_dir():
        return die(f"workspace {workspace} is not a directory")

    runbook = (workspace / args.runbook).resolve()
    if not runbook.is_file():
        return die(f"no runbook at {runbook}")
    if os.path.commonpath([str(runbook), str(workspace)]) != str(workspace):
        return die("the runbook must live inside the workspace")

    try:
        steps = parse_runbook(runbook)
    except DuplicateStep as exc:
        return die(
            f"{runbook} lists step {exc.number} more than once. A step is identified by its "
            "number, so this runbook cannot be tracked and must not be run. Do not edit the "
            "runbook to fix it; report it."
        )
    if not steps:
        return die(f"{runbook} has no numbered steps of the form '1. <command>'")
    numbers = [n for n, _ in steps]
    if numbers != list(range(1, len(steps) + 1)):
        r.say(f"WARNING: step numbers are {numbers}, not 1..{len(steps)}; using file order.")

    digest = file_digest(runbook)

    batches: dict[int, str] = {}
    for item in args.batch:
        if "=" not in item:
            return die(f"--batch expects N=ID, got {item!r}")
        key, _, val = item.partition("=")
        try:
            batches[int(key)] = val
        except ValueError:
            return die(f"--batch expects a step number, got {key!r}")

    checks_file: dict = {}
    if args.checks_file:
        try:
            raw = json.loads(Path(args.checks_file).read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            return die(f"--checks-file unreadable: {exc}")
        if not isinstance(raw, dict):
            return die("--checks-file must be a JSON object mapping step number -> command")
        checks_file = raw

    if not args.check_cmd and not checks_file:
        return die(
            "no check command given. The runbook names the command that asks the warehouse whether "
            "a batch is loaded; pass it with --check-cmd (or per step with --checks-file). Without it "
            "no batch can be proven loaded, so nothing can be loaded safely."
        )

    state_dir = Path(args.state_dir).resolve() if args.state_dir else workspace / STATE_DIRNAME
    state_dir.mkdir(parents=True, exist_ok=True)
    log_dir = state_dir / LOG_DIRNAME
    state_path = state_dir / (runbook.stem + ".json")
    lock_path = state_dir / (runbook.stem + ".lock")

    held = acquire_lock(lock_path)
    if held:
        return die(held, EXIT_UNDETERMINED)

    try:
        state = load_state(state_path, str(runbook), digest, now_iso())
        prior_digest = state.get("runbook_sha256")
        if prior_digest and prior_digest != digest:
            r.say(f"STOPPED  runbook {runbook} changed since the last recorded run.")
            r.say(f"         recorded {prior_digest[:12]}, on disk {digest[:12]}.")
            r.say("         Never edit a runbook to fit this runner. A human must confirm the")
            r.say("         change, then start a fresh ledger for it.")
            return EXIT_UNDETERMINED

        r.say(f"backfill  {runbook}  ({len(steps)} steps)")
        r.say(f"run from  {workspace}")
        r.say(f"ledger    {state_path}")
        r.say(f"runbook   sha256 {digest[:12]}")
        if state.get("started_at"):
            r.say(f"started   {state['started_at']}")
        r.say()

        # Ask the warehouse about EVERY step before loading anything, not only
        # the step that looks unfinished. A step finished by a session that
        # ended days ago is the one most likely to be reloaded by mistake.
        verdicts: dict[int, dict] = {}
        for n, cmd in steps:
            template, problem = check_template_for(n, checks_file, args.check_cmd)
            entry: dict = {"command": cmd}
            if template is None:
                verdicts[n] = {**entry, "verdict": UNKNOWN,
                              "detail": problem or "no check command for this step"}
                continue
            if "{batch}" in template and n not in batches:
                verdicts[n] = {**entry, "verdict": UNKNOWN,
                              "detail": "check command uses {batch} but no batch id was given "
                                        f"(--batch {n}=ID)"}
                continue
            ask = substitute(template, n, batches.get(n), cmd)
            entry["ask"] = ask
            entry["checked_at"] = now_iso()
            verdict, detail = run_check(ask, workspace, args.check_timeout)
            entry["verdict"] = verdict
            if detail:
                entry["detail"] = detail
            elif args.dry_run:
                entry["detail"] = ask
            verdicts[n] = entry

        width = max(len(str(n)) for n in verdicts)
        r.say(f"{'step':>{width}}  {'warehouse':<11} {'ledger':<8} {'batch':<10} action")
        for n, cmd in steps:
            v = verdicts[n]
            prior = (state["steps"].get(str(n)) or {}).get("status", "-")
            r.say(f"{n:>{width}}  {v['verdict']:<11} {prior:<8} {batches.get(n, '-'):<10} {v.get('detail', '')}")

        if args.dry_run:
            undetermined = [n for n, _ in steps if verdicts[n]["verdict"] == UNKNOWN]
            r.say()
            if undetermined:
                r.say(f"RESULT: dry run, nothing loaded. Steps {', '.join(map(str, undetermined))} "
                      "cannot be checked yet.")
                for n in undetermined:
                    r.say(f"  step {n}: {verdicts[n].get('detail')}")
                r.say("  Fix the check before loading anything; see --checks-file above.")
                return EXIT_UNDETERMINED
            r.say("RESULT: dry run. Every step is checkable; nothing was loaded.")
            return EXIT_OK

        # Decide, in order, what each step means. The first step that cannot be
        # resolved safely ends the run.
        for n, cmd in steps:
            v = verdicts[n]
            prior = (state["steps"].get(str(n)) or {}).get("status")
            verdict = v["verdict"]

            if verdict == UNKNOWN:
                r.say()
                r.say(f"STOPPED at step {n}: the warehouse could not be asked whether this batch is")
                r.say(f"loaded ({v.get('detail')}). That is not the same as 'not loaded', and")
                r.say("loading on a guess is how rows get duplicated.")
                r.say()
                r.say("  If the runbook names this check somewhere else, re-run with")
                r.say("  --checks-file '{\"%d\": \"<command>\"}'. Otherwise fix the check" % n)
                r.say("  access and run the same command again.")
                return EXIT_UNDETERMINED

            if verdict == LOADED:
                record(state, n, status="done", command=cmd, checked_at=v["checked_at"],
                       verified_loaded_at=now_iso())
                v["action"] = "skipped: already in the warehouse"
                continue

            # verdict == NOT_LOADED
            if prior == "done":
                r.say()
                r.say(f"STOPPED at step {n}: a previous run recorded this step as loaded, but the")
                r.say("warehouse says it is not. Those cannot both be true. Do not reload and do not")
                r.say("edit the ledger to make them agree -- a human has to reconcile the batch.")
                return EXIT_UNDETERMINED

            if prior == "failed":
                r.say()
                r.say(f"STOPPED at step {n}: it failed earlier and was not retried.")
                r.say("  Runbook rule: a failed step is reported, not rerun. Its cause is gone by")
                r.say("  now, and that says nothing about whether the load committed.")
                r.say("  The warehouse says this batch is not loaded, so this step still has to be")
                r.say("  done deliberately, by a person who has decided that is safe.")
                r.say(f"  Ledger: {state_path} (step {n}, status failed)")
                return EXIT_STEP_FAILED

            # NOT_LOADED and no recorded failure: the load never took effect, so
            # running it now is its first effective run, not a retry. This is
            # also the crash-mid-step case: pending in the ledger, absent in the
            # warehouse.
            if prior == "pending":
                r.say()
                r.say(f"step {n} was left pending by an earlier session and is NOT in the")
                r.say("warehouse, so that attempt committed nothing. Running it now.")

            attempt = int((state["steps"].get(str(n)) or {}).get("attempts", 0)) + 1
            record(state, n, status="pending", command=cmd, batch=batches.get(n),
                   attempts=attempt, started_at=now_iso(), exit_code=None, output_tail=None)
            save_state(state_path, state)  # pending BEFORE the load, flushed

            r.say()
            r.say(f">>> step {n}: {cmd}")
            code, out, err = run_step(cmd, workspace, log_dir, n, attempt, args.step_timeout)

            if code == 0:
                record(state, n, status="done", exit_code=0, finished_at=now_iso(),
                       output_tail=out, error_tail=None, verified_loaded_at=now_iso())
                save_state(state_path, state)
                r.say(f"<<< step {n} finished (exit 0)")
                v["action"] = "loaded"
                continue

            record(state, n, status="failed", exit_code=code, finished_at=now_iso(),
                   output_tail=out, error_tail=err)
            save_state(state_path, state)
            r.say(f"<<< step {n} FAILED (exit {code})")
            if out:
                r.say(tail(out, 15))
            if err:
                r.say(tail(err, 10))
            r.say()
            r.say(f"RESULT: stopped at step {n}. It failed, so it is reported here and not")
            r.say("retried. Nothing was changed to make it pass and the runbook was not")
            r.say("edited. Note that a non-zero exit does not mean the load was rolled back --")
            r.say("the warehouse check on the next run decides that, and this runbook's")
            r.say(f"check for step {n} is the command in the ledger:")
            r.say(f"  {verdicts[n].get('ask')}")
            r.say()
            r.say(f"  Ledger: {state_path} (step {n}, status failed)")
            r.say("  A person decides whether to continue. Do not re-run this command now.")
            return EXIT_STEP_FAILED

        save_state(state_path, state)
        r.say()
        r.say(f"RESULT: complete. All {len(steps)} steps are in the warehouse.")
        r.say(f"ledger: {state_path}")
        return EXIT_OK
    finally:
        release_lock(lock_path)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(128 + signal.SIGTERM))
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print(
            "\nInterrupted. Any step still marked pending in the ledger committed nothing the\n"
            "warehouse knows about; run the same command again and the checks will settle it.",
            file=sys.stderr,
        )
        sys.exit(130)