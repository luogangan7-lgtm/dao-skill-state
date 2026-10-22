#!/usr/bin/env python3
"""Run a data-backfill runbook from start to finish, safely across sessions.

The same command starts a run and continues one. For every step in order it asks
the warehouse whether that step's batch is already loaded, skips the steps that
are, runs the ones that are not, and stops at the first step that fails or cannot
be checked -- without retrying it and without changing anything.

It keeps no state of its own. The warehouse is the only record of what took
effect, so there is no local progress file to go stale and send a finished step
round a second time.

Check command contract, one per step, in the same order as the steps:

    exit 0        the batch is loaded      -> skip the step
    exit 1        the batch is not loaded  -> run the step
    exit >= 2     the answer is unknown    -> do not run the step, stop

Exit status: 0 when every step is done, 1 when the run stopped with an outcome it
could not confirm, 2 on a usage or workspace error.
"""

import argparse
import os
import queue
import signal
import subprocess
import sys
import threading
import time

TAIL_LINES = 40
TIMEOUT_EXIT = 124
INTERRUPT_EXIT = 130


def _tail(bucket, line):
    bucket.append(line)
    if len(bucket) > TAIL_LINES:
        del bucket[: len(bucket) - TAIL_LINES]


def _print_tail(lines, indent="    "):
    for line in lines:
        sys.stdout.write(indent + line + "\n")
    sys.stdout.flush()


def run_command(command, workspace, timeout, log, stream):
    """Run one shell command in workspace.

    Returns (exit code, last TAIL_LINES of combined output). A command killed for
    running past its timeout reports TIMEOUT_EXIT, which is deliberately not 0 or
    1 so that neither the loaded nor the not-loaded branch can claim it.

    Output is drained on a thread so the deadline is enforced on the wall clock:
    a loader that prints nothing for hours is the case a timeout is for, and
    waiting for its next line of output to notice the deadline would never catch
    it.
    """
    tail = []
    deadline = time.time() + timeout if timeout else None
    try:
        proc = subprocess.Popen(
            command,
            shell=True,
            cwd=workspace,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            # Its own session, so a timeout can kill the loaders the step
            # started too. Killing only the shell would leave them writing into
            # the pipe, and still loading, while we reported the step stopped.
            start_new_session=True,
        )
    except OSError as exc:
        return 127, ["could not start command: %s" % exc]

    def kill_group():
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (OSError, AttributeError):
            proc.kill()

    lines = queue.Queue()

    def pump():
        try:
            for line in proc.stdout:
                lines.put(line.rstrip("\n"))
        finally:
            lines.put(None)

    threading.Thread(target=pump, daemon=True).start()

    killed = False
    try:
        while True:
            wait = 0.2
            if deadline is not None:
                wait = min(wait, max(0.0, deadline - time.time()))
            try:
                line = lines.get(timeout=wait)
            except queue.Empty:
                if deadline is not None and not killed and time.time() >= deadline:
                    killed = True
                    kill_group()
                    break
                continue
            if line is None:
                break
            _tail(tail, line)
            if stream:
                sys.stdout.write(line + "\n")
                sys.stdout.flush()
            if log is not None:
                log.write(line + "\n")
                log.flush()
    except KeyboardInterrupt:
        kill_group()
        proc.wait()
        _tail(tail, "[runner] interrupted: the step and everything it started were killed")
        return INTERRUPT_EXIT, tail

    try:
        code = proc.wait(timeout=5)
    except subprocess.TimeoutExpired:  # pragma: no cover
        kill_group()
        code = proc.wait()
    if killed:
        _tail(tail, "[runner] killed after exceeding its %.0fs timeout" % timeout)
        return TIMEOUT_EXIT, tail
    return code, tail


