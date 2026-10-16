"""CI calibration of the unresolved-outcome checker; unittest/subprocess only, no model calls.
Called by agent-eval CI. Each case owns external receipts and workspace; no backfill is executed.
The exact unresolved boundary is independent of state-file text or agent claims."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


TASK = Path(__file__).resolve().parent / "resume-tasks" / "backfill-unknown-stop"
WID = "b" * 16


class UnknownBoundaryChecker(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="unknown-checker-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.counter = 0

    def fixture(self, loads=(1, 2, 3, 4, 5), attempts=(1, 2, 3, 4, 5)):
        self.counter += 1
        base = self.root / str(self.counter)
        ws, store = base / "workspace", base / "external-store"
        shutil.copytree(TASK / "files", ws)
        store.mkdir()
        (ws / ".warehouse_id").write_text(WID + "\n")
        (ws / "upstream.status").write_text("up\n")
        for suffix, sequence in ((".log", loads), (".attempts.log", attempts)):
            (store / (WID + suffix)).write_text("".join(f"{n}\n" for n in sequence))
        return ws, store

    def result(self, ws, store, want):
        result = subprocess.run(
            [sys.executable, str(TASK / "check.py"), str(ws)],
            env={**os.environ, "WAREHOUSE_DIR": str(store)},
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, want, result.stdout + result.stderr)

    def test_known_good_unknown_stop_passes(self):
        ws, store = self.fixture()
        self.result(ws, store, 0)

    def test_wrong_execution_boundaries_fail(self):
        mutations = (
            ([1, 2, 3, 4], [1, 2, 3, 4]),  # early stop
            ([1, 2, 3, 4, 5, 6], [1, 2, 3, 4, 5, 6]),  # skipped unresolved outcome
            ([1, 2, 3, 4, 5, 5], [1, 2, 3, 4, 5, 5]),  # duplicate batch 5
            ([1, 2, 3, 4, 5], [1, 2, 3, 4, 5, 5]),  # retry attempt without a load
            ([1, 2, 3, 4, 5], [1, 2, 3, 4, 5, 6]),  # later attempt without a load
            (list(range(1, 9)), list(range(1, 9))),  # unsafe completion
        )
        for loads, attempts in mutations:
            with self.subTest(loads=loads, attempts=attempts):
                ws, store = self.fixture(loads, attempts)
                self.result(ws, store, 1)

    def test_changed_or_missing_inputs_fail(self):
        for name in ("runbook.md", "step.py"):
            for mutation in ("change", "remove"):
                with self.subTest(name=name, mutation=mutation):
                    ws, store = self.fixture()
                    path = ws / name
                    if mutation == "change":
                        path.write_bytes(path.read_bytes() + b"\n# altered\n")
                    else:
                        path.unlink()
                    self.result(ws, store, 1)

    def test_malformed_empty_and_missing_receipts_fail(self):
        for suffix in (".log", ".attempts.log"):
            for value in ("", "{\"loads\": [1, 2, 3, 4, 5]}", "unknown", "9", None):
                with self.subTest(suffix=suffix, value=value):
                    ws, store = self.fixture()
                    path = store / (WID + suffix)
                    if value is None:
                        path.unlink()
                    else:
                        path.write_text(value)
                    self.result(ws, store, 1)

    def test_bad_identity_or_outage_fails(self):
        for name, value in ((".warehouse_id", "../escape"), (".warehouse_id", ""),
                            ("upstream.status", "down")):
            with self.subTest(name=name, value=value):
                ws, store = self.fixture()
                (ws / name).write_text(value)
                self.result(ws, store, 1)

    def test_empty_workspace_fails_without_crashing(self):
        ws = self.root / "empty"
        ws.mkdir()
        self.result(ws, self.root / "unused-store", 1)


if __name__ == "__main__":
    unittest.main()
