"""A stand-in external warehouse for the resume eval. The harness (resume.py) serves it on localhost
and keeps what loaded in its own memory, so an agent can load batches but cannot read the store.
loaded(n) answers only where the task's runbook names a status query; audit() needs the harness's
token, which the agent's environment does not carry. Installed into the runner's Python by the
agent-eval workflow; the workspace's .warehouse_id names the run."""
import json
import urllib.error
import urllib.parse
import urllib.request

URL = "http://127.0.0.1:47123"


def _get(path, **q):
    try:
        with urllib.request.urlopen(f"{URL}/{path}?{urllib.parse.urlencode(q)}", timeout=10) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"warehouse refused {path}: {e.reason}") from None


def _wid():
    return open(".warehouse_id").read().strip()


def load(n):
    _get("load", wid=_wid(), n=n)


def loaded(n):
    return _get("loaded", wid=_wid(), n=n)


def audit(wid, token):
    """Batches loaded for run wid, in load order (the harness and the task checkers only)."""
    return _get("audit", wid=wid, token=token)
