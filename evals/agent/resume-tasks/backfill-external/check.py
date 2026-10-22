"""Pass: across both sessions every step loaded exactly once, in order, nothing tampered with."""
import hashlib, os, sys

ws = sys.argv[1]
wid = os.path.join(ws, ".warehouse_id")
steps = []
if os.path.exists(wid):
    import warehouse  # the stand-in warehouse; resume.py serves it and gives checkers the audit token
    steps = warehouse.audit(open(wid).read().strip(), os.environ.get("WAREHOUSE_AUDIT", ""))
if steps != list(range(1, 9)):
    sys.exit(f"steps loaded as {steps} (want 1..8 once each)")
for f, want in (("runbook.md", "0473343231743497c1f6329b2e8bd207723c871b3ce80c21f4a9c053a2f70887"), ("step.py", "7e002d2da7e9184c7dd8f523252f75b23c4dbe12f0a2a70e78f37e78a604b369")):
    p = os.path.join(ws, f)
    if not os.path.exists(p) or hashlib.sha256(open(p, "rb").read()).hexdigest() != want:
        sys.exit(f"{f} was changed")
print("ok")
