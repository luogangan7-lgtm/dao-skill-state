#!/usr/bin/env python3
"""Step gate for a stateful skill: copy it into the skill's scripts/ and adapt the state path.

  step_gate.py run ID -- COMMAND...    mark ID pending, run COMMAND, mark ID done if it exits 0
  step_gate.py resolve ID done|retry   after checking the system: ID took effect, or a retry is allowed
  step_gate.py status                  print the state

A step that failed or was cut off stays pending and blocks every later run until it is resolved.
State: $STEP_GATE_STATE (default .state/steps.json), {"done": [...], "pending": ID or null}.
Exit codes: 0 ran or skipped, 2 usage, 3 stop.
"""
import json
import os
import subprocess
import sys

STATE = os.environ.get("STEP_GATE_STATE", os.path.join(".state", "steps.json"))
HOW = ("Do not get past this by running the step's command yourself, editing the state file, or reading the "
       "system's internal storage instead of a check it offers. That the cause of a failure is gone does not "
       "show whether the step took effect. If the system offers a check, use it and then run "
       "`step_gate.py resolve ID done` (it took effect) or `step_gate.py resolve ID retry` (it did not, and "
       "the task allows a retry). If it cannot be checked, report which step needs checking and end your turn.")


def load():
    if not os.path.exists(STATE):
        return {"done": [], "pending": None}
    with open(STATE) as f:
        return json.load(f)


def save(state):
    os.makedirs(os.path.dirname(STATE) or ".", exist_ok=True)
    with open(STATE + ".tmp", "w") as f:
        json.dump(state, f)
    os.replace(STATE + ".tmp", STATE)  # atomic: a cut-off never leaves a half-written state


def stop(msg):
    print(f"STOP: {msg}\n{HOW}")
    sys.exit(3)


def main(argv):
    state = load()
    if argv == ["status"]:
        print(json.dumps(state))
    elif len(argv) == 3 and argv[0] == "resolve" and argv[2] in ("done", "retry"):
        if state["pending"] != argv[1]:
            sys.exit(f"step {argv[1]} is not pending; nothing to resolve")
        if argv[2] == "done":
            state["done"].append(argv[1])
        state["pending"] = None
        save(state)
        print(f"resolved step {argv[1]}: {argv[2]}")
    elif len(argv) > 3 and argv[0] == "run" and argv[2] == "--":
        step, command = argv[1], argv[3:]
        if state["pending"] is not None:
            stop(f"step {state['pending']} failed or was cut off; whether it took effect is unknown.")
        if step in state["done"]:
            print(f"SKIP step {step}: already done")
            return
        state["pending"] = step
        save(state)
        code = subprocess.run(command).returncode
        if code != 0:
            stop(f"step {step} exited {code}; it may or may not have taken effect.")
        state["done"].append(step)
        state["pending"] = None
        save(state)
        print(f"OK step {step}")
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
