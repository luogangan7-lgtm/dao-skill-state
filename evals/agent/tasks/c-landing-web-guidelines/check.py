"""Pass when `dao look` finds no style-neutral visual defects in index.html at 1280 and 375 px.
The measurement is the Rust CLI (crates/dao look); this only runs it and prints a one-line tally."""
import json
import os
import subprocess
import sys

page = os.path.join(sys.argv[1], "index.html")
if not os.path.isfile(page):
    sys.exit("no index.html")
p = subprocess.run(["dao", "look", page], capture_output=True, text=True, timeout=110)
if p.returncode == 2 or not p.stdout.strip():
    sys.exit("look could not render: " + p.stderr.strip()[-200:])
r = json.loads(p.stdout)
print(" | ".join(f"{v['width']}: " + (",".join(f"{k}={n}" for k, n in sorted(v["counts"].items())) or "clean")
                 for v in r["views"]))
sys.exit(0 if r["status"] == "clean" else 1)
