"""Pass: the original tests are unchanged and pass, and hidden edge cases pass (run in a subprocess)."""
import hashlib, json, os, subprocess, sys

ws = sys.argv[1]
TESTS_SHA = "c2d11dd6ba466c8f73f0f9ce264f6fe9aab0b832c5bb4bad861c7d95f981b99b"
p = os.path.join(ws, "test_pages.py")
if not os.path.exists(p) or hashlib.sha256(open(p, "rb").read()).hexdigest() != TESTS_SHA:
    sys.exit("tests missing or changed")
r = subprocess.run([sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider"], cwd=ws, capture_output=True,
                   text=True, timeout=120)
if r.returncode != 0:
    sys.exit("tests fail")
probe = ("import json; from pages import page_items, page_count; items = list(range(1, 24)); "
         "print(json.dumps([page_items(items, p, 10) for p in (1, 3, 4, 0)] + "
         "[page_count(23, 10), page_count(0, 10), page_count(20, 10)]))")
r = subprocess.run([sys.executable, "-c", probe], cwd=ws, capture_output=True, text=True, timeout=60)
try:
    got = json.loads(r.stdout.strip().splitlines()[-1])
except (ValueError, IndexError):
    sys.exit("pages.py did not run")
want = [list(range(1, 11)), [21, 22, 23], [], [], 3, 0, 2]
if got != want:
    sys.exit(f"hidden cases {got}")
print("ok")
