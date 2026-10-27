#!/usr/bin/env python3
"""A later repetition must not see an earlier one's scratch dirs (run.sweep, run.model_env).
Runs in CI (agent-eval.yml, checkers job); needs no model."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run  # noqa: E402


class Isolation(unittest.TestCase):
    def test_sweep_removes_earlier_scratch_dirs(self):
        root = tempfile.gettempdir()
        self.assertNotEqual(os.path.realpath(root), os.path.realpath("/tmp"))
        earlier = tempfile.mkdtemp(prefix="ws-")
        open(os.path.join(earlier, "SKILL.md"), "w").write("x")
        run.sweep()
        self.assertFalse(os.path.exists(earlier))
        self.assertTrue(os.path.isdir(root))

    def test_agent_tmpdir_is_swept_too(self):
        self.assertEqual(run.model_env("/home-x")["TMPDIR"], tempfile.gettempdir())


if __name__ == "__main__":
    unittest.main()
