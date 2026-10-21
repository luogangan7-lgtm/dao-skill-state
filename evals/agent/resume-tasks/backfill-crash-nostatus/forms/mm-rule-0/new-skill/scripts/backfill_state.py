#!/usr/bin/env python3
"""Durable step state for a resumable backfill runbook.

One JSON file per runbook, holding only what has actually been observed about
each step: whether its command exited 0, and when. This file cannot tell you
whether a batch loaded in the warehouse -- it does not know, and neither does
the warehouse. Its whole job is to keep a committed step from looking
uncommitted, so no step ever runs twice.

It never runs a step and never touches the warehouse. Mutate it only through
this script; hand edits are what duplicate rows.

Statuses:
  not_started  no attempt recorded
  pending      started, no exit status recorded (session ended mid-step)
  failed       started, an error was observed, commit unknown
  done         exited 0 and that was recorded -- it may already be in the warehouse

Exit codes: 0 ok | 1 usage or IO error | 2 refused/blocked | 3 no run state yet
"""

import argparse
import datetime
import errno
import hashlib
import json
import os
import re
import sys
import tempfile

SCHEMA_VERSION = 1
STATE_DIRNAME = ".backfill"

NOT_STARTED = "not_started"
PENDING = "pending"
FAILED = "failed"
DONE = "done"
UNCONFIRMED = (PENDING, FAILED)

RUN_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
MAX_RESOLUTIONS = 5
MAX_TEXT = 2000
MAX_TOTAL_STEPS = 100000


def now():
    return datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def die(message, code=1):
    sys.stdout.write(message.rstrip("\n") + "\n")
    sys.exit(code)


def text(value, limit=MAX_TEXT):
    value = (value or "").strip()
    if len(value) > limit:
        value = value[: limit - 3] + "..."
    return value


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except OSError as exc:
        return exc.errno == errno.EPERM
    return True


