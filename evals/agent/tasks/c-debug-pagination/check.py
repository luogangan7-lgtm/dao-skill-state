"""Pass: the original tests are unchanged and pass, and hidden edge cases pass."""
import hashlib, importlib.util, os, subprocess, sys

ws = sys.argv[1]
if hashlib.sha256(open(os.path.join(ws, "test_pages.py"), "rb").read()).hexdigest() != open(os.path.join(ws, ".tests.sha")).read().strip():
    sys.exit("tests were changed")
r = subprocess.run([sys.executable, "-m", "pytest", "-q"], cwd=ws, capture_output=True, text=True)
if r.returncode != 0:
    sys.exit("tests fail")
spec = importlib.util.spec_from_file_location("pages", os.path.join(ws, "pages.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
items = list(range(1, 24))
for page, want in ((1, list(range(1, 11))), (3, [21, 22, 23]), (4, []), (0, [])):
    if m.page_items(items, page, 10) != want:
        sys.exit(f"page {page}: {m.page_items(items, page, 10)}")
if m.page_count(23, 10) != 3 or m.page_count(0, 10) != 0 or m.page_count(20, 10) != 2:
    sys.exit("page_count")
print("ok")
