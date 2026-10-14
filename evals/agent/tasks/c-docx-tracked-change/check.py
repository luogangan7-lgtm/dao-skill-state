"""Pass: a w:del holding "30" and a w:ins holding "45", both by Reviewer, in the payment paragraph;
the accepted text reads "45 days" and the other paragraphs are unchanged."""
import os, sys, zipfile
from xml.etree import ElementTree as ET

p = os.path.join(sys.argv[1], "contract.docx")
if not os.path.exists(p):
    sys.exit("no contract.docx")
W = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"
root = ET.fromstring(zipfile.ZipFile(p).read("word/document.xml"))


def accepted(par):
    """Paragraph text with insertions kept and deletions dropped."""
    return "".join(t.text or "" for t in par.iter(W + "t"))


pars = [p for p in root.iter(W + "p")]
texts = [accepted(p) for p in pars]
for want in ("Services Agreement", "The Supplier provides the services described in Schedule A.",
             "Either party may end this agreement with written notice."):
    if want not in texts:
        sys.exit(f"paragraph changed or missing: {want!r}")
pay = [p for p, t in zip(pars, texts) if "Invoices are payable" in t]
if len(pay) != 1 or accepted(pay[0]) != "Invoices are payable within 45 days of receipt.":
    sys.exit(f"payment paragraph reads {[accepted(p) for p in pay]}")
dels = [d for d in pay[0].iter(W + "del") if d.get(W + "author") == "Reviewer"]
ins = [i for i in pay[0].iter(W + "ins") if i.get(W + "author") == "Reviewer"]
if not any("30" in "".join(t.text or "" for t in d.iter(W + "delText")) for d in dels):
    sys.exit("no tracked deletion of 30 by Reviewer")
if not any("45" in "".join(t.text or "" for t in i.iter(W + "t")) for i in ins):
    sys.exit("no tracked insertion of 45 by Reviewer")
print("ok")
