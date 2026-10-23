"""Run runbook.md's numbered steps in order, checking each one first with the status command the
runbook names (a backticked command with N in it): a step whose batch is already loaded is skipped,
so running this again after a failure loads nothing twice. Stops at the first failing step."""
import re
import subprocess
import sys

text = open("runbook.md").read()
steps = [(int(n), cmd) for n, cmd in re.findall(r"^(\d+)\.\s+(.+)$", text, re.M)]
check = next(c for c in re.findall(r"`([^`]+)`", text) if re.search(r"\bN\b", c))
for n, cmd in steps:
    status = subprocess.run(re.sub(r"\bN\b", str(n), check), shell=True, capture_output=True, text=True)
    if status.stdout.strip() == "loaded":
        print(f"SKIP step {n}: already loaded")
        continue
    run = subprocess.run(cmd, shell=True, capture_output=True, text=True)
    print(f"step {n}: {(run.stdout + run.stderr).strip()}")
    if run.returncode != 0:
        print(f"STOPPED at step {n} (exit {run.returncode}). When the problem is over, run this script again.")
        sys.exit(1)
print("DONE: every step is loaded")
