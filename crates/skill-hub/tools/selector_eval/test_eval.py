"""The scoring rules of eval.py (no models needed). Runs in CI only."""
import unittest

from eval import BASELINE, correct, first_rank, jev_body, jev_scores, mcnemar, recall_at, rrf, run_name, summary, tune


def row(pos, top_ok, best):
    return {"id": "x", "pos": pos, "top_ok": top_ok, "best": best, "ms": 1.0}


class Rules(unittest.TestCase):
    def test_tau_separates_hits_from_chitchat_at_the_widest_gap(self):
        rows = [row(True, True, 0.6), row(True, True, 0.06), row(False, False, 0.004), row(False, False, 0.001)]
        tau = tune(rows)
        self.assertTrue(0.004 < tau <= 0.06, tau)
        self.assertTrue(all(correct(r, tau) for r in rows))

    def test_tau_works_on_cosines_that_can_be_negative(self):
        rows = [row(True, True, 0.71), row(False, False, 0.2), row(False, False, -0.1)]
        tau = tune(rows)
        self.assertTrue(0.2 < tau <= 0.71, tau)

    def test_tau_can_go_below_zero_when_every_hit_is_negative(self):
        # the first cut used to be 0.0, which abstained on every negative-cosine hit
        rows = [row(True, True, -0.1), row(True, True, -0.2), row(False, False, -0.6)]
        tau = tune(rows)
        self.assertTrue(-0.6 < tau <= -0.2, tau)
        self.assertTrue(all(correct(r, tau) for r in rows))

    def test_a_wrong_top_pick_is_never_correct_and_counts_as_wrong_skill(self):
        rows = [row(True, False, 0.9), row(False, False, 0.5)]
        s = summary(rows, 0.1)
        self.assertEqual((s["all"], s["wrong_skill"], s["false_fire"], s["false_abstain"]), ("0/2", 1, 1, 0))
        self.assertEqual(summary(rows, 0.95)["all"], "1/2")  # abstaining is right only for chit-chat

    def test_no_candidates_means_abstain(self):
        self.assertTrue(correct(row(False, False, None), 0.5))
        self.assertFalse(correct(row(True, False, None), 0.0))

    def test_rrf_matches_skill_hub(self):
        # dense.rs test: rrf([1,2,3],[2,4]) orders 2, 1, 4, 3
        self.assertEqual(rrf([1, 2, 3], [2, 4]), [2, 1, 4, 3])

    def test_mcnemar_counts_discordant_pairs(self):
        a = {"1": True, "2": True, "3": False, "4": True}
        b = {"1": False, "2": True, "3": True, "4": False}
        w, l, p = mcnemar(a, b)
        self.assertEqual((w, l), (2, 1))
        self.assertAlmostEqual(p, 1.0)
        self.assertEqual(mcnemar(a, a)[2], 1.0)

    def test_recall_counts_zero_based_ranks_strictly_below_k(self):
        lst = [7, 3, 9, 4]
        self.assertEqual(first_rank(lst, {9}), 2)
        self.assertIsNone(first_rank(lst, {42}))
        self.assertIsNone(first_rank(lst, set()))  # expected ids all missing from the registry
        ranks = [0, 2, None, 5]
        self.assertEqual((recall_at(ranks, 1), recall_at(ranks, 3), recall_at(ranks, 6)), (1, 2, 3))

    def test_the_baseline_is_the_shipped_configuration(self):
        self.assertEqual(BASELINE, run_name("qwen3rr-skill", "bgem3", 10))
        self.assertEqual(BASELINE, "qwen3rr-skill@bgem3@k10")

    def test_jev_body_matches_the_hook(self):
        long = "h" * 3000 + "TAIL"
        none, body = jev_body(long, [{"id": "a", "description": "d" * 500}, {"id": "(no skill)", "description": ""}])
        self.assertEqual(none, "(no skill)*")
        crit = body["questions"]["skill"]["criteria"]
        self.assertEqual((len(crit["a"]), crit["(no skill)"]), (400, "(no skill)"))
        self.assertIn(none, crit)
        self.assertEqual(len(body["state"]), 2500)
        self.assertTrue(body["state"].endswith("TAIL"))

    def test_jev_scores_follow_the_hook(self):
        c = [{"id": "a"}, {"id": "b"}]
        resp = lambda ans: {"answers": {"skill": ans}}
        self.assertEqual(jev_scores(resp({"choice": "(no skill)"}), c, "(no skill)"), [])
        # the choice wins even when Jev reports a higher probability elsewhere
        self.assertEqual(jev_scores(resp({"choice": "b", "probabilities": {"a": 0.9, "b": 0.1}}), c, "(no skill)"),
                         [0.0, 1.0])
        self.assertEqual(jev_scores(resp({"choice": "zzz"}), c, "(no skill)"), [])
        rows = [{"pos": True, "top_ok": True, "best": 1.0}, {"pos": False, "top_ok": False, "best": None}]
        self.assertTrue(all(correct(r, tune(rows)) for r in rows))

if __name__ == "__main__":
    unittest.main()
