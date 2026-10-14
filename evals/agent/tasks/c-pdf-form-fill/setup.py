"""application.pdf: an AcroForm with text fields full_name, date and a checkbox agree."""
from reportlab.pdfgen import canvas

c = canvas.Canvas("application.pdf")
c.drawString(72, 760, "Application")
c.drawString(72, 720, "Full name:")
c.acroForm.textfield(name="full_name", x=160, y=712, width=240, height=20)
c.drawString(72, 680, "Date:")
c.acroForm.textfield(name="date", x=160, y=672, width=120, height=20)
c.drawString(72, 640, "I agree:")
c.acroForm.checkbox(name="agree", x=160, y=636, size=16)
c.showPage()
c.save()
