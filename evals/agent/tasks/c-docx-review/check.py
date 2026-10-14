"""Pass: heading style, the 3x3 table, and a two-item bulleted list in a list style."""
import os, sys
from docx import Document

p = os.path.join(sys.argv[1], "report.docx")
if not os.path.exists(p):
    sys.exit("no report.docx")
d = Document(p)
heads = [x for x in d.paragraphs if x.text.strip() == "Quarterly Review"]
if not heads or not (heads[0].style.name.startswith("Heading") or heads[0].style.name == "Title"):
    sys.exit("heading missing or not a heading style")
want = [["Region", "Q1", "Q2"], ["North", "10", "12"], ["South", "8", "9"]]
if not any([[c.text.strip() for c in r.cells] for r in t.rows] == want for t in d.tables):
    sys.exit("table missing or different")
def is_bullet(par):
    if "Bullet" in par.style.name:
        return True
    num = par._p.pPr.numPr if par._p.pPr is not None else None
    if num is None or num.numId is None:
        return False
    try:
        root = d.part.numbering_part.element
    except (KeyError, NotImplementedError):
        return False
    ns = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"
    for n in root.findall(f"{ns}num"):
        if n.get(f"{ns}numId") == str(num.numId.val):
            aid = n.find(f"{ns}abstractNumId").get(f"{ns}val")
            for a in root.findall(f"{ns}abstractNum"):
                if a.get(f"{ns}abstractNumId") == aid:
                    lvl = a.find(f"{ns}lvl")
                    fmt = lvl.find(f"{ns}numFmt") if lvl is not None else None
                    return fmt is not None and fmt.get(f"{ns}val") == "bullet"
    return False


bullets = [x.text.strip() for x in d.paragraphs if x.text.strip() and is_bullet(x)]
if bullets != ["Hire two engineers", "Ship the mobile app"]:
    sys.exit(f"bulleted items {bullets}")
print("ok")
