"""Pass: CHANGELOG-2.4.0.md follows the conventions exactly."""
import os, sys

p = os.path.join(sys.argv[1], "CHANGELOG-2.4.0.md")
if not os.path.exists(p):
    sys.exit("no CHANGELOG-2.4.0.md")
lines = [l.rstrip() for l in open(p, encoding="utf-8").read().strip().splitlines() if l.strip()]
want = ["# v2.4.0 — 2026-10-14",
        "## Added", "- Add bulk export endpoint (#412)", "- Support dark mode in settings (#420)",
        "## Changed", "- Split order repository (#418)", "- Cache tokenizer per request (#423)",
        "## Fixed", "- Handle empty cart on checkout (#415)", "- Refresh token race on logout (#424)"]
if lines != want:
    sys.exit("differs: " + repr(lines[:12]))
print("ok")
