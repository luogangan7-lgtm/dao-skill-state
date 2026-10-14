"""Create a.pdf (pages A1, A2) and b.pdf (page B1) in the workspace."""
from reportlab.pdfgen import canvas

for name, pages in (("a.pdf", ["A1", "A2"]), ("b.pdf", ["B1"])):
    c = canvas.Canvas(name)
    for t in pages:
        c.drawString(100, 700, t)
        c.showPage()
    c.save()
