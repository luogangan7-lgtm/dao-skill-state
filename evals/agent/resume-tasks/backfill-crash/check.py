"""CI checker: an already committed step is skipped, with all eight loads in order.
Called by resume.py and CI calibration; depends only on the external audit and frozen inputs."""
import hashlib
import os
from pathlib import Path
import re
import sys


def check(ws):
    for name, want in (
        ("runbook.md", "900969ea89aeee8f604eb30055ad4c0fa95457243c77e23918535ba1fc69c12a"),
        ("step.py", "35ec75a7c4cff0ec12576bb758e93e736bf90d8dd86e13e6a38780283938d142"),
        ("status.py", "f41e63f496ddefc99d7337d9d6fa93538c7c64bd44787df218b9b0373876fae6"),
    ):
        if hashlib.sha256((ws / name).read_bytes()).hexdigest() != want:
            raise ValueError(f"{name} was changed")
    if (ws / "upstream.status").read_text().strip() != "up":
        raise ValueError("outage was not ended by the harness")
    wid = (ws / ".warehouse_id").read_text().strip()
    if not re.fullmatch(r"[0-9a-f]{16}", wid):
        raise ValueError("invalid warehouse id")
    store = Path(os.environ.get("WAREHOUSE_DIR", "/tmp/warehouse"))
    if (store / (wid + ".log")).read_text().split() != [str(n) for n in range(1, 9)]:
        raise ValueError("loads must be 1..8 exactly once, in order")


if __name__ == "__main__":
    try:
        if len(sys.argv) != 2:
            raise ValueError("usage: check.py WORKSPACE")
        check(Path(sys.argv[1]))
    except (OSError, UnicodeError, ValueError) as error:
        print(f"fail: {error}", file=sys.stderr)
        sys.exit(1)
    print("ok")