def _not_started(number, total):
    """The report clause naming what the run did not get to."""
    rest = number + 1
    if rest > total:
        return "there were no further steps"
    if rest == total:
        return "step %d was not started" % rest
    return "steps %d-%d were not started" % (rest, total)


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Run a backfill runbook, checking the warehouse before every step.",
    )
    parser.add_argument(
        "--step", action="append", required=True, metavar="CMD",
        help="a runbook step, exactly as the runbook writes it (repeat, in runbook order)",
    )
    parser.add_argument(
        "--check", action="append", required=True, metavar="CMD",
        help="command that exits 0 if that step's batch is loaded, 1 if not, "
             ">=2 if it cannot tell (repeat, paired with --step in the same order)",
    )
    parser.add_argument(
        "--workspace", default=os.getcwd(), metavar="DIR",
        help="directory the steps and checks run in (default: the current directory)",
    )
    parser.add_argument(
        "--log", metavar="FILE",
        help="append the full output of this run to FILE, for a later session to read",
    )
    parser.add_argument(
        "--check-timeout", type=float, default=600.0, metavar="SEC",
        help="kill a check that runs longer than SEC seconds (default: 600)",
    )
    parser.add_argument(
        "--step-timeout", type=float, default=0.0, metavar="SEC",
        help="kill a step that runs longer than SEC seconds (default: no limit)",
    )
    args = parser.parse_args(argv)

    steps, checks = args.step, args.check
    if len(steps) != len(checks):
        parser.error(
            "--step was given %d time(s) but --check %d time(s): every step needs its own "
            "check, in the same order." % (len(steps), len(checks))
        )

    workspace = os.path.abspath(args.workspace)
    if not os.path.isdir(workspace):
        sys.stderr.write("run_backfill: not a directory: %s\n" % workspace)
        return 2

    try:
        sys.stdout.reconfigure(line_buffering=True)
    except AttributeError:  # pragma: no cover - very old stdout
        pass

    log = None
    if args.log:
        log_path = args.log if os.path.isabs(args.log) else os.path.join(workspace, args.log)
        parent = os.path.dirname(log_path)
        if parent:
            os.makedirs(parent, exist_ok=True)
        log = open(log_path, "a", encoding="utf-8")
        log.write("\n=== run_backfill.py at %s, %d step(s), in %s\n" % (
            time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), len(steps), workspace))

    total = len(steps)
    print("run_backfill: %d step(s), workspace %s" % (total, workspace))
    ran = skipped = 0
    try:
        for number, (step, check) in enumerate(zip(steps, checks), start=1):
            # Ask the warehouse first, for every step, every time. This is the
            # only thing standing between a cut-off run and a duplicated load.
            code, out = run_command(check, workspace, args.check_timeout, log, stream=False)

            if code == 0:
                skipped += 1
                print("[%d/%d] already loaded, skipping: %s" % (number, total, step))
                continue

            if code != 1:
                print("[%d/%d] STOPPED: the check could not say whether this batch is loaded "
                      "(exit %d).\n  step:  %s\n  check: %s"
                      % (number, total, code, step, check))
                _print_tail(out)
                print("Nothing was run for this step, and %s. "
                      "Nothing was retried and nothing was changed."
                      % _not_started(number, total))
                return 1

            print("[%d/%d] not loaded, running: %s" % (number, total, step))
            code, out = run_command(step, workspace, args.step_timeout, log, stream=True)

            if code != 0:
                print("\n[%d/%d] FAILED (exit %d).\n  step:  %s\n  check: %s"
                      % (number, total, code, step, check))
                _print_tail(out)
                print("\nWhether this batch is loaded is UNKNOWN: a step can fail after its load "
                      "has committed. This step was not retried, and %s."
                      % _not_started(number, total))
                print("Resolve it by running this same command again. The check for every step, "
                      "this one first, decides what is still to run. The failure being explained "
                      "does not show whether the batch is loaded.")
                return 1

            ran += 1
            print("[%d/%d] done: %s" % (number, total, step))

        print("run_backfill: complete - %d step(s) run, %d already loaded, %d total."
              % (ran, skipped, total))
        return 0
    finally:
        if log is not None:
            log.close()


if __name__ == "__main__":
    sys.exit(main())