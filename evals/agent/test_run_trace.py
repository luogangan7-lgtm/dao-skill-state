"""CI tests of run._run_one tool evidence; stdlib unittest mocks both subprocess calls.
Called by agent-eval CI. Complete inputs survive display shortening, secrets are redacted,
and the aggregate trace budget declares omissions. All runner temp directories stay test-owned."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run


class RunToolTrace(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="run-trace-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.task = self.root / "task"
        self.task.mkdir()
        (self.task / "prompt.md").write_text("Fixture prompt; no model is invoked.\n")
        self.original_mkdtemp = tempfile.mkdtemp  # save before patching the shared module

    def record(self, tools, extra_env=None, result_event=None, rc=0, write_output=False):
        stream = "\n".join(json.dumps(event) for event in (
            {"type": "assistant", "message": {"content": [
                {"type": "tool_use", "name": name, "input": value} for name, value in tools
            ]}},
            {"type": "result", "result": "done", "num_turns": 1, "is_error": False,
             "subtype": "success", "errors": [], **(result_event or {})},
        ))
        created = []

        def owned_mkdtemp(*args, **kwargs):
            kwargs["dir"] = str(self.root)
            path = self.original_mkdtemp(*args, **kwargs)
            created.append(Path(path))
            return path

        responses = [subprocess.CompletedProcess(["claude"], rc, stream, ""),
                     subprocess.CompletedProcess(["checker"], 0, "ok\n", "")]

        def respond(*args, **kwargs):
            if len(responses) == 2 and write_output:
                (Path(kwargs["cwd"]) / "created.txt").write_text("Agent output exists despite failure.\n")
            return responses.pop(0)

        with mock.patch.dict(os.environ, {"PATH": os.defpath, **(extra_env or {})}, clear=True), \
                mock.patch.object(run.tempfile, "mkdtemp", side_effect=owned_mkdtemp), \
                mock.patch.object(run.subprocess, "run", side_effect=respond) as process:
            result = run._run_one(str(self.task), "none", 3, 10)
        self.assertEqual(process.call_count, 2)
        self.assertEqual(process.call_args_list[0].args[0][0], "claude")
        self.assertEqual(process.call_args_list[1].args[0][1], str(self.task / "check.py"))
        self.assertEqual(len(created), 2)
        for path in created:
            self.assertEqual(path.parent, self.root)
        self.assertTrue(result["pass"])
        return result

    def test_complete_bash_and_write_inputs_survive_display_shortening(self):
        command = "echo " + "command-body-" * 20 + "; python3 step.py 5"
        written = {"file_path": "receipt.json", "content": "full-write-body-" * 30,
                   "metadata": {"source": "fixture", "steps": [1, 2, 3, 4, 5]}}
        result = self.record([("Bash", {"command": command}), ("Write", written)])
        self.assertEqual(result["bash"], [command[:120]])
        self.assertEqual(result["tool_trace"], [
            {"name": "Bash", "input": {"command": command}},
            {"name": "Write", "input": written},
        ])
        self.assertFalse(result["trace_truncated"])

    def test_anthropic_test_token_is_redacted_in_all_trace_inputs(self):
        token = "trace-fixture-not-a-real-token-123456789"
        result = self.record(
            [("Bash", {"command": "printf " + token}),
             ("Write", {"file_path": "safe.txt", "content": token,
                        "metadata": {"token": token}})],
            {"ANTHROPIC_TRACE_TEST_TOKEN": token},
        )
        self.assertEqual(result["tool_trace"], [
            {"name": "Bash", "input": {"command": "printf [REDACTED]"}},
            {"name": "Write", "input": {"file_path": "safe.txt", "content": "[REDACTED]",
                                         "metadata": {"token": "[REDACTED]"}}},
        ])
        self.assertNotIn(token, json.dumps(result["tool_trace"]))
        self.assertFalse(result["trace_truncated"])

    def test_aggregate_trace_budget_declares_omission(self):
        first = {"file_path": "first.txt", "content": "a" * (140 * 1024)}
        second = {"file_path": "second.txt", "content": "b" * (140 * 1024)}
        result = self.record([("Write", first), ("Write", second), ("Read", {"file_path": "last.txt"})])
        self.assertTrue(result["trace_truncated"])
        self.assertEqual(result["tool_trace"], [{"name": "Write", "input": first}])
        self.assertEqual(result["tools"], {"Write": 2, "Read": 1})
        self.assertLessEqual(sum(len(json.dumps(entry).encode("utf-8"))
                                 for entry in result["tool_trace"]), 256 * 1024)

    def test_single_oversized_input_declares_omission(self):
        result = self.record([("Write", {"content": "z" * (256 * 1024)})])
        self.assertTrue(result["trace_truncated"])
        self.assertEqual(result["tool_trace"], [])
        self.assertEqual(result["tools"], {"Write": 1})

    def test_failed_result_keeps_diagnostics_despite_checker_accepting_written_output(self):
        token = "result-fixture-not-a-real-token-123456789"
        result = self.record([], {"ANTHROPIC_TRACE_TEST_TOKEN": token},
                             {"subtype": "error_max_turns", "is_error": True,
                              "errors": ["execution stopped " + token], "result": "failed " + token},
                             rc=1, write_output=True)
        self.assertTrue((Path(result["ws"]) / "created.txt").is_file())
        self.assertTrue(result["pass"])  # checker verdict remains separate from execution evidence
        self.assertEqual(result["rc"], 1)
        self.assertTrue(result["is_error"])
        self.assertEqual(result["subtype"], "error_max_turns")
        self.assertEqual(result["errors"], ["execution stopped [REDACTED]"])
        self.assertEqual(result["error"], "failed [REDACTED]")
        self.assertFalse(result["diagnostics_truncated"])
        self.assertNotIn(token, json.dumps(result))

    def test_result_diagnostics_are_bounded_and_declare_omission(self):
        self.assertEqual(run.result_diagnostics({"subtype": "success"}),
                         {"subtype": "success", "errors": [], "diagnostics_truncated": False})
        result = self.record([], result_event={"errors": ["x" * (20 * 1024)]})
        self.assertTrue(result["diagnostics_truncated"])
        self.assertLess(len(json.dumps(result["errors"])), 4096)


if __name__ == "__main__":
    unittest.main()
