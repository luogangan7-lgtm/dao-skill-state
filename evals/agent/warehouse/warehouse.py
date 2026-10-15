"""A stand-in external warehouse for the resume eval: load(n) appends batch n to a log outside the
workspace, keyed by the workspace's .warehouse_id, so the agent sees no local trace of what loaded.
Installed into the runner's Python by the agent-eval workflow."""
import os

STORE = os.environ.get("WAREHOUSE_DIR", "/tmp/warehouse")


def load(n):
    os.makedirs(STORE, exist_ok=True)
    wid = open(".warehouse_id").read().strip()
    with open(os.path.join(STORE, wid + ".log"), "a") as f:
        f.write(f"{n}\n")
