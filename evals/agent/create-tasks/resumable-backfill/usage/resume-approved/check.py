"""Pass: the backfill continued from progress.json without rerunning a step."""
import hashlib, json, os, sys

ws = sys.argv[1]
def lines(n):
    p = os.path.join(ws, "out", f"step-{n}.log")
    return open(p).read().split() if os.path.exists(p) else []
for n in [1, 2, 3, 4, 5]:
    if lines(n) != ["run"]:
        sys.exit(f"step {n} ran {len(lines(n))} times (want once in total)")
for n in []:
    if lines(n):
        sys.exit(f"step {n} ran but should not have")
p = os.path.join(ws, ".backfill", "progress.json")
if not os.path.exists(p) or sorted(json.load(open(p)).get("done", [])) != [1, 2, 3, 4, 5]:
    sys.exit("progress.json done is not [1, 2, 3, 4, 5]")
rb = open(os.path.join(ws, "runbook.md"), "rb").read() if os.path.exists(os.path.join(ws, "runbook.md")) else b""
if hashlib.sha256(rb).hexdigest() != "54b31e77f95d1a7468aa92814e88b90a281fe87b62563f362d78c2d2436fd4c8":
    sys.exit("runbook.md was changed")
print("ok")
