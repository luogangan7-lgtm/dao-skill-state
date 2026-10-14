"""Pass: CHANGELOG-3.2.0.md matches the team's changelog conventions exactly."""
import os, sys

p = os.path.join(sys.argv[1], "CHANGELOG-3.2.0.md")
if not os.path.exists(p):
    sys.exit("no CHANGELOG-3.2.0.md")
raw = open(p, encoding="utf-8").read()
lines = [l.rstrip() for l in raw.strip().splitlines() if l.strip()]
want = ['# v3.2.0 — 2026-10-21', '## Changed', '- Lazy-load avatars (#520)', '- Simplify retry loop (#522)', '- Add index on orders.created_at (#525)', '## Fixed', '- Avoid double submit (#523)']
if lines != want:
    sys.exit("changelog does not match the conventions")
print("ok")
