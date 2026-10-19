import os
import tempfile
import unittest

from ledger import main


class LedgerTest(unittest.TestCase):
    def test_cut_off_step_is_not_rerun(self):
        p = os.path.join(tempfile.mkdtemp(), "s", "ledger.json")
        self.assertEqual(main([p, "begin", "1"]), 0)
        self.assertEqual(main([p, "done", "1"]), 0)
        self.assertEqual(main([p, "begin", "1"]), 3)   # done: skip
        self.assertEqual(main([p, "begin", "2"]), 0)   # cut off before done
        self.assertEqual(main([p, "begin", "2"]), 4)   # resume: must check, not rerun
        self.assertEqual(main([p, "done", "3"]), 2)    # done without begin
        self.assertEqual(main([p, "resolve", "2", "not-done"]), 0)
        self.assertEqual(main([p, "begin", "2"]), 0)   # checked absent: may run
        self.assertEqual(main([p, "resolve", "2", "done"]), 0)
        self.assertEqual(main([p, "begin", "2"]), 3)
        self.assertEqual(main([p, "resolve", "2", "done"]), 2)  # not pending
        self.assertEqual(main([p, "show"]), 0)


if __name__ == "__main__":
    unittest.main()
