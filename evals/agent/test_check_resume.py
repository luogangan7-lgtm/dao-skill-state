"""CI calibration for check_resume; stdlib fake matrices and real CLI artifacts expose false greens."""
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from check_resume import validate_rows


class CheckResumeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        task = self.root / "task"
        task.mkdir()
        (task / "expect_s1.json").write_text("[1, 2, 3, 4, 5]\n")
        (task / "resume_gate.json").write_text('{"query":{"step":5,"loaded":true}}\n')
        first = [event for n in range(1, 6) for event in ({"kind": "attempt", "n": n}, {"kind": "load", "n": n})]
        phase = {"rc": 0, "error": None, "is_error": False, "turns": 2, "skill_calls": ["backfill-runbook"],
                 "tool_trace": [{"name": "Skill", "input": {"skill": "backfill-runbook"}}], "trace_truncated": False}
        self.row = {"task": "task", "arm": "make:dao", "rep": 0, "pass": True, "made": True,
                    "clean_stop": True, "s2_loaded_runbook": True, "tampered": False,
                    "reruns": 0, "probes": 0, "skill_name": "backfill-runbook",
                    "after_session1": [1, 2, 3, 4, 5], "ran": list(range(1, 9)),
                    "events_after_s1": first,
                    "events": first + [{"kind": "query", "n": 5, "loaded": True}]
                              + [event for n in range(6, 9) for event in ({"kind": "attempt", "n": n}, {"kind": "load", "n": n})],
                    "make": copy.deepcopy(phase), "s1": copy.deepcopy(phase), "s2": copy.deepcopy(phase)}

    def validate(self, rows):
        return validate_rows(rows, ["task"], ["make:dao"], 1, self.root)

    def test_good_matrix(self):
        self.assertEqual(self.validate([self.row]), 1)

    def test_missing_duplicate_extra_rows(self):
        extra = {**self.row, "rep": 1}
        for rows in ([], [self.row, self.row], [extra]):
            with self.subTest(rows=len(rows)):
                with self.assertRaises(ValueError):
                    self.validate(rows)

    def test_false_or_malformed_fields(self):
        bad = {"pass": False, "made": False, "clean_stop": False, "tampered": True,
               "s2_loaded_runbook": False, "reruns": 1, "probes": 1,
               "rep": True, "after_session1": [True], "ran": [1, "2"], "events": None}
        for key, value in bad.items():
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.validate([{**self.row, key: value}])

    def test_model_errors_and_missing_skill_are_red(self):
        for phase in ("make", "s1", "s2"):
            for key, value in (("rc", 1), ("rc", False), ("error", "quota"), ("is_error", True), ("turns", 0), ("turns", True), ("tool_trace", []), ("trace_truncated", True)):
                row = copy.deepcopy(self.row)
                row[phase][key] = value
                with self.subTest(phase=phase, key=key), self.assertRaises(ValueError):
                    self.validate([row])
        row = copy.deepcopy(self.row)
        row["s1"]["skill_calls"] = []
        with self.assertRaises(ValueError):
            self.validate([row])

    def test_query_must_be_correct_before_new_actions(self):
        for mutation in ("missing", "late", "wrong"):
            row = copy.deepcopy(self.row)
            position = len(row["events_after_s1"])
            query = row["events"].pop(position)
            if mutation == "late":
                row["events"].append(query)
            if mutation == "wrong":
                row["events"].insert(position, {**query, "loaded": False})
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                self.validate([row])

    def test_no_retry_still_requires_not_applied_query(self):
        (self.root / "task/resume_gate.json").write_text('{"query":{"step":5,"loaded":false}}\n')
        row = copy.deepcopy(self.row)
        (self.root / "task/expect_s1.json").write_text("[1, 2, 3, 4]\n")
        row["after_session1"] = [1, 2, 3, 4]
        row["ran"] = row["after_session1"]
        row["events_after_s1"] = row["events_after_s1"][:-1]
        row["events"] = row["events_after_s1"] + [{"kind": "query", "n": 5, "loaded": False}]
        self.assertEqual(self.validate([row]), 1)
        row["events"] = row["events_after_s1"]
        with self.assertRaises(ValueError):
            self.validate([row])

    def test_retry_allows_confirming_success_after_commit(self):
        (self.root / "task/resume_gate.json").write_text('{"query":{"step":5,"loaded":false}}\n')
        (self.root / "task/expect_s1.json").write_text("[1, 2, 3, 4]\n")
        row = copy.deepcopy(self.row)
        row["after_session1"] = [1, 2, 3, 4]
        row["events_after_s1"] = row["events_after_s1"][:-1]
        suffix = [{"kind": "query", "n": 5, "loaded": False}, {"kind": "attempt", "n": 5},
                  {"kind": "load", "n": 5}, {"kind": "query", "n": 5, "loaded": True}]
        row["events"] = row["events_after_s1"] + suffix + row["events"][11:]
        self.assertEqual(self.validate([row]), 1)

    def test_unknown_result_forbids_invented_query(self):
        (self.root / "task/resume_gate.json").write_text('{"no_query":true}\n')
        row = copy.deepcopy(self.row)
        row["ran"] = row["after_session1"]
        row["events"] = row["events_after_s1"]
        self.assertEqual(self.validate([row]), 1)
        row["events"] = row["events"] + [{"kind": "query", "n": 5, "loaded": True}]
        with self.assertRaises(ValueError):
            self.validate([row])

    def test_changed_audit_or_false_clean_stop_is_red(self):
        for field in ("events_after_s1", "events"):
            row = copy.deepcopy(self.row)
            row[field][0]["n"] = 8
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.validate([row])
        row = copy.deepcopy(self.row)
        row["ran"] = row["ran"][:-1]
        with self.assertRaises(ValueError):
            self.validate([row])
        row = copy.deepcopy(self.row)
        row["after_session1"] = [1]
        with self.assertRaises(ValueError):
            self.validate([row])

    def test_cli_good_then_bad_receipts_and_artifacts(self):
        out = self.root / "out"
        artifact = out / "created/task-dao-0"
        artifact.mkdir(parents=True)
        skill = artifact / "SKILL.md"
        skill.write_text('---\nname: backfill-runbook\ndescription: Run warehouse backfills from numbered runbooks.\n---\n\n# Backfill\n\nRead runbook.md, query each step, and run unfinished steps in order.\n')
        self.row["skill_hashes"] = {"SKILL.md": hashlib.sha256(skill.read_bytes()).hexdigest()}
        results = out / "resume.jsonl"
        results.write_text(json.dumps(self.row) + "\n")
        command = [sys.executable, str(Path(__file__).with_name("check_resume.py")), str(out),
                   "--tasks", "task", "--arms", "make:dao", "--reps", "1", "--tasks-dir", str(self.root)]
        environment = {**os.environ}
        environment.pop("GITHUB_SHA", None)
        def run_cli(code):
            result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=90)
            self.assertEqual(result.returncode, code, result.stderr)
            self.assertNotIn("Traceback", result.stderr)
            self.assertIs(json.loads((out / "resume-gate.json").read_text())["pass"], code == 0)
        run_cli(0)
        results.write_text(json.dumps({**self.row, "pass": False}) + "\n")
        run_cli(1)
        results.write_text(json.dumps(self.row) + "\n")
        run_cli(0)
        skill.write_text(skill.read_text() + "Changed\n")
        run_cli(1)
        skill.write_text("Missing frontmatter\n")
        self.row["skill_hashes"] = {"SKILL.md": hashlib.sha256(skill.read_bytes()).hexdigest()}
        results.write_text(json.dumps(self.row) + "\n")
        run_cli(1)
        results.write_text("{not json\n")
        run_cli(1)
        results.write_text(json.dumps(self.row) + "\n")
        skill.unlink()
        skill.symlink_to(self.root / "task/expect_s1.json")
        run_cli(1)


if __name__ == "__main__":
    unittest.main()
