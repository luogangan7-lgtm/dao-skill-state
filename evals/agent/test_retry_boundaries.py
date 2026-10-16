"""CI calibration for retry-policy checkers; unittest and subprocess only, no model calls.
Invoked by agent-eval CI. Known-good external receipts pass; mutated receipts/inputs exit 1.
Each case owns its temporary workspace/store and never executes a backfill command."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent / "resume-tasks"
TASKS = ("backfill-not-applied-retry", "backfill-not-applied-no-retry")
WID = "a" * 16


class RetryBoundaryCheckers(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="retry-checker-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.counter = 0

    def fixture(self, task):
        self.counter += 1
        base = self.root / str(self.counter)
        ws, store = base / "workspace", base / "external-store"
        shutil.copytree(ROOT / task / "files", ws)
        store.mkdir()
        (ws / ".warehouse_id").write_text(WID + "\n")
        (ws / "upstream.status").write_text("up\n")
        if task == TASKS[0]:
            loads, attempts = list(range(1, 9)), [1, 2, 3, 4, 5, 5, 6, 7, 8]
        else:
            loads, attempts = [1, 2, 3, 4], [1, 2, 3, 4, 5]
        self.receipts(store, loads, attempts)
        return ws, store

    def receipts(self, store, loads, attempts):
        for suffix, sequence in ((".log", loads), (".attempts.log", attempts)):
            (store / (WID + suffix)).write_text("".join(f"{n}\n" for n in sequence))

    def result(self, task, ws, store, want):
        result = subprocess.run(
            [sys.executable, str(ROOT / task / "check.py"), str(ws)],
            env={**os.environ, "WAREHOUSE_DIR": str(store)},
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, want, result.stdout + result.stderr)

    def test_known_good_boundaries_pass(self):
        for task in TASKS:
            with self.subTest(task=task):
                ws, store = self.fixture(task)
                self.result(task, ws, store, 0)

    def test_duplicate_receipts_fail(self):
        for task in TASKS:
            for suffix in (".log", ".attempts.log"):
                with self.subTest(task=task, suffix=suffix):
                    ws, store = self.fixture(task)
                    path = store / (WID + suffix)
                    path.write_text(path.read_text() + "1\n")
                    self.result(task, ws, store, 1)

    def test_wrong_execution_boundaries_fail(self):
        mutations = (
            (TASKS[0], [1, 2, 3, 4], [1, 2, 3, 4, 5]),  # stopped despite authorized retry
            (TASKS[0], [1, 2, 3, 4, 5, 7, 8], [1, 2, 3, 4, 5, 5, 7, 8]),  # skipped batch 6
            (TASKS[0], list(range(1, 9)), list(range(1, 9))),  # no initial failed attempt
            (TASKS[0], [1, 2, 3, 4, 6, 5, 7, 8], [1, 2, 3, 4, 5, 6, 5, 7, 8]),
            (TASKS[1], list(range(1, 9)), [1, 2, 3, 4, 5, 5, 6, 7, 8]),  # unauthorized retry
            (TASKS[1], [1, 2, 3, 4], [1, 2, 3, 4, 5, 5]),  # retry with no commit
            (TASKS[1], [1, 2, 3, 4, 6, 7, 8], [1, 2, 3, 4, 5, 6, 7, 8]),  # skipped failed batch
            (TASKS[1], [1, 2, 3], [1, 2, 3, 4]),  # stopped before required boundary
        )
        for task, loads, attempts in mutations:
            with self.subTest(task=task, loads=loads, attempts=attempts):
                ws, store = self.fixture(task)
                self.receipts(store, loads, attempts)
                self.result(task, ws, store, 1)

    def test_changed_or_missing_fixture_inputs_fail(self):
        for task in TASKS:
            for name in ("runbook.md", "step.py", "status.py"):
                for mutation in ("change", "remove"):
                    with self.subTest(task=task, name=name, mutation=mutation):
                        ws, store = self.fixture(task)
                        path = ws / name
                        if mutation == "change":
                            path.write_bytes(path.read_bytes() + b"\n# altered\n")
                        else:
                            path.unlink()
                        self.result(task, ws, store, 1)

    def test_outage_not_ended_fails(self):
        for task in TASKS:
            ws, store = self.fixture(task)
            (ws / "upstream.status").write_text("down\n")
            self.result(task, ws, store, 1)

    def test_malformed_missing_and_empty_receipts_fail_without_crashing(self):
        for task in TASKS:
            for suffix in (".log", ".attempts.log"):
                for content in ("", "{\"steps\": [1, 2, 3, 4]}", "1\nunknown\n", "9\n", None):
                    with self.subTest(task=task, suffix=suffix, content=content):
                        ws, store = self.fixture(task)
                        path = store / (WID + suffix)
                        if content is None:
                            path.unlink()
                        else:
                            path.write_text(content)
                        self.result(task, ws, store, 1)

    def test_bad_workspace_identity_fails_without_crashing(self):
        for task in TASKS:
            for value in ("", "../escape", '{"id": 1}', None):
                with self.subTest(task=task, value=value):
                    ws, store = self.fixture(task)
                    path = ws / ".warehouse_id"
                    if value is None:
                        path.unlink()
                    else:
                        path.write_text(value)
                    self.result(task, ws, store, 1)

    def test_empty_workspace_fails_without_crashing(self):
        for task in TASKS:
            self.counter += 1
            ws = self.root / str(self.counter)
            ws.mkdir()
            self.result(task, ws, self.root / "unused-store", 1)


if __name__ == "__main__":
    unittest.main()
