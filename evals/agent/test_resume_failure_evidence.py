"""CI-only maker-failure evidence regression; mocked producer/model dependencies never run sessions.
Depends on resume.main and stdlib unittest; failure status and complete redacted diagnostics must survive.
"""
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import resume


class ResumeFailureEvidence(unittest.TestCase):
    def test_failed_maker_keeps_evidence_and_prints_without_sessions(self):
        with tempfile.TemporaryDirectory(prefix="resume-failure-test-") as temporary:
            root = Path(temporary)
            task = root / "tasks/maker-failure"
            task.mkdir(parents=True)
            (task / "request.md").write_text("Make a backfill skill.\n")
            out = root / "out"
            maker = {"pass": False, "turns": 3, "duration_s": 1.25, "rc": 17, "is_error": True,
                     "error": "maker failed [REDACTED]", "final": "Could not create the skill.",
                     "check": "new-skill/SKILL.md is missing", "skill_calls": ["read:dao-skill-state"],
                     "tool_trace": [{"name": "Write", "input": {"content": "[REDACTED]"}}],
                     "trace_truncated": False}
            output = io.StringIO()
            argv = ["resume.py", str(root / "tasks"), str(out), "--tasks", "maker-failure",
                    "--arms", "make:dao", "--reps", "1"]
            with mock.patch.object(sys, "argv", argv), \
                    mock.patch.object(resume.subprocess, "check_output", return_value="a" * 40 + "\n"), \
                    mock.patch.object(resume, "make_skill", return_value=(None, None, maker)) as make, \
                    mock.patch.object(resume, "session") as session, \
                    mock.patch.object(resume.run, "run_one") as run_one, \
                    contextlib.redirect_stdout(output):
                resume.main()
            make.assert_called_once()
            session.assert_not_called()
            run_one.assert_not_called()
            rows = [json.loads(line) for line in (out / "resume.jsonl").read_text().splitlines()]
            self.assertEqual(len(rows), 1)
            row = rows[0]
            self.assertIs(row["pass"], False)
            self.assertIs(row["made"], False)
            self.assertEqual(row["make"], {key: value for key, value in maker.items() if key != "pass"})
            self.assertNotIn("s1", row)
            self.assertNotIn("s2", row)
            summary = json.loads(output.getvalue().splitlines()[0])
            self.assertEqual((summary["task"], summary["arm"], summary["rep"]), ("maker-failure", "make:dao", 0))
            self.assertIs(summary["pass"], False)
            self.assertIs(summary["made"], False)
            self.assertEqual(summary["make"]["rc"], 17)
            self.assertEqual(summary["make"]["error"], maker["error"])
            self.assertEqual(summary["make"]["check"], maker["check"])
            self.assertNotIn("tool_trace", summary["make"])


if __name__ == "__main__":
    unittest.main()
