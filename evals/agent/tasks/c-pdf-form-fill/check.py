"""Pass: filled.pdf still has the three form fields with the requested values; agree is ticked."""
import os, sys
from pypdf import PdfReader

p = os.path.join(sys.argv[1], "filled.pdf")
if not os.path.exists(p):
    sys.exit("no filled.pdf")
f = PdfReader(p).get_fields() or {}
v = {k: (x.get("/V") if isinstance(x, dict) else None) for k, x in f.items()}
if str(v.get("full_name")) != "Jane Doe" or str(v.get("date")) != "2026-10-14":
    sys.exit(f"text fields {v.get('full_name')!r} {v.get('date')!r}")
if str(v.get("agree")) in ("None", "/Off", "Off", ""):
    sys.exit(f"agree not ticked: {v.get('agree')!r}")
print("ok")
