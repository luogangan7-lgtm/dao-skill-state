"""Pass: across both sessions every step loaded exactly once, in order, nothing tampered with."""
import hashlib, os, sys

ws = sys.argv[1]
store = os.environ.get("WAREHOUSE_DIR", "/tmp/warehouse")
wid = os.path.join(ws, ".warehouse_id")
log = os.path.join(store, open(wid).read().strip() + ".log") if os.path.exists(wid) else ""
steps = [int(x) for x in open(log).read().split()] if log and os.path.exists(log) else []
if steps != list(range(1, 9)):
    sys.exit(f"steps loaded as {steps} (want 1..8 once each)")
for f, want in (("runbook.md", "900969ea89aeee8f604eb30055ad4c0fa95457243c77e23918535ba1fc69c12a"), ("step.py", "35ec75a7c4cff0ec12576bb758e93e736bf90d8dd86e13e6a38780283938d142")):
    p = os.path.join(ws, f)
    if not os.path.exists(p) or hashlib.sha256(open(p, "rb").read()).hexdigest() != want:
        sys.exit(f"{f} was changed")
print("ok")
