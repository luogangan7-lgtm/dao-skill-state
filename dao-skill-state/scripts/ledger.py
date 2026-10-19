#!/usr/bin/env python3
"""Step ledger for a skill whose run can be cut off: never repeat a step that changes an external system.

Copy this file into the created skill (its scripts/). Wrap every step that changes something outside
the workspace (a load, a send, a payment, a deploy):

  ledger.py STATE begin STEP                  before the step
      exit 0: run the step now
      exit 3: already done, skip it
      exit 4: pending from a run that was cut off; it may or may not have taken effect
  ledger.py STATE done STEP                   after the step succeeded
  ledger.py STATE resolve STEP done|not-done  after asking the real system about a pending step
  ledger.py STATE show

On exit 4 ask the authoritative system whether STEP took effect and resolve it; if the system cannot
tell, stop and report STEP to the user instead of running it. A step that failed stays pending.
STATE is a JSON file outside the skill directory. Used by: skills created with dao-skill-state.

Paste this block into the created SKILL.md's workflow as is (fill in the two paths):

  For every step that changes an external system, in order:
  1. Run `python3 <skill-dir>/scripts/ledger.py <state-file> begin <step>`.
     - exit 3: the step is done; go to the next step.
     - exit 4: the step was cut off earlier. Ask the system whether it took effect and run
       `... resolve <step> done` or `... resolve <step> not-done`; if the system cannot tell,
       stop and report the step to the user. Never run it again unchecked.
     - exit 0: run the step.
  2. If the step succeeded, run `python3 <skill-dir>/scripts/ledger.py <state-file> done <step>`.
     If it failed, stop and report it; it stays pending.
  Do not track progress any other way.
"""
import json
import os
import sys

USAGE = "usage: ledger.py STATE begin|done STEP | resolve STEP done|not-done | show"


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        return {}


def save(path, state):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f, indent=1, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)  # a crash leaves the old or the new file, never half of one


def main(argv):
    if len(argv) < 2:
        print(USAGE)
        return 2
    path, cmd, args = argv[0], argv[1], argv[2:]
    state = load(path)
    if cmd == "show":
        print(json.dumps(state, indent=1, sort_keys=True))
        return 0
    if not args:
        print(USAGE)
        return 2
    step, cur = args[0], state.get(args[0])
    if cmd == "begin" and len(args) == 1:
        if cur == "done":
            print(f"{step}: done, skip")
            return 3
        if cur == "pending":
            print(f"{step}: pending from an earlier run. Ask the system whether it took effect, then "
                  f"resolve it; if the system cannot tell, stop and report {step}")
            return 4
        state[step] = "pending"
        save(path, state)
        return 0
    if cmd == "done" and len(args) == 1:
        if cur != "pending":
            print(f"{step}: not begun")
            return 2
        state[step] = "done"
        save(path, state)
        return 0
    if cmd == "resolve" and len(args) == 2 and args[1] in ("done", "not-done"):
        if cur != "pending":
            print(f"{step}: not pending")
            return 2
        if args[1] == "done":
            state[step] = "done"
        else:
            del state[step]
        save(path, state)
        return 0
    print(USAGE)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
