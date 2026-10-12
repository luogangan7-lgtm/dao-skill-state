#!/usr/bin/env python3

from __future__ import annotations

import json
import math
import os
import contextlib
import io
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import check_assessment as check


def sample() -> dict:
    sha = "a" * 64
    return {
        "schema": "dao-assessment/v1",
        "identity": {"skill": "demo", "skill_sha256": sha, "task_set_sha256": sha,
                     "model": "model", "host": "local", "date": "2026-09-08", "rubric": "r1"},
        "arms": ["baseline", "candidate"], "repetitions": 1,
        "cases": [{"id": "p", "kind": "positive"}, {"id": "b", "kind": "boundary"}],
        "isolation": {"status": "PASS", "evidence": "isolated"}, "runs": [],
    }


def run(case_id: str, arm: str, status: str = "PASS", trust: str = "PASS", cost: dict | None = None) -> dict:
    if cost is None:
        cost = {"seconds": 1.0, "input_tokens": 10, "output_tokens": 2, "cached_input_tokens": 0}
    return {"case_id": case_id, "arm": arm, "repetition": 1, "status": status,
            "trust": trust, "evidence": "recorded", "cost": cost}


class AssessmentTests(unittest.TestCase):
    def complete(self) -> dict:
        data = sample()
        data["runs"] = [run(case, arm) for case in ("p", "b") for arm in data["arms"]]
        return data

    def test_happy_path_counts_cost_and_pairing(self):
        result = check.summarize(self.complete())
        self.assertEqual("COMPLETE", result["gate"])
        self.assertTrue(result["all_tasks_passed"])
        self.assertEqual(2, result["arms"]["candidate"]["overall"]["counts"]["PASS"])
        self.assertEqual(2.0, result["arms"]["candidate"]["overall"]["cost"]["seconds"]["sum"])
        self.assertEqual(2, result["pairwise"]["comparable_pairs"])
        self.assertEqual(0.0, result["pairwise"]["delta_pp"])

    def test_single_arm_is_valid_without_pairwise(self):
        data = sample(); data["arms"] = ["solo"]; data["runs"] = [run(case, "solo") for case in ("p", "b")]
        self.assertEqual([], check.validate(data))
        self.assertNotIn("pairwise", check.summarize(data))

    def test_many_arms_are_grouped_without_rescanning_rows(self):
        class CountingArm(str):
            comparisons = 0
            __hash__ = str.__hash__

            def __eq__(self, other):
                type(self).comparisons += 1
                return str.__eq__(self, other)

        data = sample(); data["cases"] = [{"id": "p", "kind": "positive"}]
        data["arms"] = [CountingArm(f"arm-{i}") for i in range(check.MAX_ARMS)]
        data["runs"] = [run("p", data["arms"][-1])]
        self.assertEqual([], check.validate(data))
        result = check.summarize(data)
        self.assertEqual(1, result["arms"][data["arms"][0]]["overall"]["counts"]["NOT_RUN"])
        self.assertEqual(1, result["arms"][data["arms"][-1]]["overall"]["counts"]["PASS"])
        self.assertLess(CountingArm.comparisons, len(data["arms"]) * 4)

    def test_arm_resource_limit_accepts_boundary_and_rejects_ninth(self):
        data = sample(); data["arms"] = [f"arm-{i}" for i in range(check.MAX_ARMS)]
        self.assertEqual([], check.validate(data))
        data["arms"].append("too-many")
        self.assertIn("at most 8 arms", " ".join(e["message"] for e in check.validate(data)))

    def test_failures_stay_assessed_and_do_not_mean_all_passed(self):
        data = self.complete()
        data["runs"][2]["status"] = "FAIL"
        result = check.summarize(data)
        self.assertEqual("COMPLETE", result["gate"])
        self.assertFalse(result["all_tasks_passed"])
        self.assertEqual(1.0, result["arms"]["baseline"]["overall"]["coverage"])
        self.assertEqual(0.5, result["arms"]["baseline"]["overall"]["conditional_pass"])

    def test_missing_unknown_and_invalid_block_completeness(self):
        for status in (None, "UNKNOWN", "INVALID"):
            with self.subTest(status=status):
                data = self.complete()
                if status is None:
                    data["runs"].pop()
                else:
                    data["runs"][-1]["status"] = status
                result = check.summarize(data)
                self.assertFalse(result["data_complete"])
                self.assertEqual("INCOMPLETE_OR_BLOCKED", result["gate"])
                if status is None:
                    self.assertEqual(1, result["arms"]["candidate"]["overall"]["counts"]["NOT_RUN"])

    def test_trust_blocks_gate_without_changing_task_passes(self):
        data = self.complete()
        data["runs"][0]["trust"] = "CONDITIONAL"
        result = check.summarize(data)
        self.assertTrue(result["data_complete"])
        self.assertTrue(result["all_tasks_passed"])
        self.assertFalse(result["trust_clear"])
        self.assertEqual("INCOMPLETE_OR_BLOCKED", result["gate"])

    def test_duplicate_unknown_and_bad_repetitions(self):
        mutations = []
        data = self.complete(); data["cases"].append({"id": "p", "kind": "positive"}); mutations.append(data)
        data = self.complete(); data["arms"].append("baseline"); mutations.append(data)
        data = self.complete(); data["runs"].append(dict(data["runs"][0])); mutations.append(data)
        data = self.complete(); data["runs"][0]["case_id"] = "missing"; mutations.append(data)
        data = self.complete(); data["runs"][0]["arm"] = "missing"; mutations.append(data)
        data = self.complete(); data["runs"][0]["repetition"] = 2; mutations.append(data)
        data = self.complete(); data["repetitions"] = True; mutations.append(data)
        data = self.complete(); data["runs"][0]["case_id"] = []; mutations.append(data)
        data = self.complete(); data["runs"][0]["repetition"] = []; mutations.append(data)
        for data in mutations:
            with self.subTest(errors=check.validate(data)):
                self.assertTrue(check.validate(data))

    def test_bool_fraction_nonfinite_and_negative_cost_rejected(self):
        for field, value in (("seconds", True), ("seconds", math.inf), ("seconds", -1),
                             ("input_tokens", True), ("input_tokens", 1.5), ("output_tokens", -1)):
            with self.subTest(field=field, value=value):
                data = self.complete()
                data["runs"][0]["cost"][field] = value
                self.assertTrue(check.validate(data))

        data = self.complete(); data["runs"][0]["cost"]["seconds"] = 10 ** 1000
        self.assertTrue(check.validate(data))

    def test_expanded_plan_limit_is_validation_error(self):
        data = sample(); data["repetitions"] = check.MAX_PLANNED_ROWS + 1
        self.assertIn("maximum", " ".join(e["message"] for e in check.validate(data)))

    def test_nonfinite_cost_sum_is_null_with_reason(self):
        data = self.complete()
        for row in data["runs"]:
            row["cost"]["seconds"] = 1e308
        result = check.summarize(data)
        cost = result["arms"]["baseline"]["overall"]["cost"]["seconds"]
        self.assertIsNone(cost["sum"])
        self.assertEqual("sum is non-finite", cost["unavailable_reason"])
        json.dumps(result, allow_nan=False)

    def test_unknown_isolation_suppresses_delta(self):
        data = self.complete(); data["isolation"]["status"] = "UNKNOWN"
        pairwise = check.summarize(data)["pairwise"]
        self.assertIsNone(pairwise["delta_pp"])
        self.assertIn("isolation", pairwise["delta_unavailable_reason"])

    def test_unknown_cost_is_not_zero(self):
        data = self.complete(); data["runs"][0]["cost"] = None
        cost = check.summarize(data)["arms"]["baseline"]["overall"]["cost"]["seconds"]
        self.assertIsNone(cost["sum"])
        self.assertEqual(1, cost["known_values"])
        self.assertEqual("missing planned values", cost["unavailable_reason"])

    def test_empty_kind_cost_is_unknown_not_zero_and_identity_is_retained(self):
        data = self.complete()
        result = check.summarize(data)
        cost = result["arms"]["baseline"]["by_kind"]["nontrigger"]["cost"]["seconds"]
        self.assertEqual(data["identity"], result["identity"])
        self.assertIsNone(cost["sum"])
        self.assertEqual(0, cost["known_values"])
        self.assertEqual("no planned rows", cost["unavailable_reason"])

    def test_identity_evidence_and_unknown_fields_rejected(self):
        data = self.complete(); data["identity"]["date"] = "2026-02-30"
        data["runs"][0]["evidence"] = " "; data["score"] = 100
        messages = " ".join(e["message"] for e in check.validate(data))
        self.assertIn("ISO date", messages)
        self.assertIn("nonempty", messages)
        self.assertIn("unknown fields", messages)

    def test_cli_invalid_json_and_unreadable_are_exit_2_json(self):
        script = Path(check.__file__)
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "bad.json"; bad.write_text("{", encoding="utf-8")
            for path in (bad, Path(tmp) / "missing.json"):
                completed = subprocess.run([sys.executable, str(script), str(path)], text=True, capture_output=True)
                self.assertEqual(2, completed.returncode)
                self.assertEqual("input_error", json.loads(completed.stdout)["status"])

    def test_cli_duplicate_object_key_is_exit_2_json(self):
        script = Path(check.__file__)
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "duplicate.json"; bad.write_text('{"schema":"one","schema":"two"}', encoding="utf-8")
            completed = subprocess.run([sys.executable, str(script), str(bad)], text=True, capture_output=True)
            self.assertEqual(2, completed.returncode)
            self.assertIn("duplicate object key", json.loads(completed.stdout)["errors"][0]["message"])

    def test_cli_surrogate_string_remains_machine_readable(self):
        script = Path(check.__file__)
        with tempfile.TemporaryDirectory() as tmp:
            data = sample(); data["identity"]["host"] = "\ud800"
            path = Path(tmp) / "surrogate.json"; path.write_text(json.dumps(data), encoding="ascii")
            completed = subprocess.run([sys.executable, str(script), str(path)], text=True, capture_output=True)
            self.assertEqual(0, completed.returncode)
            self.assertEqual("", completed.stderr)
            self.assertEqual("\ud800", json.loads(completed.stdout)["identity"]["host"])

    def test_decoder_recursion_is_structured_exit_2(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "input.json"; path.write_text("{}", encoding="ascii")
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout), mock.patch.object(
                check.json, "loads", side_effect=RecursionError("decoder nesting limit")
            ):
                self.assertEqual(2, check.main([str(path)]))
            result = json.loads(stdout.getvalue())
            self.assertEqual("input_error", result["status"])
            self.assertIn("decoder nesting limit", result["errors"][0]["message"])

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO unavailable")
    def test_cli_fifo_is_rejected_without_blocking(self):
        script = Path(check.__file__)
        with tempfile.TemporaryDirectory() as tmp:
            fifo = Path(tmp) / "input.fifo"; os.mkfifo(fifo)
            completed = subprocess.run([sys.executable, str(script), str(fifo)], text=True, capture_output=True, timeout=15)
            self.assertEqual(2, completed.returncode)
            self.assertIn("regular file", json.loads(completed.stdout)["errors"][0]["message"])

    def test_cli_oversize_is_exit_2_json(self):
        script = Path(check.__file__)
        with tempfile.TemporaryDirectory() as tmp:
            huge = Path(tmp) / "huge.json"
            with huge.open("wb") as stream:
                stream.truncate(check.MAX_BYTES + 1)
            completed = subprocess.run([sys.executable, str(script), str(huge)], text=True, capture_output=True)
            self.assertEqual(2, completed.returncode)
            self.assertEqual("input_error", json.loads(completed.stdout)["status"])


if __name__ == "__main__":
    unittest.main()
