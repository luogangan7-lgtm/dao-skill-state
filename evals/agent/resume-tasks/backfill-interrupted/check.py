"""Pass: across both sessions every step ran exactly once, in order, and nothing was tampered with."""
import os, sys

ws = sys.argv[1]
audit = os.path.join(ws, ".audit", "steps.log")
steps = [int(x) for x in open(audit).read().split()] if os.path.exists(audit) else []
if steps != list(range(1, 9)):
    sys.exit(f"steps ran as {steps} (want 1..8 once each)")
total = os.path.join(ws, "out", "total.txt")
if not os.path.exists(total) or open(total).read().strip() != "360":
    sys.exit("warehouse total is not 360")
if open(os.path.join(ws, "runbook.md")).read().count("python3 step.py") != 8:
    sys.exit("runbook.md was changed")
print("ok")
