"""Pass: three pages in order B1, A1, A2; the first rotated 90; inputs unchanged."""
import hashlib, json, os, sys
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
digests = json.load(open(os.path.join(ws, ".inputs.json")))
for f, h in digests.items():
    if hashlib.sha256(open(os.path.join(ws, f), "rb").read()).hexdigest() != h:
        sys.exit(f"{f} was modified")
print("ok")
