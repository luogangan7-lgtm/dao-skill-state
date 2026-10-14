"""Pass: hidden cases pass and the agent's own pytest file has at least 3 passing tests.
Agent code runs in a subprocess, so it cannot end the checker with a fake success."""
import json, os, subprocess, sys

ws = sys.argv[1]
if not os.path.exists(os.path.join(ws, "slug.py")):
    sys.exit("no slug.py")
cases = {"Crème Brûlée": "creme-brulee", "  Hello,   World!  ": "hello-world", "a--b__c": "a-b-c",
         "Ünïcödé 2024": "unicode-2024", "!!!": "", "": "", "Already-slug": "already-slug", "Ça va?": "ca-va"}
probe = "import json,sys; from slug import slugify; print(json.dumps([slugify(x) for x in json.load(sys.stdin)]))"
r = subprocess.run([sys.executable, "-c", probe], cwd=ws, input=json.dumps(list(cases)), capture_output=True,
                   text=True, timeout=60)
try:
    got = json.loads(r.stdout.strip().splitlines()[-1])
except (ValueError, IndexError):
    sys.exit("slugify did not run")
for (i, o), g in zip(cases.items(), got):
    if g != o:
        sys.exit(f"slugify({i!r}) = {g!r}, want {o!r}")
if not os.path.exists(os.path.join(ws, "test_slug.py")):
    sys.exit("no test_slug.py")
r = subprocess.run([sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider", "test_slug.py"], cwd=ws,
                   capture_output=True, text=True, timeout=120)
n = [int(w) for w, nx in zip(r.stdout.split(), r.stdout.split()[1:]) if w.isdigit() and nx.startswith("passed")]
if r.returncode != 0 or not n or n[-1] < 3:
    sys.exit("own tests missing, failing or fewer than 3")
print("ok")
