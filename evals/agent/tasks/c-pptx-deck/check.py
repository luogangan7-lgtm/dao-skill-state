"""Pass: three slides with those titles; Results body has exactly the three points."""
import os, sys
from pptx import Presentation

p = os.path.join(sys.argv[1], "deck.pptx")
if not os.path.exists(p):
    sys.exit("no deck.pptx")
pr = Presentation(p)
titles = [(s.shapes.title.text.strip() if s.shapes.title is not None else "") for s in pr.slides]
if titles != ["Agenda", "Results", "Next Steps"]:
    sys.exit(f"titles {titles}")
s = pr.slides[1]
paras = [pa.text.strip() for sh in s.shapes if sh.has_text_frame and sh != s.shapes.title
         for pa in sh.text_frame.paragraphs if pa.text.strip()]
if paras != ["Revenue up 12%", "Churn down 3%", "NPS 41"]:
    sys.exit(f"Results body {paras}")
print("ok")
