"""Pass: hidden cases pass and the agent's own pytest file exists with at least 3 passing tests."""
import importlib.util, os, subprocess, sys

ws = sys.argv[1]
if not os.path.exists(os.path.join(ws, "slug.py")):
    sys.exit("no slug.py")
spec = importlib.util.spec_from_file_location("slug", os.path.join(ws, "slug.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
cases = {"Crème Brûlée": "creme-brulee", "  Hello,   World!  ": "hello-world", "a--b__c": "a-b-c",
         "Ünïcödé 2024": "unicode-2024", "!!!": "", "": "", "Already-slug": "already-slug", "Ça va?": "ca-va"}
for i, o in cases.items():
    if m.slugify(i) != o:
        sys.exit(f"slugify({i!r}) = {m.slugify(i)!r}, want {o!r}")
if not os.path.exists(os.path.join(ws, "test_slug.py")):
    sys.exit("no test_slug.py")
r = subprocess.run([sys.executable, "-m", "pytest", "-q", "test_slug.py"], cwd=ws, capture_output=True, text=True)
passed = [w for w in r.stdout.split() if w.isdigit()]
if r.returncode != 0 or " passed" not in r.stdout or int(r.stdout.split(" passed")[0].split()[-1]) < 3:
    sys.exit("own tests missing, failing or fewer than 3")
print("ok")
