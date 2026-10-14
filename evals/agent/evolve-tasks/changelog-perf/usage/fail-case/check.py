"""Pass: CHANGELOG-3.1.0.md matches the team's changelog conventions exactly."""
import os, sys

p = os.path.join(sys.argv[1], "CHANGELOG-3.1.0.md")
if not os.path.exists(p):
    sys.exit("no CHANGELOG-3.1.0.md")
raw = open(p, encoding="utf-8").read()
lines = [l.rstrip() for l in raw.strip().splitlines() if l.strip()]
want = ['# v3.1.0 — 2026-09-30', '## Added', '- Add audit log export (#501)', '## Changed', '- Batch database writes (#503)', '## Fixed', '- Correct currency rounding (#504)']
if lines != want:
    sys.exit("changelog does not match the conventions")
print("ok")
