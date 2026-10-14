"""Pass: labels and inputs present; Profit and Total are live formulas that reference cells and
evaluate to the right values. Supports + - * /, SUM over ranges and lists, $ refs, lowercase,
and references to the same sheet by name (Q1!B2)."""
import os, re, sys
from openpyxl import load_workbook

p = os.path.join(sys.argv[1], "budget.xlsx")
if not os.path.exists(p):
    sys.exit("no budget.xlsx")
wb = load_workbook(p)
if "Q1" not in wb.sheetnames:
    sys.exit("no sheet Q1")
ws = wb["Q1"]
REF = r"(?:'?Q1'?!)?\$?([A-Za-z]{1,2})\$?(\d+)"


def val(ref, seen=()):
    v = ws[ref].value
    if v is None:
        return 0
    if isinstance(v, str) and v.startswith("="):
        if ref in seen:
            raise ValueError("cycle")
        return ev(v[1:], seen + (ref,))
    return v


def ev(expr, seen):
    def cells(a, b):
        (c1, r1), (c2, r2) = re.fullmatch(REF, a).groups(), re.fullmatch(REF, b).groups()
        return [f"{chr(c)}{r}" for c in range(ord(c1.upper()), ord(c2.upper()) + 1) for r in range(int(r1), int(r2) + 1)]

    def total(m):
        parts = []
        for arg in m.group(1).split(","):
            arg = arg.strip()
            if ":" in arg:
                a, b = arg.split(":")
                parts += [str(val(c, seen)) for c in cells(a.strip(), b.strip())]
            else:
                parts.append(f"({arg})")
        return "(" + "+".join(parts or ["0"]) + ")"
    e = re.sub(r"(?i)SUM\(([^()]*)\)", total, expr)
    e = re.sub(REF, lambda m: str(val(m.group(1).upper() + m.group(2), seen)), e)
    if not re.fullmatch(r"[0-9+\-*/(). ]+", e):
        raise ValueError(f"unsupported formula {expr!r}")
    return eval(e)


heads = [str(ws[c].value or "").strip().lower() for c in ("A1", "B1", "C1", "D1", "E1")]
if heads != ["item", "jan", "feb", "mar", "total"]:
    sys.exit(f"headers {heads}")
labels = [str(ws[c].value or "").strip().lower() for c in ("A2", "A3", "A4")]
if labels != ["revenue", "cost", "profit"]:
    sys.exit(f"row labels {labels}")
for ref, v in {"B2": 1200, "C2": 1350, "D2": 1500, "B3": 800, "C3": 820, "D3": 900}.items():
    if ws[ref].value != v:
        sys.exit(f"{ref} = {ws[ref].value!r}, want {v}")
for ref, v in {"B4": 400, "C4": 530, "D4": 600, "E2": 4050, "E3": 2520, "E4": 1530}.items():
    raw = ws[ref].value
    if not (isinstance(raw, str) and raw.startswith("=") and re.search(REF, raw[1:])):
        sys.exit(f"{ref} is not a formula over cells: {raw!r}")
    try:
        got = val(ref)
    except (ValueError, SyntaxError, AttributeError) as e:
        sys.exit(f"{ref}: {e}")
    if abs(got - v) > 1e-9:
        sys.exit(f"{ref} evaluates to {got}, want {v}")
print("ok")
