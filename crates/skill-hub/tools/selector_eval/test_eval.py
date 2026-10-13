"""The scoring rules of eval.py (no models needed)."""
import unittest

from eval import correct, summary, tune


def row(pos, top_ok, best):
    return {"id": "x", "pos": pos, "top_ok": top_ok, "best": best, "ms": 1.0}


class Rules(unittest.TestCase):
    def test_tau_separates_hits_from_chitchat_at_the_widest_gap(self):
        rows = [row(True, True, 0.6), row(True, True, 0.06), row(False, False, 0.004), row(False, False, 0.001)]
        tau = tune(rows)
        self.assertTrue(0.004 < tau <= 0.06, tau)
        self.assertTrue(all(correct(r, tau) for r in rows))

    def test_a_wrong_top_pick_is_never_correct_and_counts_as_wrong_skill(self):
        rows = [row(True, False, 0.9), row(False, False, 0.5)]
        s = summary(rows, 0.1)
        self.assertEqual((s["all"], s["wrong_skill"], s["false_fire"], s["false_abstain"]), ("0/2", 1, 1, 0))
        self.assertEqual(summary(rows, 0.95)["all"], "1/2")  # abstaining is right only for chit-chat

    def test_no_candidates_means_abstain(self):
        self.assertTrue(correct(row(False, False, None), 0.5))
        self.assertFalse(correct(row(True, False, None), 0.0))


if __name__ == "__main__":
    unittest.main()
