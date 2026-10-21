#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

GATE = Path(__file__).resolve().parent.parent / "assets" / "step_gate.py"


class StepGateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.env = {**os.environ, "STEP_GATE_STATE": os.path.join(self.tmp.name, "steps.json")}
        self.log = os.path.join(self.tmp.name, "log")

    def tearDown(self):
        self.tmp.cleanup()

    def gate(self, *args):
        return subprocess.run([sys.executable, str(GATE), *args], env=self.env, capture_output=True, text=True)

    def step(self, sid, code=0):  # appends sid to the log, then exits with code
        return self.gate("run", sid, "--", sys.executable, "-c",
                         f"open({self.log!r}, 'a').write('{sid}\\n'); raise SystemExit({code})")

    def ran(self):
        return open(self.log).read().split() if os.path.exists(self.log) else []

    def test_failure_blocks_until_resolved_and_done_steps_skip(self):
        self.assertEqual(self.step("1").returncode, 0)
        failed = self.step("2", code=1)
        self.assertEqual(failed.returncode, 3)
        self.assertIn("STOP", failed.stdout)
        self.assertEqual(self.step("2").returncode, 3)  # blocked: not run again
        self.assertEqual(self.step("3").returncode, 3)  # nor anything after it
        self.assertEqual(self.ran(), ["1", "2"])
        self.assertEqual(self.gate("resolve", "2", "done").returncode, 0)
        self.assertIn("SKIP", self.step("2").stdout)
        self.assertEqual(self.step("1").stdout.split()[0], "SKIP")
        self.assertEqual(self.step("3").returncode, 0)
        self.assertEqual(self.ran(), ["1", "2", "3"])
        self.assertEqual(json.loads(self.gate("status").stdout), {"done": ["1", "2", "3"], "pending": None})

    def test_retry_clears_pending_and_resolve_needs_a_pending_step(self):
        self.step("1", code=1)
        self.assertNotEqual(self.gate("resolve", "9", "done").returncode, 0)
        self.assertEqual(self.gate("resolve", "1", "retry").returncode, 0)
        self.assertEqual(self.step("1").returncode, 0)
        self.assertEqual(self.ran(), ["1", "1"])


if __name__ == "__main__":
    unittest.main()
