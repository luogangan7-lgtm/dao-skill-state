#!/usr/bin/env python3
"""Selector experiment for `skill-hub hook` (D7): which local model best picks one skill or none.

Runs in CI only (`.github/workflows/selector-eval.yml`). Every selector sees the same frozen
candidates (skill-hub RRF top-10 per prompt). The threshold `tau` is tuned on the dev split only
and then applied once to the held-out split, so held-out numbers are not fitted.

Input: the decrypted bundle (`data.json.enc` holds private skill descriptions and prompts; the
key is the SELECTOR_EVAL_KEY Actions secret):
  {"dev"|"held": {"cases": [{id, prompt, expect: [ids]}], "cands": {case_id: [{id, description, score}]}}}
Output: results/<model>.json (per-case rows) and a markdown row on stdout.

Usage: python eval.py BUNDLE MODEL OUT_DIR    MODEL in: bge qwen3rr qwen3rr-skill mxbai gte
"""
import json
import math
import os
import sys
import time

# Every evaluated repository at a fixed commit, so reruns compare the same weights, configs and
# tokenizers (and gte's config, which chooses its remote code, cannot change underneath us).
REVISIONS = json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "revisions.json")))

QWEN_DEFAULT = "Given a web search query, retrieve relevant passages that answer the query"
QWEN_SKILL = ("Given a user's request to a coding agent, judge whether this skill (name: description) "
              "should be loaded to carry out the request; small talk or general questions need no skill")


def sigmoid(x):
    """Overflow-safe logistic function."""
    return 1 / (1 + math.exp(-x)) if x >= 0 else math.exp(x) / (1 + math.exp(x))


