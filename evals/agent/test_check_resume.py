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
                 "tool_trace": [{"name": "Skill", "input": {"skill": "backfill-runbook"}}], "trace_truncated": False, "diagnostics_truncated": False, "subtype": "success", "errors": []}
        self.row = {"task": "task", "arm": "make:dao", "rep": 0, "pass": True, "made": True,
                    "clean_stop": True, "s2_loaded_runbook": True, "tampered": False,
                    "reruns": 0, "probes": 0, "skill_name": "backfill-runbook",
                    "after_session1": [1, 2, 3, 4, 5], "ran": list(range(1, 9)),
                    "events_after_s1": first,
                    "events": copy.deepcopy(first) + [{"kind": "query", "n": 5, "loaded": True}]
                              + [event for n in range(6, 9) for event in ({"kind": "attempt", "n": n}, {"kind": "load", "n": n})],
                    "make": {**copy.deepcopy(phase), "skill_calls": ["dao-skill-state"]},
                    "s1": copy.deepcopy(phase), "s2": copy.deepcopy(phase)}

    def validate(self, rows):
        return validate_rows(rows, ["task"], ["make:dao"], 1, self.root)

    def test_good_matrix(self):
        self.assertEqual(self.validate([self.row]), 1)

    def test_observed_maker_and_worker_probes_cannot_hide_behind_zero_counter(self):
        # Actual CI38876167175 maker/s2 command shapes, with unrelated phase failures removed.
        accesses = [
            {"name": "Bash", "input": {"command": "cat /tmp/warehouse/*.events.jsonl"}},
            {"name": "Bash", "input": {"command": "cat /tmp/resume-89t6zs8r/prompt.md; cat /tmp/resume-89t6zs8r/files/runbook.md"}},
            {"name": "Bash", "input": {"command": "cat /opt/hostedtoolcache/Python/3.12.15/x64/lib/python3.12/site-packages/warehouse.py"}},
            {"name": "Bash", "input": {"command": "cd /home/runner/work/dao-skill-state/dao-skill-state/evals/agent && sed -n '1,200p' check_resume.py"}},
            {"name": "Read", "input": {"file_path": "/repo/evals/agent/resume-tasks/backfill-crash/expect_s1.json"}},
        ]
        for phase in ("make", "s1", "s2"):
            for event in accesses:
                row = copy.deepcopy(self.row)
                row[phase]["tool_trace"].append(event)
                self.assertEqual(row["probes"], 0)
                with self.subTest(phase=phase, event=event), self.assertRaisesRegex(ValueError, "oracle access"):
                    self.validate([row])

    def test_written_mock_content_and_own_trial_inputs_remain_green(self):
        row = copy.deepcopy(self.row)
        for phase in ("make", "s1", "s2"):
            row[phase]["tool_trace"] += [
                {"name": "Write", "input": {"file_path": "/tmp/trial/warehouse.py", "content": "import warehouse; /tmp/warehouse; /repo/evals/agent/check_resume.py"}},
                {"name": "Bash", "input": {"command": "cat > /tmp/trial/mock.py <<'PY'\nimport warehouse\nSTORE = '/tmp/warehouse'\nPY"}},
                {"name": "Bash", "input": {"command": "cat /tmp/bf-fwd-a/warehouse.txt; python3 status.py 5"}},
            ]
        self.assertEqual(self.validate([row]), 1)

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
            for key, value in (("rc", 1), ("rc", False), ("error", "quota"), ("is_error", True), ("turns", 0), ("turns", True), ("tool_trace", []), ("trace_truncated", True), ("diagnostics_truncated", True), ("diagnostics_truncated", None), ("subtype", "error_max_turns"), ("subtype", None), ("errors", ["failed"]), ("errors", None)):
                row = copy.deepcopy(self.row)
                row[phase][key] = value
                with self.subTest(phase=phase, key=key), self.assertRaises(ValueError):
                    self.validate([row])
        row = copy.deepcopy(self.row)
        row["s1"]["skill_calls"] = []
        with self.assertRaises(ValueError):
            self.validate([row])

    def test_maker_must_load_selected_meta_skill(self):
        for calls in ([], ["backfill-runbook"], ["skill-creator"]):
            row = copy.deepcopy(self.row)
            row["make"]["skill_calls"] = calls
            with self.subTest(calls=calls), self.assertRaises(ValueError):
                self.validate([row])
        row = copy.deepcopy(self.row)
        row["arm"] = "make:skill-creator"
        row["make"]["skill_calls"] = ["read:skill-creator"]
        self.assertEqual(validate_rows([row], ["task"], ["make:skill-creator"], 1, self.root), 1)
        row["make"]["skill_calls"] = ["dao-skill-state"]
        with self.assertRaises(ValueError):
            validate_rows([row], ["task"], ["make:skill-creator"], 1, self.root)
        row["arm"], row["make"]["skill_calls"] = "make:none", []
        self.assertEqual(validate_rows([row], ["task"], ["make:none"], 1, self.root), 1)

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
        repo = Path(__file__).resolve().parents[2]
        producer = {"sha": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip(),
                    "authoring_sha256": hashlib.sha256((repo / "dao-skill-state/references/authoring.md").read_bytes()).hexdigest(),
                    "fixtures": {"task": {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in (self.root / "task").iterdir()}},
                    "tasks": ["task"], "arms": ["make:dao"], "reps": 1,
                    "budgets": {phase: {"max_turns": 40, "timeout_s": 1200} for phase in ("make", "s1", "s2")},
                    "github_run_id": os.environ.get("GITHUB_RUN_ID"), "github_run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT")}
        producer_file = out / "resume-producer.json"
        producer_file.write_text(json.dumps(producer, sort_keys=True, indent=2) + "\n")
        self.row["producer_sha256"] = hashlib.sha256(producer_file.read_bytes()).hexdigest()
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
            return result
        run_cli(0)
        original_producer = producer_file.read_bytes()
        for mutation in ("old-sha", "changed-fixture", "producer-hash", "bad-budget", "bool-budget", "missing-budget"):
            modified = copy.deepcopy(producer)
            if mutation == "old-sha":
                modified["sha"] = "0" * 40
            elif mutation == "changed-fixture":
                modified["fixtures"]["task"]["expect_s1.json"] = "0" * 64
            elif mutation == "bad-budget":
                modified["budgets"]["make"]["timeout_s"] = 0
            elif mutation == "bool-budget":
                modified["budgets"]["s1"]["max_turns"] = True
            elif mutation == "missing-budget":
                del modified["budgets"]["s2"]
            producer_file.write_text(json.dumps(modified, sort_keys=True, indent=2) + "\n")
            if mutation == "producer-hash":
                producer_file.write_bytes(original_producer + b"\n")
            # Old SHA / fixtures remain RED even when the row points at the changed manifest.
            changed_row = dict(self.row)
            if mutation != "producer-hash":
                changed_row["producer_sha256"] = hashlib.sha256(producer_file.read_bytes()).hexdigest()
            results.write_text(json.dumps(changed_row) + "\n")
            result = run_cli(1)
            if mutation == "producer-hash":
                self.assertIn("row producer hash mismatch", result.stderr)
        producer_file.write_bytes(original_producer)
        results.write_text(json.dumps(self.row) + "\n")
        fixture = self.root / "task/expect_s1.json"
        original_fixture = fixture.read_bytes()
        fixture.write_bytes(original_fixture + b" ")
        run_cli(1)
        fixture.write_bytes(original_fixture)
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
