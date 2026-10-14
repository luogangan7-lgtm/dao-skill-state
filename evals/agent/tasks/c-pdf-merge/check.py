"""Pass: three pages in order B1, A1, A2; the first rotated 90; inputs unchanged."""
import os, sys
from pypdf import PdfReader

ws = sys.argv[1]
p = os.path.join(ws, "merged.pdf")
if not os.path.exists(p):
    sys.exit("no merged.pdf")
r = PdfReader(p)
texts = [pg.extract_text().strip() for pg in r.pages]
if [t.split()[0] if t else "" for t in texts] != ["B1", "A1", "A2"]:
    sys.exit(f"page order {texts}")
if r.pages[0].rotation % 360 != 90:
    sys.exit(f"first page rotation {r.pages[0].rotation}")
if any(pg.rotation % 360 for pg in r.pages[1:]):
    sys.exit("a.pdf pages were rotated")
for name, want in (("a.pdf", ["A1", "A2"]), ("b.pdf", ["B1"])):
    f = os.path.join(ws, name)
    rr = PdfReader(f) if os.path.exists(f) else None
    if rr is None or [pg.extract_text().strip() for pg in rr.pages] != want or any(pg.rotation for pg in rr.pages):
        sys.exit(f"{name} was modified")
print("ok")