def load(model):
    """A function (query, docs) -> relevance in [0, 1] for each doc."""
    import torch
    import transformers
    # transformers 5 renamed torch_dtype to dtype; gte's remote code needs 4.x
    f32 = {("dtype" if int(transformers.__version__.split(".")[0]) >= 5 else "torch_dtype"): torch.float32}
    if model == "bge":
        from sentence_transformers import CrossEncoder
        r = "BAAI/bge-reranker-v2-m3"
        m = CrossEncoder(r, max_length=512, device="cpu", revision=REVISIONS[r])
        return lambda q, docs: [float(x) for x in m.predict([(q, d) for d in docs])]
    if model == "mxbai":
        from sentence_transformers import CrossEncoder
        r = "mixedbread-ai/mxbai-rerank-base-v2"
        m = CrossEncoder(r, device="cpu", revision=REVISIONS[r])
        # one scale for every case, decided from the model's configured activation: if predict
        # already applies a sigmoid the scores are probabilities, otherwise they are raw logits
        act = getattr(m, "activation_fn", None)
        is_prob = isinstance(act, torch.nn.Sigmoid)
        print(f"mxbai activation_fn={act} -> {'as is' if is_prob else 'sigmoid'}", file=sys.stderr)

        def fn(q, docs):
            raw = [float(x) for x in m.predict([(q, d) for d in docs])]
            return raw if is_prob else [sigmoid(x) for x in raw]
        return fn
    if model in ("qwen3rr", "qwen3rr-skill"):
        from transformers import AutoModelForCausalLM, AutoTokenizer
        instruction = QWEN_SKILL if model == "qwen3rr-skill" else QWEN_DEFAULT
        r = "Qwen/Qwen3-Reranker-0.6B"
        tok = AutoTokenizer.from_pretrained(r, padding_side="left", revision=REVISIONS[r])
        m = AutoModelForCausalLM.from_pretrained(r, revision=REVISIONS[r], **f32).eval()
        yes, no = tok.convert_tokens_to_ids("yes"), tok.convert_tokens_to_ids("no")
        pre = ("<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and the "
               "Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n<|im_start|>user\n")
        suf = "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"

        pre_ids = tok(pre, add_special_tokens=False)["input_ids"]
        suf_ids = tok(suf, add_special_tokens=False)["input_ids"]
        ids = lambda t: tok(t, add_special_tokens=False)["input_ids"]
        # pieces split before a space so " text" merges like whole-string tokenization does
        head_ids = ids(f"<Instruct>: {instruction}\n<Query>:")
        mid_ids = ids("\n<Document>:")
        budget = 1024 - len(pre_ids) - len(head_ids) - len(mid_ids) - len(suf_ids)

        def fn(q, docs):
            # query and document are cut separately, so a long query can never crowd out the
            # candidate (scores stay candidate-dependent), and the suffix always ends at the
            # yes/no answer position
            q_ids = ids(" " + q)[: max(budget // 2, 1)]
            rows = [pre_ids + head_ids + q_ids + mid_ids + ids(" " + d)[: max(budget - len(q_ids), 1)] + suf_ids
                    for d in docs]
            enc = tok.pad({"input_ids": rows}, padding=True, return_tensors="pt")
            with torch.no_grad():
                lg = m(**enc, logits_to_keep=1).logits[:, -1, :]  # only the answer position
            return torch.stack([lg[:, no], lg[:, yes]], dim=1).log_softmax(dim=1)[:, 1].exp().tolist()
        return fn
    if model == "gte":
        # remote code (Alibaba-NLP/new-impl) is pinned by the workflow: it downloads that repo, checks
        # the commit, then loads with HF_HUB_OFFLINE=1 so nothing newer can be fetched and run
        from transformers import AutoModelForSequenceClassification, AutoTokenizer
        r = "Alibaba-NLP/gte-multilingual-reranker-base"
        rev = REVISIONS[r]  # the inspected snapshot; its config chooses the remote code
        tok = AutoTokenizer.from_pretrained(r, revision=rev)
        m = AutoModelForSequenceClassification.from_pretrained(r, revision=rev, trust_remote_code=True,
                                                               code_revision=REVISIONS["Alibaba-NLP/new-impl"],
                                                               **f32).eval()

        def fn(q, docs):
            enc = tok([[q, d] for d in docs], padding=True, truncation=True, max_length=512, return_tensors="pt")
            with torch.no_grad():
                return torch.sigmoid(m(**enc).logits.view(-1).float()).tolist()
        return fn
    raise SystemExit(f"unknown model {model}")


def score_split(fn, split):
    rows = []
    for c in split["cases"]:
        hs = split["cands"][c["id"]]
        t = time.time()
        s = fn(c["prompt"], [f'{h["id"]}: {h["description"][:400]}' for h in hs]) if hs else []
        ms = (time.time() - t) * 1000
        # ties go to the earlier (retrieval-ranked) candidate, as in the hook
        best = max(range(len(s)), key=lambda i: (s[i], -i)) if s else None
        # no skill names or prompts in the rows: artifacts of a public repo are downloadable
        rows.append({"id": c["id"], "pos": bool(c["expect"]),
                     "top_ok": bool(s) and hs[best]["id"] in c["expect"],
                     "best": s[best] if s else None, "ms": ms})
    return rows


def picks(r, tau):
    return r["best"] is not None and r["best"] >= tau


def correct(r, tau):
    return (picks(r, tau) and r["top_ok"]) if r["pos"] else not picks(r, tau)


def tune(rows):
    """tau with the most correct dev cases; ties -> the widest gap, at its geometric middle."""
    vals = sorted({r["best"] for r in rows if r["best"] is not None})
    cuts = [0.0] + [math.sqrt(a * b) if a > 0 and b > 0 else (a + b) / 2 for a, b in zip(vals, vals[1:])] + [1.0]
    gap = {c: (min([v for v in vals if v >= c] or [1.0]) - max([v for v in vals if v < c] or [0.0])) for c in cuts}
    return max(cuts, key=lambda c: (sum(correct(r, c) for r in rows), gap[c]))


def summary(rows, tau):
    pos = [r for r in rows if r["pos"]]
    neg = [r for r in rows if not r["pos"]]
    picked = [r for r in rows if picks(r, tau)]
    ms = sorted(r["ms"] for r in rows)
    return {
        "all": f'{sum(correct(r, tau) for r in rows)}/{len(rows)}',
        "pos": f'{sum(correct(r, tau) for r in pos)}/{len(pos)}',
        "abstain": f'{sum(correct(r, tau) for r in neg)}/{len(neg)}',
        "top1": f'{sum(r["top_ok"] for r in pos)}/{len(pos)}',
        "false_abstain": sum(1 for r in pos if not picks(r, tau)),
        "wrong_skill": sum(1 for r in picked if r["pos"] and not r["top_ok"]),
        "false_fire": sum(1 for r in picked if not r["pos"]),
        "p50_ms": round(ms[len(ms) // 2]), "p95_ms": round(ms[min(len(ms) - 1, math.ceil(len(ms) * 0.95) - 1)]),
    }


def main():
    bundle_path, model, out = sys.argv[1:4]
    t = time.time()
    fn = load(model)  # before the private data is in this process
    load_s = time.time() - t
    bundle = json.load(open(bundle_path))
    fn("warm up", ["warm up"])
    dev, held = score_split(fn, bundle["dev"]), score_split(fn, bundle["held"])
    tau = tune(dev)
    res = {"model": model, "tau": tau, "load_s": round(load_s, 1),
           "dev": summary(dev, tau), "held": summary(held, tau),
           "rows": {"dev": dev, "held": held}}
    os.makedirs(out, exist_ok=True)
    json.dump(res, open(os.path.join(out, f"{model}.json"), "w"), indent=1)
    d, h = res["dev"], res["held"]
    print(f'| {model} | {tau:.4g} | {d["all"]} | **{h["all"]}** | {h["pos"]} | {h["abstain"]} | {h["top1"]} '
          f'| {h["false_abstain"]} | {h["wrong_skill"]} | {h["false_fire"]} | {h["p50_ms"]} / {h["p95_ms"]} | {res["load_s"]} |')


if __name__ == "__main__":
    main()
