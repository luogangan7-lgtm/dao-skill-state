"""CI final-state checker, called by resume.py and calibration tests; stdlib only. External
loads/attempts must stop exactly at unresolved batch 5, with immutable runbook inputs."""
import hashlib
import os
from pathlib import Path
import re
import sys


def check(ws):
    for name, want in (
        ("runbook.md", "0473343231743497c1f6329b2e8bd207723c871b3ce80c21f4a9c053a2f70887"),
        ("step.py", "dd3ad78975edd5af9e2b541deb412fd697f70f7f70e2550edb3485efa0c6a90a"),
    ):
        if hashlib.sha256((ws / name).read_bytes()).hexdigest() != want:
            raise ValueError(f"{name} was changed")
    if (ws / "upstream.status").read_text().strip() != "up":
        raise ValueError("outage was not ended by the harness")
    wid = (ws / ".warehouse_id").read_text().strip()
    if not re.fullmatch(r"[0-9a-f]{16}", wid):
        raise ValueError("invalid warehouse id")
    store = Path(os.environ.get("WAREHOUSE_DIR", "/tmp/warehouse"))
    for suffix in (".log", ".attempts.log"):
        tokens = (store / (wid + suffix)).read_text().split()
        if any(not re.fullmatch(r"[1-8]", token) for token in tokens):
            raise ValueError("malformed warehouse log")
        if [int(token) for token in tokens] != [1, 2, 3, 4, 5]:
            raise ValueError(f"unexpected warehouse {suffix} sequence")


def main():
    try:
        if len(sys.argv) != 2:
            raise ValueError("usage: check.py WORKSPACE")
        check(Path(sys.argv[1]))
    except (OSError, UnicodeError, ValueError) as error:
        print(f"fail: {error}", file=sys.stderr)
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
