#!/usr/bin/env python3
"""Why does Qwen3-Reranker in float32 on the Apple GPU (MPS) score lower than on CPU? (D7 round 6:
88/112 vs 98.) Compares P(yes) for the same public, synthetic rows across: CPU float32 (reference),
MPS float32, MPS float16; each batched with left padding (as served) and one row at a time (no
padding). Prints numbers only. Runs in CI (rerank-e2e.yml), not on a developer machine.

Depends on: torch, transformers, rerank_server.py (the served prompt rows).
"""
import sys

import torch
from transformers import AutoModelForCausalLM, AutoTokenizer

import rerank_server as rs

QUERIES = ["Set up a new Rust CLI tool with clap for argument parsing",
           "帮我把这些文案和图片整理成一篇小红书笔记",
           "Thanks, that is all for now",
           "Speed up this slow SQL query that joins orders and customers"]
TEXTS = ["rust-cli-scaffold: Scaffold a Rust command-line application with cargo and clap argument parsing.",
         "xiaohongshu-note: Prepare a Xiaohongshu (小红书) image-text note package from existing copy and images.",
         "sql-query-tuning: Diagnose and speed up slow SQL queries with indexes and EXPLAIN plans.",
         "pdf-merge: Merge, split and reorder PDF files. " + "Handles bookmarks and page ranges. " * 6,
         "a: b"]


LONG_Q = ("Here is the build log from CI:\n" + "error[E0425]: cannot find value `x` in this scope at src/main.rs:12\n" * 40
          + "Please fix the build and set up clap argument parsing for the Rust CLI.")
LONG_T = [t + " " + ("Covers setup, conventions, common errors and the commands to run, with examples. " * 5)
          for t in TEXTS] * 2  # 10 texts of ~400+ chars, as the hook sends


def scores(dev, dtype, attn, batched, queries=None, texts=None):
    tok = AutoTokenizer.from_pretrained(rs.QWEN, padding_side="left", revision=rs.QWEN_REVISION)
    m = AutoModelForCausalLM.from_pretrained(rs.QWEN, revision=rs.QWEN_REVISION, dtype=dtype,
                                             attn_implementation=attn).to(dev).eval()
    yes, no, rows = rs.qwen3_rows(tok)

    def run(rr):
        enc = tok.pad({"input_ids": rr}, padding=True, return_tensors="pt").to(dev)
        with torch.no_grad():
            lg = m(**enc, logits_to_keep=1).logits[:, -1, :].float()
        return (lg[:, yes] - lg[:, no]).cpu().tolist()  # logit of P(yes)
    out = []
    for q in queries or QUERIES:
        rr = rows(q, texts or TEXTS)
        out += run(rr) if batched else [run([r])[0] for r in rr]
    return out


def main():
    ref = scores("cpu", torch.float32, "eager", True)
    ref1 = scores("cpu", torch.float32, "eager", False)
    print(f"cpu fp32 batched vs one-by-one: max |dlogit| {max(abs(a - b) for a, b in zip(ref, ref1)):.4f}")
    if not torch.backends.mps.is_available():
        sys.exit("no MPS")
    for name, dtype in (("fp32", torch.float32), ("fp16", torch.float16)):
        for batched in (True, False):
            s = scores("mps", dtype, "eager", batched)
            d = [a - b for a, b in zip(s, ref)]
            print(f"mps {name} {'batched' if batched else 'one-by-one'}: max |dlogit| {max(map(abs, d)):.4f} "
                  f"mean dlogit {sum(d) / len(d):+.4f}  "
                  f"P(yes) flips at 0.865: {sum((x > 1.857) != (y > 1.857) for x, y in zip(s, ref))}/{len(d)}")
    # hook-sized rows: a ~2,000-char prompt (head and tail kept) against 10 long texts
    for name, dtype in (("fp32", torch.float32), ("fp16", torch.float16)):
        lr = scores("cpu", torch.float32, "eager", True, [LONG_Q], LONG_T)
        s = scores("mps", dtype, "eager", True, [LONG_Q], LONG_T)
        d = [a - b for a, b in zip(s, lr)]
        print(f"long rows mps {name}: max |dlogit| {max(map(abs, d)):.4f} mean dlogit {sum(d) / len(d):+.4f} "
              f"flips at 0.865: {sum((x > 1.857) != (y > 1.857) for x, y in zip(s, lr))}/{len(d)}")
    # uneven rows, as real candidates are: one very long text pads the short ones by hundreds of tokens
    mixed = TEXTS[:4] + ["long-skill: " + "Handles every step of the release, from changelog to tags. " * 30]
    for name, dtype in (("fp32", torch.float32), ("fp16", torch.float16)):
        for q in (QUERIES[0], LONG_Q):
            ref_m = scores("cpu", torch.float32, "eager", True, [q], mixed)
            s = scores("mps", dtype, "eager", True, [q], mixed)
            nan = sum(x != x for x in s)
            d = [a - b for a, b in zip(s, ref_m) if a == a]
            print(f"uneven rows mps {name} ({'long' if q is LONG_Q else 'short'} prompt): NaN {nan}/{len(s)} "
                  f"max |dlogit| {max(map(abs, d)) if d else float('nan'):.4f}")
    # the uneven long case again, row by row on the GPU (no padding, small attention), and on MLX,
    # the Apple Silicon default: does either avoid the GPU command-buffer errors seen above?
    import math
    ref_u = scores("cpu", torch.float32, "eager", True, [LONG_Q], mixed)
    for name, dtype in (("fp32", torch.float32), ("fp16", torch.float16)):
        s = scores("mps", dtype, "eager", False, [LONG_Q], mixed)
        print(f"uneven long one-by-one mps {name}: max |dlogit| {max(abs(a - b) for a, b in zip(s, ref_u)):.4f}")
    try:
        mlx = rs.qwen3_mlx_scorer()
    except ImportError:
        print("mlx not installed")
    else:
        for label, q, tx in (("uneven long", LONG_Q, mixed), ("long", LONG_Q, LONG_T)):
            ref_l = ref_u if tx is mixed else scores("cpu", torch.float32, "eager", True, [q], tx)
            p = mlx([(q, tx)])[0]
            lg = [math.log(max(x, 1e-12) / max(1 - x, 1e-12)) for x in p]
            print(f"{label} mlx 4bit: max |dlogit| {max(abs(a - b) for a, b in zip(lg, ref_l)):.4f} "
                  f"flips at 0.865 (raw scale, informational): {sum((x > 1.857) != (y > 1.857) for x, y in zip(lg, ref_l))}/{len(lg)}")
    # per padded row: is the error tied to how much left padding a row got?
    s = scores("mps", torch.float32, "eager", True)
    for qi, q in enumerate(QUERIES):
        lens = [len(r) for r in rs.qwen3_rows(AutoTokenizer.from_pretrained(
            rs.QWEN, revision=rs.QWEN_REVISION))[2](q, TEXTS)]
        n = max(lens)
        cells = " ".join(f"pad{n - L}:{s[qi * len(TEXTS) + i] - ref[qi * len(TEXTS) + i]:+.3f}"
                         for i, L in enumerate(lens))
        print(f"mps fp32 query {qi}: {cells}")


if __name__ == "__main__":
    main()
