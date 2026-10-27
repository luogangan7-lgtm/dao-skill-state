#!/usr/bin/env python3
"""run_one keeps what check_skill printed, so a warning and the writer's response can be read
from the results (eval H, run 40486956565 recorded commands only). Runs in CI (agent-eval.yml,
checkers job); needs no model: subprocess.run is replaced by a recorded stream."""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run  # noqa: E402

CHECK = "python3 /x/.claude/skills/dao-skill-state/scripts/check_skill.py out/pages"
REPORT = {"path": "out/pages", "errors": [], "status": "valid",
          "warnings": [{"kind": "unchecked-values", "message": "the skill quotes several ratios"}]}


def stream(*events):
    return "\n".join(json.dumps(e) for e in events) + "\n"


def bash(i, command):
    return {"type": "assistant", "message": {"content": [
        {"type": "tool_use", "id": i, "name": "Bash", "input": {"command": command}}]}}


def result(i, content, error=False):
    return {"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": i, "content": content, "is_error": error}]}}


class Trace(unittest.TestCase):
    def test_check_skill_output_is_kept(self):
        out = stream(bash("a", CHECK), result("a", json.dumps(REPORT, indent=2) + "\nexit: 0"),
                     bash("b", "ls out"), result("b", "pages"),
                     bash("c", CHECK), result("c", [{"type": "text", "text": "Traceback: boom"}], error=True),
                     {"type": "result", "num_turns": 3})
        task = tempfile.mkdtemp()
        open(os.path.join(task, "prompt.md"), "w").write("write a skill")
        fake = [subprocess.CompletedProcess([], 0, out, ""), subprocess.CompletedProcess([], 0, "ok", "")]
        with mock.patch.object(run, "install"), mock.patch.object(run.subprocess, "run", side_effect=fake):
            r = run._run_one(task, "none", 5, 60)
        self.assertEqual(r["checks"], [
            {"cmd": CHECK, "status": "valid", "errors": [], "warnings": ["unchecked-values"]},
            {"cmd": CHECK, "out": "Traceback: boom"},
        ])


if __name__ == "__main__":
    unittest.main()
