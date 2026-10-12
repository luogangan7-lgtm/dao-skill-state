#!/usr/bin/env python3
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from check_skill import check_skill


SCRIPT = Path(__file__).with_name("check_skill.py")


def write_skill(root: Path, name: str, frontmatter: str) -> Path:
    skill = root / name
    skill.mkdir()
    (skill / "SKILL.md").write_text(f"---\n{frontmatter}---\nSee [guide](refs/guide.md).\n", encoding="utf-8")
    return skill


class CheckSkillTests(unittest.TestCase):
    def test_valid_nested_metadata_optional_fields_and_plain_link(self):
        with tempfile.TemporaryDirectory() as tmp:
            skill = write_skill(Path(tmp), "dao-skill-state", """name: dao-skill-state
description: Read-only format checks for Agent Skills.
license: MIT
compatibility: Python 3.11+
allowed-tools: read
metadata:
  owner: dao
  tier: state
""")
            result = check_skill(skill)
            self.assertEqual(result["status"], "valid", result)
            self.assertFalse(result["errors"])

    def test_unicode_name_is_valid_but_warns_portability(self):
        with tempfile.TemporaryDirectory() as tmp:
            skill = write_skill(Path(tmp), "道-skill", "name: 道-skill\ndescription: short\n")
            result = check_skill(skill)
            self.assertEqual(result["status"], "valid", result)
            self.assertTrue(any(item["kind"] == "portability" for item in result["warnings"]))

    def test_missing_name_and_directory_mismatch_are_errors(self):
        with tempfile.TemporaryDirectory() as tmp:
            skill = write_skill(Path(tmp), "wrong-dir", "description: short\n")
            result = check_skill(skill)
            self.assertEqual(result["status"], "invalid")
            self.assertTrue(any(item["kind"] == "name" for item in result["errors"]))

    def test_duplicate_key_bad_type_and_nested_non_string_metadata_fail(self):
        cases = [
            "name: x-{i}\nname: x-{i}\ndescription: short\n",
            "name: x-{i}\ndescription:\n  - list\n",
            "name: x-{i}\ndescription: short\nmetadata:\n  nested:\n    bad: value\n",
        ]
        with tempfile.TemporaryDirectory() as tmp:
            for index, template in enumerate(cases):
                with self.subTest(index=index):
                    frontmatter = template.format(i=index)
                    skill = write_skill(Path(tmp), f"x-{index}", frontmatter)
                    self.assertEqual(check_skill(skill)["status"], "invalid")

    def test_dangerous_tag_and_broken_frontmatter_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            dangerous = write_skill(Path(tmp), "danger", "name: danger\ndescription: !!python/object/apply:os.system ['id']\n")
            self.assertEqual(check_skill(dangerous)["status"], "invalid")
            broken = Path(tmp) / "broken"
            broken.mkdir()
            (broken / "SKILL.md").write_text("---\nname: broken\ndescription: short\n", encoding="utf-8")
            self.assertEqual(check_skill(broken)["status"], "invalid")

    def test_cli_json_and_exit_codes(self):
        with tempfile.TemporaryDirectory() as tmp:
            valid = write_skill(Path(tmp), "ok", "name: ok\ndescription: short\n")
            proc = subprocess.run([sys.executable, str(SCRIPT), str(valid)], capture_output=True, text=True)
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(json.loads(proc.stdout)["status"], "valid")
            proc = subprocess.run([sys.executable, str(SCRIPT), str(Path(tmp) / "missing")], capture_output=True, text=True)
            self.assertEqual(proc.returncode, 2)

    def test_current_directory_and_uppercase(self):
        with tempfile.TemporaryDirectory() as tmp:
            valid = write_skill(Path(tmp), "ok", "name: ok\ndescription: short\n")
            proc = subprocess.run([sys.executable, str(SCRIPT), "."], cwd=valid, capture_output=True, text=True)
            self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
            upper = write_skill(Path(tmp), "Upper", "name: Upper\ndescription: short\n")
            self.assertEqual(check_skill(upper)["status"], "invalid")

    def test_blank_description_and_invalid_keys(self):
        cases = [
            "name: x-{i}\ndescription: '   '\n",
            "name: x-{i}\ndescription: short\n12: value\n",
            "name: x-{i}\ndescription: short\n? [a, b]\n: value\n",
        ]
        with tempfile.TemporaryDirectory() as tmp:
            for index, template in enumerate(cases):
                with self.subTest(index=index):
                    skill = write_skill(Path(tmp), f"x-{index}", template.format(i=index))
                    proc = subprocess.run([sys.executable, str(SCRIPT), str(skill)], capture_output=True, text=True)
                    self.assertEqual(proc.returncode, 1, proc.stderr)
                    self.assertEqual(json.loads(proc.stdout)["status"], "invalid")

    def test_unreadable_encoding_and_resource_limit(self):
        with tempfile.TemporaryDirectory() as tmp:
            skill = write_skill(Path(tmp), "bad-bytes", "name: bad-bytes\ndescription: short\n")
            (skill / "SKILL.md").write_bytes(b"\xff\xfe")
            self.assertEqual(check_skill(skill)["status"], "uncheckable")
            (skill / "SKILL.md").write_bytes(b"a" * (1024 * 1024 + 1))
            self.assertEqual(check_skill(skill)["status"], "uncheckable")

    def test_missing_dependency_has_json_exit_two(self):
        proc = subprocess.run([sys.executable, "-S", str(SCRIPT)], capture_output=True, text=True)
        self.assertEqual(proc.returncode, 2, proc.stderr)
        self.assertEqual(json.loads(proc.stdout)["status"], "uncheckable")

    def test_scalar_delimiter_limits_and_unknown_field_warning(self):
        with tempfile.TemporaryDirectory() as tmp:
            valid = write_skill(Path(tmp), "text", "name: text\ndescription: |\n  ---\n  Read supplied text.\ncustom-host-field: optional\n")
            self.assertEqual(check_skill(valid)["status"], "valid")
            self.assertTrue(any(x["kind"] == "unknown-field" for x in check_skill(valid)["warnings"]))
            wrong = write_skill(Path(tmp), "different", "name: mismatch\ndescription: short\n")
            self.assertEqual(check_skill(wrong)["status"], "invalid")
            for size in (1024, 1025):
                skill = write_skill(Path(tmp), f"len-{size}", f"name: len-{size}\ndescription: {'x' * size}\n")
                self.assertEqual(check_skill(skill)["status"], "valid" if size == 1024 else "invalid")


if __name__ == "__main__":
    unittest.main()
