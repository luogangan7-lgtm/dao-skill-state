"""public_bench.py without a model (CI only)."""
import json
import os
import tempfile
import unittest

from public_bench import accepted, bundle, names_itself, split_of


class PublicBench(unittest.TestCase):
    def test_a_request_naming_its_skill_is_too_easy(self):
        self.assertTrue(names_itself("Use pdf-tools to merge these", "pdf-tools"))
        self.assertTrue(names_itself("use the PDF tools to merge", "pdf-tools"))
        self.assertTrue(names_itself("run pdf_tools on it", "pdf-tools"))
        self.assertFalse(names_itself("merge these two PDFs into one", "pdf-tools"))
        self.assertFalse(names_itself("my-pdf-tools-fork is broken", "pdf-tools"))
        # a one-word name is the domain's own word
        self.assertFalse(names_itself("merge these two PDF files", "pdf"))

    def test_a_sibling_skill_is_also_accepted(self):
        self.assertEqual(accepted(["figma-use"]), ["figma", "figma-use"])
        self.assertEqual(accepted(["pdf"]), ["pdf"])

    def test_split_keeps_a_skills_requests_together(self):
        a, b = {"prompt": "x", "expect": ["pdf"]}, {"prompt": "y", "expect": ["pdf"]}
        self.assertEqual(split_of(a), split_of(b))

    def test_bundle_maps_names_to_every_same_named_id_and_flags_missing(self):
        reg = {"skills": [{"id": "thirdparty:aa", "name": "pdf"}, {"id": "thirdparty:bb", "name": "pdf"},
                          {"id": "docx", "name": "docx"}]}
        cases = {"cases": [{"prompt": "merge PDFs", "expect": ["pdf"]}, {"prompt": "hi", "expect": []}]}
        with tempfile.TemporaryDirectory() as d:
            paths = [os.path.join(d, n) for n in ("reg.json", "cases.json", "out.json")]
            json.dump(reg, open(paths[0], "w"))
            json.dump(cases, open(paths[1], "w"))
            self.assertEqual(bundle(*paths), 0)
            b = json.load(open(paths[2]))
            got = [c for s in ("dev", "held") for c in b[s]["cases"]]
            self.assertEqual(sorted(c["expect"] for c in got), [[], ["thirdparty:aa", "thirdparty:bb"]])
            cases["cases"].append({"prompt": "gone", "expect": ["removed-skill"]})
            json.dump(cases, open(paths[1], "w"))
            self.assertEqual(bundle(*paths), 1)


if __name__ == "__main__":
    unittest.main()
