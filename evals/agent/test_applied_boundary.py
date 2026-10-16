"""CI calibration of committed-action continuation; the checker grades external receipts.
Known-good input goes green; duplicates, skips, altered helpers and malformed logs go red."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


TASK = Path(__file__).resolve().parent / "resume-tasks/backfill-crash"
WID = "a" * 16


class AppliedBoundary(unittest.TestCase):
    def check_case(self, sequence="1 2 3 4 5 6 7 8", changed=None):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ws, store = root / "workspace", root / "store"
            shutil.copytree(TASK / "files", ws)
            store.mkdir()
            (ws / ".warehouse_id").write_text(WID)
            (ws / "upstream.status").write_text("up\n")
            (store / (WID + ".log")).write_text(sequence)
            if changed:
                path = ws / changed
                path.write_bytes(path.read_bytes() + b"# changed\n")
            return subprocess.run(
                [sys.executable, str(TASK / "check.py"), str(ws)],
                env={**os.environ, "WAREHOUSE_DIR": str(store)},
                capture_output=True, text=True, timeout=10,
            )

    def test_known_good_continuation_passes(self):
        result = self.check_case()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_duplicate_skipped_or_malformed_loads_are_red(self):
        for sequence in ("", "1 2 3 4 5", "1 2 3 4 5 5 6 7 8", "1 2 3 4 6 7 8", "1 2 invalid"):
            with self.subTest(sequence=sequence):
                result = self.check_case(sequence)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertNotIn("Traceback", result.stderr)

    def test_changed_authoritative_helpers_are_red(self):
        for name in ("runbook.md", "step.py", "status.py", "upstream.status"):
            with self.subTest(name=name):
                result = self.check_case(changed=name)
                self.assertEqual(result.returncode, 1, result.stderr)


if __name__ == "__main__":
    unittest.main()