class StateLock:
    """Advisory lock so two sessions cannot interleave writes to one run.

    A lock left by a dead process is stale and is taken over; a lock held by a
    live process means another session is working on this runbook, and this one
    stops rather than racing it.
    """

    def __init__(self, path, prog):
        self.path = path
        self.prog = prog

    def __enter__(self):
        for _ in range(2):
            try:
                fd = os.open(self.path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            except FileExistsError:
                holder = None
                try:
                    with open(self.path) as handle:
                        holder = int((handle.read().strip() or "-1").split()[0])
                except (OSError, ValueError, IndexError):
                    holder = None
                if holder and holder != os.getpid() and pid_alive(holder):
                    die(
                        "REFUSED: another live process (pid %d) is holding %s.\n"
                        "One runbook is worked by one session at a time: two agents running\n"
                        "steps concurrently is how a batch gets loaded twice. Stop and find out\n"
                        "whether that process is still working before continuing." % (holder, self.path),
                        2,
                    )
                os.remove(self.path)
                continue
            os.write(fd, ("%d %s\n" % (os.getpid(), now())).encode())
            os.close(fd)
            return self
        die("REFUSED: could not take the lock at %s (it keeps changing hands)." % self.path, 2)

    def __exit__(self, *exc_info):
        try:
            os.remove(self.path)
        except OSError:
            pass
        return False


class Run:
    """Resolves paths for one runbook and loads/saves its state."""

    def __init__(self, args, prog):
        self.prog = prog
        self.workspace = os.path.abspath(args.workspace)
        self.runbook = args.runbook
        self.runbook_path = (
            args.runbook if os.path.isabs(args.runbook) else os.path.join(self.workspace, args.runbook)
        )
        run_id = args.run or os.path.splitext(os.path.basename(self.runbook))[0]
        if not RUN_ID_RE.match(run_id):
            die("Invalid run name %r: use letters, digits, dot, dash, underscore." % run_id)
        self.run_id = run_id
        self.state_dir = os.path.join(self.workspace, STATE_DIRNAME)
        self.state_path = os.path.join(self.state_dir, run_id + ".json")
        self.lock_path = self.state_path + ".lock"

    def fingerprint(self):
        if not os.path.isfile(self.runbook_path):
            die("No runbook at %s" % self.runbook_path)
        return sha256_file(self.runbook_path)

    def load(self):
        try:
            with open(self.state_path) as handle:
                return json.load(handle)
        except FileNotFoundError:
            die(
                "No run state at %s.\n"
                "Read %s, count its steps, then: %s init --steps <count>"
                % (self.state_path, self.runbook, self.prog),
                3,
            )
        except (OSError, ValueError) as exc:
            die("Cannot read %s: %s\nThis file is the only record of what has run. Do not\n"
                "recreate or overwrite it: ask the user what happened to this run." % (self.state_path, exc))

    def save(self, state):
        state["updated_at"] = now()
        state["schema_version"] = SCHEMA_VERSION
        fd, tmp = tempfile.mkstemp(dir=self.state_dir, prefix=".tmp-")
        try:
            with os.fdopen(fd, "w") as handle:
                json.dump(state, handle, indent=2)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(tmp, self.state_path)
        except BaseException:
            if os.path.exists(tmp):
                os.remove(tmp)
            raise
        dir_fd = os.open(self.state_dir, os.O_RDONLY)
        try:
            os.fsync(dir_fd)
        finally:
            os.close(dir_fd)


def step_record(state, number):
    return state["steps"].get(str(number), {"status": NOT_STARTED, "attempts": 0})


def first_unfinished(state):
    for number in range(1, state["total_steps"] + 1):
        if step_record(state, number)["status"] != DONE:
            return number
    return None


def check_range(state, number):
    if not isinstance(number, int) or not 1 <= number <= state["total_steps"]:
        die("Step %r is out of range; this runbook has %d steps." % (number, state["total_steps"]))


def block_message(run, number, record):
    """The refusal text. Says what to do instead, and what not to do."""
    return (
        "REFUSED: step %d has no confirmed outcome.\n"
        "\n"
        "  command    %s\n"
        "  started    %s\n"
        "  observed   %s\n"
        "\n"
        "That command may already have committed to the warehouse. A non-zero exit, a\n"
        "dropped connection, or a missing exit status does not show whether the load\n"
        "landed -- and it does not stop showing it once the cause is gone. The warehouse\n"
        "cannot be queried for loaded batches, so nothing here can settle the question.\n"
        "\n"
        "Do not: run this command again, hand-edit %s, or read the warehouse's own\n"
        "tables or files to guess whether it loaded.\n"
        "\n"
        "Do: report step %d above to the user and end the run here. Only the user can say\n"
        "what happened, based on a check made outside this file. Then:\n"
        "  %s resolve --step %d --outcome loaded    --evidence \"<how they checked>\"\n"
        "  %s resolve --step %d --outcome not-loaded --evidence \"<how they checked>\"\n"
        % (
            number,
            record.get("command") or "(command not recorded)",
            record.get("started_at") or "(unknown)",
            record.get("detail") or "the session ended before an exit status was recorded",
            run.state_path,
            number,
            run.prog,
            number,
            run.prog,
            number,
        )
    )


def render(run, state):
    out = []
    out.append('run       %s  (workspace %s)' % (run.run_id, run.workspace))
    out.append('runbook   %s  sha256 %s' % (run.runbook, state["runbook"]["sha256"]))
    out.append('steps     %d   updated %s' % (state["total_steps"], state.get("updated_at")))
    try:
        current = run.fingerprint()
    except SystemExit:
        current = None
    if current and current != state["runbook"]["sha256"]:
        out.append("WARNING   %s has changed since this run started (now %s)." % (run.runbook, current[:12]))
    unfinished = first_unfinished(state)
    out.append("next      %s" % ("none, every step is done" if unfinished is None else str(unfinished)))
    out.append("")
    out.append("  %-5s %-12s %-21s %-21s %s" % ("step", "status", "started", "finished", "detail"))
    blocked = []
    for number in range(1, state["total_steps"] + 1):
        record = step_record(state, number)
        detail = record.get("detail") or ""
        for resolution in record.get("resolutions", []):
            detail = "resolved %s (%s): %s" % (resolution["outcome"], resolution["at"], resolution["evidence"])
        out.append(
            "  %-5d %-12s %-21s %-21s %s"
            % (
                number,
                record["status"],
                record.get("started_at") or "-",
                record.get("finished_at") or "-",
                detail,
            )
        )
        if record["status"] in UNCONFIRMED:
            blocked.append((number, record))
    for number, record in blocked:
        out.append("")
        out.append(block_message(run, number, record))
    if unfinished is not None and step_record(state, unfinished)["status"] not in UNCONFIRMED:
        out.append("")
        out.append(
            "Next: read step %d's command from %s, then run it once through\n"
            "  %s begin --step %d --command \"<the exact command>\"\n"
            "before running it. begin writes 'pending' first, so a session that dies\n"
            "mid-step leaves the step unconfirmed rather than unrecorded." % (unfinished, run.runbook, run.prog, unfinished)
        )
    return "\n".join(out)


def cmd_init(run, args):
    if os.path.exists(run.state_path):
        die(
            "%s already exists. This run is already underway; init never overwrites it.\n"
            "Run: %s status" % (run.state_path, run.prog),
            2,
        )
    if args.steps < 1 or args.steps > MAX_TOTAL_STEPS:
        die("--steps must be between 1 and %d." % MAX_TOTAL_STEPS)
    state = {
        "schema_version": SCHEMA_VERSION,
        "run_id": run.run_id,
        "workspace": run.workspace,
        "created_at": now(),
        "updated_at": now(),
        "runbook": {"path": run.runbook, "sha256": run.fingerprint(), "total_steps": args.steps},
        "total_steps": args.steps,
        "steps": {},
    }
    os.makedirs(run.state_dir, exist_ok=True)
    with StateLock(run.lock_path, run.prog):
        if os.path.exists(run.state_path):
            die("%s already exists; nothing written." % run.state_path, 2)
        run.save(state)
    print("Recorded a new run of %s: %d steps, state at %s" % (run.runbook, args.steps, run.state_path))


def cmd_status(run, args):
    state = run.load()
    if args.json:
        print(json.dumps(state, indent=2))
    else:
        print(render(run, state))
    unfinished = first_unfinished(state)
    if unfinished is not None and step_record(state, unfinished)["status"] in UNCONFIRMED:
        sys.exit(2)
    return None


def cmd_begin(run, args):
    state = run.load()
    check_range(state, args.step)
    with StateLock(run.lock_path, run.prog):
        state = run.load()
        check_range(state, args.step)
        current = run.fingerprint()
        if current != state["runbook"]["sha256"]:
            die(
                "REFUSED: %s has changed since this run started (recorded %s, now %s).\n"
                "Step numbers no longer mean what they meant when the run began, so running\n"
                "them against this state could load the wrong batch. Do not reset the state\n"
                "on your own: report the change and let the user decide."
                % (run.runbook, state["runbook"]["sha256"][:12], current[:12]),
                2,
            )
        unfinished = first_unfinished(state)
        if unfinished is not None and unfinished != args.step:
            first = step_record(state, unfinished)
            detail = (
                block_message(run, unfinished, first)
                if first["status"] in UNCONFIRMED
                else "Step %d has not run yet, so step %d has to wait." % (unfinished, args.step)
            )
            die("REFUSED: steps run in order, and step %d is not done.\n%s" % (unfinished, detail), 2)
        record = step_record(state, args.step)
        status = record["status"]
        if status == DONE:
            die(
                "REFUSED: step %d is already recorded as done (exit 0, finished %s).\n"
                "Running it again loads its batch a second time. If this step genuinely\n"
                "must run again, that is a decision about the backfill, not this state:\n"
                "report it to the user instead of forcing it." % (args.step, record.get("finished_at")),
                2,
            )
        if status in UNCONFIRMED:
            die(block_message(run, args.step, record), 2)
        record.update(
            {
                "status": PENDING,
                "command": text(args.command, 4000),
                "started_at": now(),
                "finished_at": None,
                "detail": "",
            }
        )
        record["attempts"] = int(record.get("attempts", 0)) + 1
        state["steps"][str(args.step)] = record
        run.save(state)
    print("Step %d is pending. Run its command once, now, exactly as recorded:" % args.step)
    print("  %s" % args.command)
    print("Then record the result with succeed (exit 0) or fail (anything else).")


def cmd_succeed(run, args):
    state = run.load()
    check_range(state, args.step)
    with StateLock(run.lock_path, run.prog):
        state = run.load()
        record = step_record(state, args.step)
        if record["status"] == DONE:
            die("Step %d is already done; nothing to change." % args.step, 2)
        if record["status"] != PENDING:
            die(
                "REFUSED: step %d is %s, not pending.\nsucceed records that a command which had\n"
                "already started exited 0. It is not a way to mark a step done without\n"
                "running it." % (args.step, record["status"]),
                2,
            )
        record.update({"status": DONE, "finished_at": now(), "receipt": text(args.receipt)})
        state["steps"][str(args.step)] = record
        run.save(state)
    print("Step %d recorded as done. Its batch is loaded; do not run it again." % args.step)


def cmd_fail(run, args):
    state = run.load()
    check_range(state, args.step)
    with StateLock(run.lock_path, run.prog):
        state = run.load()
        record = step_record(state, args.step)
        if record["status"] == DONE:
            die("Step %d is already done; nothing to change." % args.step, 2)
        if record["status"] != PENDING:
            die("REFUSED: step %d is %s, not pending, so there is no attempt to fail."
                % (args.step, record["status"]), 2)
        detail = text(args.detail or "exit status %s" % args.exit_code)
        record.update({"status": FAILED, "finished_at": now(), "exit_code": args.exit_code, "detail": detail})
        state["steps"][str(args.step)] = record
        run.save(state)
    print(block_message(run, args.step, step_record(state, args.step)))


def cmd_resolve(run, args):
    state = run.load()
    check_range(state, args.step)
    with StateLock(run.lock_path, run.prog):
        state = run.load()
        record = step_record(state, args.step)
        if record["status"] not in UNCONFIRMED:
            die(
                "REFUSED: step %d is %s, so there is no unconfirmed attempt to resolve.\n"
                "resolve only records a determination about a step that may or may not have\n"
                "committed. It is not a way to mark an unrun step as done." % (args.step, record["status"]),
                2,
            )
        evidence = text(args.evidence)
        if not evidence:
            die("--evidence is required: say how the outcome was determined, not just what it was.")
        stamp = now()
        resolution = {"outcome": args.outcome, "evidence": evidence, "by": text(args.by, 200) or "user", "at": stamp}
        record.setdefault("resolutions", []).append(resolution)
        del record["resolutions"][:-MAX_RESOLUTIONS]
        record["detail"] = "resolved %s by %s: %s" % (args.outcome, resolution["by"], evidence)
        if args.outcome == "loaded":
            record["status"] = DONE
            record["finished_at"] = stamp
        else:
            record["status"] = NOT_STARTED
            record["finished_at"] = None
        state["steps"][str(args.step)] = record
        run.save(state)
    if args.outcome == "loaded":
        print("Step %d resolved as loaded: marked done and not run again." % args.step)
    else:
        print("Step %d resolved as not loaded: cleared for one more run." % args.step)


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    prog = "backfill_state.py"
    parser = argparse.ArgumentParser(
        prog=prog,
        description="Durable per-step state for a backfill runbook. Records what was observed; never runs a step.",
    )
    parser.add_argument("--workspace", default=".", help="directory holding runbook.md (default: .)")
    parser.add_argument("--runbook", default="runbook.md", help="runbook filename in the workspace (default: runbook.md)")
    parser.add_argument("--run", default=None, help="state file name (default: the runbook stem)")
    subparsers = parser.add_subparsers(dest="command")

    init = subparsers.add_parser("init", help="record the start of a new run; refuses to overwrite an existing one")
    init.add_argument("--steps", type=int, required=True, help="number of numbered steps in the runbook")
    init.set_defaults(handler=cmd_init)

    status = subparsers.add_parser("status", help="print where the run stands; exit 2 if a step is unconfirmed")
    status.add_argument("--json", action="store_true", help="print the raw state instead of the summary")
    status.set_defaults(handler=cmd_status)

    begin = subparsers.add_parser("begin", help="mark a step pending BEFORE its command runs")
    begin.add_argument("--step", type=int, required=True)
    begin.add_argument("--command", required=True, help="the exact command from the runbook, recorded verbatim")
    begin.set_defaults(handler=cmd_begin)

    succeed = subparsers.add_parser("succeed", help="mark a pending step done, after its command exited 0")
    succeed.add_argument("--step", type=int, required=True)
    succeed.add_argument("--receipt", default="", help="evidence it loaded: rows, bytes, log line")
    succeed.set_defaults(handler=cmd_succeed)

    fail = subparsers.add_parser("fail", help="record an error from a pending step; commit stays unknown")
    fail.add_argument("--step", type=int, required=True)
    fail.add_argument("--exit-code", type=int, default=1)
    fail.add_argument("--detail", default="", help="the error as observed")
    fail.set_defaults(handler=cmd_fail)

    resolve = subparsers.add_parser("resolve", help="record the user's determination of an unconfirmed step")
    resolve.add_argument("--step", type=int, required=True)
    resolve.add_argument("--outcome", required=True, choices=["loaded", "not-loaded"])
    resolve.add_argument("--evidence", required=True, help="how this was determined")
    resolve.add_argument("--by", default="", help="who determined it")
    resolve.set_defaults(handler=cmd_resolve)

    args = parser.parse_args(argv)
    if not args.command:
        parser.print_help()
        return 1
    run = Run(args, prog)
    args.handler(run, args)
    return 0


if __name__ == "__main__":
    sys.exit(main())