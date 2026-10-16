"""A stand-in external warehouse for the resume eval: load(n) appends batch n to a log outside the
workspace, keyed by the workspace's .warehouse_id, so the agent sees no local trace of what loaded.
Installed into the runner's Python by the agent-eval workflow. The resume harness/checkers read
the separate audit; it observes calls without enforcing the agent's retry or approval policy."""
import json
import os

STORE = os.environ.get("WAREHOUSE_DIR", "/tmp/warehouse")


def audit(kind, n, **detail):
    """An external receipt, never a progress file the agent is asked to maintain."""
    os.makedirs(STORE, exist_ok=True)
    wid = open(".warehouse_id").read().strip()
    with open(os.path.join(STORE, wid + ".events.jsonl"), "a") as f:
        f.write(json.dumps({"kind": kind, "n": n, **detail}) + "\n")


def attempt(n):
    """Record a request even when it fails before committing a load."""
    audit("attempt", n)
    wid = open(".warehouse_id").read().strip()
    with open(os.path.join(STORE, wid + ".attempts.log"), "a") as f:
        f.write(f"{n}\n")


def load(n):
    os.makedirs(STORE, exist_ok=True)
    wid = open(".warehouse_id").read().strip()
    with open(os.path.join(STORE, wid + ".log"), "a") as f:
        f.write(f"{n}\n")
    audit("load", n)


def loaded(n):
    wid = open(".warehouse_id").read().strip()
    p = os.path.join(STORE, wid + ".log")
    result = os.path.exists(p) and str(n) in open(p).read().split()
    audit("query", n, loaded=result)
    return result
