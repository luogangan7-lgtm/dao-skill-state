"""Pass: error_response matches the conventions for every kind (run in a subprocess)."""
import json, os, subprocess, sys

ws = sys.argv[1]
if not os.path.exists(os.path.join(ws, "errors.py")):
    sys.exit("no errors.py")
calls = [["validation", "email is required", {"X-Request-Id": "r-1"}],
         ["not_found", "order 7 not found", {}],
         ["duplicate", "sku exists", {"X-Request-Id": "r-3"}],
         ["forbidden", "admins only", {"X-Request-Id": "r-4"}],
         ["unexpected", "db timeout at node 3", {"X-Request-Id": "r-5"}]]
probe = ("import json,sys; from errors import error_response; "
         "print(json.dumps([list(error_response(*c)) for c in json.load(sys.stdin)]))")
r = subprocess.run([sys.executable, "-c", probe], cwd=ws, input=json.dumps(calls), capture_output=True,
                   text=True, timeout=60)
try:
    got = json.loads(r.stdout.strip().splitlines()[-1])
except (ValueError, IndexError):
    sys.exit("errors.py did not run")
want = [[422, {"error": {"code": "VALIDATION_FAILED", "message": "email is required", "request_id": "r-1"}}],
        [404, {"error": {"code": "NOT_FOUND", "message": "order 7 not found", "request_id": None}}],
        [409, {"error": {"code": "ALREADY_EXISTS", "message": "sku exists", "request_id": "r-3"}}],
        [403, {"error": {"code": "FORBIDDEN", "message": "admins only", "request_id": "r-4"}}],
        [500, {"error": {"code": "INTERNAL", "message": "Internal error", "request_id": "r-5"}}]]
for g, w_ in zip(got, want):
    if g != w_:
        sys.exit(f"got {g}, want {w_}")
print("ok")
