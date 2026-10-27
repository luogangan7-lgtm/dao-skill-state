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

    def test_description_without_a_trigger_warns(self):
        # created skills whose description never said when to use them were not loaded (eval H, run 40445025600)
        with tempfile.TemporaryDirectory() as tmp:
            bare = write_skill(Path(tmp), "pages", "name: pages\ndescription: Write and review web pages so they pass review with readable contrast.\n")
            result = check_skill(bare)
            self.assertEqual(result["status"], "valid", result)
            self.assertIn("when to use it", next(x["message"] for x in result["warnings"] if x["kind"] == "trigger"))
            for text in ("Write and review web pages with readable contrast. Use when an agent changes a page.",
                         "检查网页的对比度、字号和手机布局，在修改任何网页时使用这个技能。", "short"):
                ok = write_skill(Path(tmp), f"t{len(text)}", f"name: t{len(text)}\ndescription: {text}\n")
                self.assertFalse([x for x in check_skill(ok)["warnings"] if x["kind"] == "trigger"], text)

    def test_quoted_ratios_without_a_script_warn(self):
        # a skill quoting a wrong contrast table failed 4 of 4 use runs; one shipping a script passed (eval H, run 40471493176)
        with tempfile.TemporaryDirectory() as tmp:
            skill = write_skill(Path(tmp), "pages", "name: pages\ndescription: short\n")
            (skill / "references").mkdir()
            (skill / "references" / "contrast.md").write_text("#111827 16.7:1, #4b5563 10.9:1, #6b7280 7.2:1, minimum 4.5:1\n", encoding="utf-8")
            result = check_skill(skill)
            self.assertEqual(result["status"], "valid", result)
            self.assertIn("scripts/", next(x["message"] for x in result["warnings"] if x["kind"] == "unchecked-values"))
            (skill / "scripts").mkdir()
            (skill / "scripts" / "contrast.py").write_text("print()\n", encoding="utf-8")
            self.assertFalse([x for x in check_skill(skill)["warnings"] if x["kind"] == "unchecked-values"])
            thresholds = write_skill(Path(tmp), "rules", "name: rules\ndescription: short\n")
            (thresholds / "SKILL.md").write_text("---\nname: rules\ndescription: short\n---\nBody text 4.5:1, large text 3:1, AAA 7:1.\n", encoding="utf-8")
            self.assertFalse([x for x in check_skill(thresholds)["warnings"] if x["kind"] == "unchecked-values"])

    def test_script_not_named_in_skill_md_warns(self):
        # writers added scripts/contrast.py to clear unchecked-values but never named it; no use run ran it (eval H, run 40496212294)
        with tempfile.TemporaryDirectory() as tmp:
            skill = write_skill(Path(tmp), "pages", "name: pages\ndescription: short\n")
            (skill / "scripts").mkdir()
            (skill / "scripts" / "contrast.py").write_text("print()\n", encoding="utf-8")
            result = check_skill(skill)
            self.assertEqual(result["status"], "valid", result)
            self.assertIn("contrast.py", next(x["message"] for x in result["warnings"] if x["kind"] == "unnamed-script"))
            (skill / "SKILL.md").write_text("---\nname: pages\ndescription: short\n---\nRun `python3 scripts/contrast.py FG BG` before writing colours.\n", encoding="utf-8")
            self.assertFalse([x for x in check_skill(skill)["warnings"] if x["kind"] == "unnamed-script"])

    def test_missing_name_and_directory_mismatch_are_errors(self):
        with tempfile.TemporaryDirectory() as tmp:
            skill = write_skill(Path(tmp), "wrong-dir", "description: short\n")
            result = check_skill(skill)
            self.assertEqual(result["status"], "invalid")
            self.assertTrue(any(item["kind"] == "name" for item in result["errors"]))

    def test_name_mismatch_says_how_to_fix_without_moving_the_folder(self):
        # agents renamed or nested a requested folder to satisfy this error (eval B, run 38610369890)
        with tempfile.TemporaryDirectory() as tmp:
            skill = write_skill(Path(tmp), "new-skill", "name: clean-csv\ndescription: short\n")
            message = next(item["message"] for item in check_skill(skill)["errors"] if item["kind"] == "name")
            self.assertIn("set name to 'new-skill'", message)
            self.assertIn("'clean-csv'", message)
            self.assertIn("keep a folder the user asked for", message)
            # an invalid folder name must not be suggested as the skill name
            skill = write_skill(Path(tmp), "My_Skill", "name: my-skill\ndescription: short\n")
            message = next(item["message"] for item in check_skill(skill)["errors"] if item["kind"] == "name")
            self.assertNotIn("set name to", message)
            self.assertIn("not a valid skill name", message)

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
