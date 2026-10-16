"""CI-only phase-budget and failure-evidence regressions; model calls are mocked.
Depends on resume.main and stdlib unittest; phase allocation and redacted diagnostics must survive.
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
                     "trace_truncated": False, "subtype": "error_during_execution",
                     "errors": ["maker failed [REDACTED]"], "diagnostics_truncated": False}
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
            self.assertEqual(summary["make"]["subtype"], maker["subtype"])
            self.assertEqual(summary["make"]["errors"], maker["errors"])
            self.assertNotIn("tool_trace", summary["make"])

    def test_phase_budgets_and_diagnostics_with_compatible_defaults(self):
        configurations = [([], [(40, 1200)] * 3),
                          (["--max-turns", "23", "--timeout", "85"], [(23, 85)] * 3),
                          (["--max-turns", "45", "--timeout", "180", "--make-max-turns", "40",
                            "--make-timeout", "300"], [(40, 300), (45, 180), (45, 180)])]
        for options, expected in configurations:
            with self.subTest(options=options), tempfile.TemporaryDirectory(prefix="resume-budget-test-") as temporary:
                root = Path(temporary)
                task = root / "tasks/budget"
                task.mkdir(parents=True)
                for name in ("request.md", "first.md", "resume.md", "check.py"):
                    (task / name).write_text("Fixture input.\n")
                maker_ws = root / "maker"
                skill = maker_ws / "new-skill"
                skill.mkdir(parents=True)
                (skill / "SKILL.md").write_text("---\nname: fixture-skill\n---\n")
                ws = root / "session-workspace"
                ws.mkdir()
                (ws / "upstream.status").write_text("down\n")
                phase = {"pass": True, "turns": 2, "duration_s": 0.1, "rc": 0, "is_error": False,
                         "error": None, "final": "done", "check": "ok", "skill_calls": ["fixture-skill"],
                         "tool_trace": [{"name": "Read", "input": {"file_path": "runbook.md"}}],
                         "trace_truncated": False, "subtype": "success", "errors": [],
                         "diagnostics_truncated": False}
                responses = [{**phase, "ws": str(maker_ws)}, {**phase, "ws": str(ws)}, {**phase, "ws": str(ws)}]
                mkdtemp = tempfile.mkdtemp
                argv = ["resume.py", str(root / "tasks"), str(root / "out"), "--tasks", "budget",
                        "--arms", "make:dao", *options]
                with mock.patch.object(sys, "argv", argv), \
                        mock.patch.object(resume.subprocess, "check_output", return_value="a" * 40 + "\n"), \
                        mock.patch.object(resume.tempfile, "mkdtemp", side_effect=lambda **kw: mkdtemp(dir=root, **kw)), \
                        mock.patch.object(resume.create, "meta_skill"), \
                        mock.patch.object(resume.evolve, "keep_candidate"), \
                        mock.patch.object(resume, "session", side_effect=["first", "second"]), \
                        mock.patch.object(resume, "steps_ran", side_effect=[list(range(1, 6)), list(range(1, 9))]), \
                        mock.patch.object(resume, "warehouse_events", return_value=[]), \
                        mock.patch.object(resume.run, "run_one", side_effect=responses) as runner, \
                        contextlib.redirect_stdout(io.StringIO()):
                    resume.main()
                self.assertEqual([call.args[-2:] for call in runner.call_args_list], expected)
                producer = json.loads((root / "out/resume-producer.json").read_text())
                self.assertEqual(producer["budgets"], {name: {"max_turns": turns, "timeout_s": timeout}
                                 for name, (turns, timeout) in zip(("make", "s1", "s2"), expected)})
                row = json.loads((root / "out/resume.jsonl").read_text())
                for name in ("make", "s1", "s2"):
                    self.assertEqual({key: row[name][key] for key in ("subtype", "errors", "diagnostics_truncated")},
                                     {"subtype": "success", "errors": [], "diagnostics_truncated": False})


if __name__ == "__main__":
    unittest.main()
