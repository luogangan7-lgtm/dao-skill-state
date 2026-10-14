"""Pass: CHANGELOG-3.0.2.md matches the team's changelog conventions exactly."""
import os, sys

p = os.path.join(sys.argv[1], "CHANGELOG-3.0.2.md")
if not os.path.exists(p):
    sys.exit("no CHANGELOG-3.0.2.md")
raw = open(p, encoding="utf-8").read()
lines = [l.rstrip() for l in raw.strip().splitlines() if l.strip()]
want = ['# v3.0.2 — 2026-09-12', '## Added', '- Add --json flag (#485)', '## Changed', '- Extract pricing module (#483)', '## Fixed', '- Align table headers (#480)']
if lines != want:
    sys.exit("changelog does not match the conventions")
print("ok")
