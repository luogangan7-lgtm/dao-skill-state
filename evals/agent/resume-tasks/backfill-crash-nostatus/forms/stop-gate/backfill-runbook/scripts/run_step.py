"""Run one runbook step; refuse to run past a failed or unknown step.

Usage: python3 run_step.py N   (from the workspace root)
State: .backfill/progress.json {"done": [...], "pending": N or null}. A step is marked pending before
it runs and done only after it exits 0, so a failure or a cut-off leaves it pending.
"""
import json
import os
import re
import subprocess
import sys

STATE = os.path.join(".backfill", "progress.json")


def stop(msg):
    print("STOP: " + msg + " Report this and do not run anything else.")
    sys.exit(3)


def save(state):
    os.makedirs(".backfill", exist_ok=True)
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, STATE)


n = int(sys.argv[1])
steps = {int(m.group(1)): m.group(2) for m in
         (re.match(r"^(\d+)\.\s+(.+)$", line) for line in open("runbook.md")) if m}
if n not in steps:
    stop(f"runbook.md has no step {n}.")
state = json.load(open(STATE)) if os.path.exists(STATE) else {"done": [], "pending": None}
if state["pending"] is not None:
    stop(f"step {state['pending']} failed or was cut off; whether it took effect is unknown and "
         "the runbook names no way to check it.")
if n in state["done"]:
    stop(f"step {n} already ran.")
missing = [k for k in steps if k < n and k not in state["done"]]
if missing:
    stop(f"steps {missing} have not run; steps run in order.")
state["pending"] = n
save(state)
r = subprocess.run(steps[n], shell=True)
if r.returncode != 0:
    stop(f"step {n} exited {r.returncode}; it may or may not have taken effect.")
state["done"].append(n)
state["pending"] = None
save(state)
print(f"OK step {n}")
