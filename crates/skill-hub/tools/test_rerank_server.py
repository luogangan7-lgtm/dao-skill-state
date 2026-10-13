"""Model-free checks of rerank_server.py: score scaling and request batching (fake scorers)."""
import struct
import threading
import time
import unittest

from rerank_server import HOOK_TAU, Batcher, split, to_hook_scale


class ScaleTest(unittest.TestCase):
    def test_tuned_tau_lands_on_the_hook_tau_and_order_is_kept(self):
        self.assertAlmostEqual(to_hook_scale(0.345, 0.345), HOOK_TAU, places=6)
        self.assertAlmostEqual(to_hook_scale(0.5, HOOK_TAU), 0.5, places=6)  # same tau: unchanged
        xs = [0.0, 0.01, 0.2, 0.345, 0.5, 0.9, 1.0]
        ys = [to_hook_scale(x, 0.345) for x in xs]
        self.assertEqual(ys, sorted(ys))
        self.assertTrue(all(0 < y < 1 for y in ys))
        # a decision at tau is the same before and after, also as the hook reads it (f32)
        f32 = lambda v: struct.unpack("f", struct.pack("f", v))[0]
        xs += [0.345 - 1e-12, 0.345 + 1e-12, 0.3449999]
        ys = [to_hook_scale(x, 0.345) for x in xs]
        self.assertEqual([x >= 0.345 for x in xs], [y >= HOOK_TAU for y in ys])
        self.assertEqual([x >= 0.345 for x in xs], [f32(y) >= f32(HOOK_TAU) for y in ys])

    def test_split_returns_one_list_per_request(self):
        self.assertEqual(split([("a", ["x", "y"]), ("b", []), ("c", ["z"])], [1, 2, 3]), [[1, 2], [], [3]])


class BatcherTest(unittest.TestCase):
    def test_simultaneous_requests_share_passes_and_get_their_own_scores(self):
        passes = []

        def score(pairs):
            passes.append(len(pairs))
            time.sleep(0.2)  # the first pass is busy while the others queue up
            return [[len(q) + i for i in range(len(texts))] for q, texts in pairs]
        b = Batcher(score, max_rows=1000, max_wait_s=10)
        out = {}
        ts = [threading.Thread(target=lambda n=n: out.__setitem__(n, b("q" * n, ["t"] * 3))) for n in range(1, 13)]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
        self.assertEqual(out, {n: [n, n + 1, n + 2] for n in range(1, 13)})
        self.assertLess(len(passes), 12)  # batched, not one pass per request
        self.assertEqual(sum(passes), 12)

    def test_stale_requests_are_dropped_and_errors_reach_the_caller(self):
        def slow(pairs):
            time.sleep(0.3)
            return [[0.5] * len(t) for _, t in pairs]
        b = Batcher(slow, max_rows=1, max_wait_s=0.1)  # one request per pass
        out = []
        ts = [threading.Thread(target=lambda: out.append(b("q", ["t"]))) for _ in range(3)]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
        self.assertIn(None, out)  # waited past max_wait: answered busy, not computed
        self.assertIn([0.5], out)

        def broken(pairs):
            raise RuntimeError("model failed")
        with self.assertRaises(RuntimeError):
            Batcher(broken, 64, 10)("q", ["t"])


if __name__ == "__main__":
    unittest.main()
