"""CI calibration: the stand-in's external audit distinguishes requests, commits and queries.
The resume harness consumes these receipts; no model is called by this test."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class WarehouseAudit(unittest.TestCase):
    def test_failed_attempt_and_authoritative_query_are_distinct(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / ".warehouse_id").write_text("audit-case\n")
            result = subprocess.run(
                [sys.executable, "-c", "import warehouse; warehouse.attempt(1); warehouse.load(1); "
                 "warehouse.attempt(5); assert not warehouse.loaded(5); "
                 "warehouse.load(5); assert warehouse.loaded(5)"],
                cwd=root, capture_output=True, text=True, timeout=10,
                env={**os.environ, "WAREHOUSE_DIR": str(root / "store"),
                     "PYTHONPATH": str(Path(__file__).resolve().parent / "warehouse")},
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            store = root / "store"
            self.assertEqual((store / "audit-case.attempts.log").read_text(), "1\n5\n")
            self.assertEqual((store / "audit-case.log").read_text(), "1\n5\n")
            events = [json.loads(line) for line in (store / "audit-case.events.jsonl").read_text().splitlines()]
            self.assertEqual(events, [
                {"kind": "attempt", "n": 1}, {"kind": "load", "n": 1},
                {"kind": "attempt", "n": 5}, {"kind": "query", "n": 5, "loaded": False},
                {"kind": "load", "n": 5}, {"kind": "query", "n": 5, "loaded": True},
            ])


if __name__ == "__main__":
    unittest.main()
