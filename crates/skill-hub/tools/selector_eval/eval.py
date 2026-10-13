#!/usr/bin/env python3
"""Selector experiment for `skill-hub hook` (D7): which model (local, or hosted Jev) best picks one skill or none.

Runs in CI only (`.github/workflows/selector-eval.yml`); nothing here runs on a developer machine.

  eval.py cands  BUNDLE SKILL_HUB OUT   rebuild every case's candidates the way skill-hub ranks
                                        them: BM25 top-50 (the skill-hub binary) fused by RRF
                                        (K=60) with a dense top-200, as in dense.rs; two dense
                                        models (bge-m3, as shipped, and Qwen3-Embedding), top-16
                                        kept, recall@k of every list printed
  eval.py run    BUNDLE MODEL OUT_DIR [SRC K]   score one selector on retrieval variant SRC
                                        (bgem3|qwen3emb) at depth K; tau tuned on dev only
  eval.py report OUT_DIR                markdown table + paired exact McNemar tests

Bundle (decrypted from data.json.enc; key = SELECTOR_EVAL_KEY in the `selector-eval` environment):
  {"registry": <skill-hub registry>, "dev"|"held": {"cases": [{id, prompt, expect: [ids]}],
   "cands": {case_id: [{id, description, score}]}}}   ("cands" is added by the `cands` step)
Rows, artifacts and the summary carry case ids and numbers only, never skill names or prompts.
"""
import json
import math
import os
import subprocess
import sys
import time

# Every evaluated repository at a fixed commit, so reruns compare the same weights and tokenizers.
REVISIONS = json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "revisions.json")))

QWEN_DEFAULT = "Given a web search query, retrieve relevant passages that answer the query"
QWEN_SKILL = ("Given a user's request to a coding agent, judge whether this skill (name: description) "
              "should be loaded to carry out the request; small talk or general questions need no skill")
QWEN_EMB_PROMPT = ("Instruct: Given a user's request to a coding agent, retrieve the skill that should be "
                   "loaded to carry it out\nQuery:")
LAYA_QUESTION = ("Which skill should a coding agent load to carry out this request? "
                 "Choose none when the request needs no skill or no listed skill fits.")
LAYA_DESC_CHARS = 150
# Laya splits its question head evenly across options; the shipped head (192/256 tokens) leaves
# ~16-21 tokens per option for 11 options, so both limits are raised: 640 head tokens for the
# options, the rest of 1,024 for the request (both encoders read far longer inputs)
LAYA_MAX_LEN, LAYA_HEAD = 1024, 640
MODELS = ["bge", "qwen3rr", "qwen3rr-skill", "gte-mbert", "qwen3emb", "laya-ml", "laya-en", "laya-td", "jev"]
# Jev (TypeSafe, hosted): the request skill-hub hook.rs sends (jev_body), with the same limits
JEV_URL = "https://api.typesafe.ai/v1/systemone"
JEV_NONE = "(no skill)"
PROMPT_CHARS, DESC_CHARS = 2500, 400
JEV_OFF_SET = [0]  # answers outside the option set (counted, printed as a number)


def bound_prompt(p):
    """hook.rs bound_prompt: head and tail of a long prompt, PROMPT_CHARS at most."""
    gap = "\n…\n"
    if len(p) <= PROMPT_CHARS:
        return p
    keep = PROMPT_CHARS - len(gap)
    return p[: keep // 2] + gap + p[len(p) - (keep - keep // 2):]


def jev_body(q, cands, model="jev-latest"):
    none = JEV_NONE
    while any(h["id"] == none for h in cands):
        none += "*"
    crit = {h["id"]: h["description"][:DESC_CHARS] or h["id"] for h in cands}
    crit[none] = "No listed skill clearly fits this request; answer directly without a skill."
    return none, {"model": model, "state": bound_prompt(q), "questions": {"skill": {
        "type": "choice", "criteria": crit,
        "instructions": "Which skill should an AI agent load to handle this request? "
                        f"Choose {none} if no listed skill clearly fits."}}}


def jev_scores(resp, cands, none):
    """The hook's decision as scores: 1.0 for Jev's choice, 0 elsewhere (the hook has no tau, so
    any cut in (0, 1] reproduces it); [] when Jev abstains or answers outside the options (the
    hook then injects nothing / falls back to its lexical rule; counted here as no pick)."""
    ans = resp["answers"]["skill"]
    label = next((ans[k] for k in ("answer", "label", "choice") if isinstance(ans.get(k), str)), None)
    ids = [h["id"] for h in cands]
    if label == none or label not in ids:
        if label != none:
            JEV_OFF_SET[0] += 1
        return []
    return [1.0 if i == label else 0.0 for i in ids]


def sigmoid(x):
    """Overflow-safe logistic function."""
    return 1 / (1 + math.exp(-x)) if x >= 0 else math.exp(x) / (1 + math.exp(x))


def text(h, n=400):
    return f'{h["id"]}: {h["description"][:n]}'


def cross_encoder(repo, **kw):
    """A sentence-transformers cross-encoder scored on one probability scale for every case."""
    import torch
    from sentence_transformers import CrossEncoder
    m = CrossEncoder(repo, device="cpu", revision=REVISIONS[repo], **kw)
    is_prob = isinstance(getattr(m, "activation_fn", None), torch.nn.Sigmoid)
    print(f"{repo} activation -> {'as is' if is_prob else 'sigmoid'}", file=sys.stderr)

    def fn(q, cands):
        raw = [float(x) for x in m.predict([(q, text(h)) for h in cands])]
        return raw if is_prob else [sigmoid(x) for x in raw]
    return fn


def load(model):
    """A function (query, candidates) -> one score per candidate (higher = better)."""
    import torch
    if model == "bge":
        return cross_encoder("BAAI/bge-reranker-v2-m3", max_length=512)
    if model == "gte-mbert":
        return cross_encoder("Alibaba-NLP/gte-reranker-modernbert-base", max_length=512)
    if model == "qwen3emb":
        from sentence_transformers import SentenceTransformer
        r = "Qwen/Qwen3-Embedding-0.6B"
        m = SentenceTransformer(r, device="cpu", revision=REVISIONS[r])

        def fn(q, cands):
            qv = m.encode([q], prompt=QWEN_EMB_PROMPT, normalize_embeddings=True)
            dv = m.encode([text(h) for h in cands], normalize_embeddings=True)
            return [float(x) for x in (dv @ qv[0])]  # cosine
        return fn
    if model in ("qwen3rr", "qwen3rr-skill"):
        from transformers import AutoModelForCausalLM, AutoTokenizer
        instruction = QWEN_SKILL if model == "qwen3rr-skill" else QWEN_DEFAULT
        r = "Qwen/Qwen3-Reranker-0.6B"
        tok = AutoTokenizer.from_pretrained(r, padding_side="left", revision=REVISIONS[r])
        m = AutoModelForCausalLM.from_pretrained(r, revision=REVISIONS[r], dtype=torch.float32).eval()
        yes, no = tok.convert_tokens_to_ids("yes"), tok.convert_tokens_to_ids("no")
        pre = ("<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and the "
               "Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n<|im_start|>user\n")
        suf = "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
        ids = lambda t: tok(t, add_special_tokens=False)["input_ids"]
        pre_ids, suf_ids = ids(pre), ids(suf)
        # pieces split before a space so " text" merges like whole-string tokenization does
        head_ids = ids(f"<Instruct>: {instruction}\n<Query>:")
        mid_ids = ids("\n<Document>:")
        budget = 1024 - len(pre_ids) - len(head_ids) - len(mid_ids) - len(suf_ids)

        def fn(q, cands):
            # query and document are cut separately (a long query can't crowd out the candidate);
            # the suffix always ends at the yes/no answer position
            q_ids = ids(" " + q)[: max(budget // 2, 1)]
            rows = [pre_ids + head_ids + q_ids + mid_ids + ids(" " + text(h))[: max(budget - len(q_ids), 1)]
                    + suf_ids for h in cands]
            enc = tok.pad({"input_ids": rows}, padding=True, return_tensors="pt")
            with torch.no_grad():
                lg = m(**enc, logits_to_keep=1).logits[:, -1, :]  # only the answer position
            return torch.stack([lg[:, no], lg[:, yes]], dim=1).log_softmax(dim=1)[:, 1].exp().tolist()
        return fn
    if model in ("laya-ml", "laya-en", "laya-td"):
        import laya
        r = "convaiinnovations/laya"
        sub = {"laya-ml": "multilingual", "laya-en": None, "laya-td": "typed-decisions"}[model]
        agent = laya.load(r, device="cpu", subfolder=sub, revision=REVISIONS[r])

        def fn(q, cands):
            # one typed choice over the candidates plus an explicit "none" (a choice's probabilities
            # are conditional on the option set, so abstaining needs an option of its own)
            none = "none" if all(h["id"] != "none" for h in cands) else "no_skill_needed"
            crit = {h["id"]: h["description"][:LAYA_DESC_CHARS] for h in cands}
            crit[none] = "no skill is needed: small talk, a general question, or none of these skills fits"
            qs = {"skill": {"type": "choice", "instructions": LAYA_QUESTION, "criteria": crit}}
            p = agent.predict(q, qs, max_len=LAYA_MAX_LEN, head_max_len=LAYA_HEAD)["answers"]["skill"]["probabilities"]
            return [float(p.get(h["id"], 0.0)) for h in cands]
        return fn
    if model == "jev":
        import http.client
        import urllib.request
        key = os.environ.get("JEV_API")
        if not key:
            raise SystemExit("JEV_API is not set")

        def fn(q, cands):
            none, body = jev_body(q, cands)
            req = urllib.request.Request(JEV_URL, data=json.dumps(body).encode(), method="POST", headers={
                "Authorization": f"Bearer {key}", "Content-Type": "application/json"})
            for attempt in range(4):
                try:
                    with urllib.request.urlopen(req, timeout=30) as r:
                        return jev_scores(json.load(r), cands, none)
                except (OSError, http.client.HTTPException) as e:  # URLError, timeouts, resets
                    # status codes only: the body could echo the request
                    code = getattr(e, "code", None)
                    if attempt == 3 or (code is not None and code not in (429, 500, 502, 503, 529)):
                        raise RuntimeError(f"Jev call failed (HTTP {code})") from None
                    time.sleep(2 ** attempt * 2)
        return fn
    raise SystemExit(f"unknown model {model}")


def score_split(fn, split):
    rows = []
    for c in split["cases"]:
        hs = split["cands"][c["id"]]
        t = time.time()
        try:
            s = fn(c["prompt"], hs) if hs else []
        except Exception as e:  # the case id only: a traceback could carry the private prompt
            raise SystemExit(f"selector failed on case {c['id']}: {type(e).__name__}: "
                             f"{e if isinstance(e, RuntimeError) else ''}") from None
        ms = (time.time() - t) * 1000
        # ties go to the earlier (retrieval-ranked) candidate, as in the hook
        best = max(range(len(s)), key=lambda i: (s[i], -i)) if s else None
        rows.append({"id": c["id"], "pos": bool(c["expect"]),
                     "top_ok": bool(s) and hs[best]["id"] in c["expect"],
                     "in_cands": any(h["id"] in c["expect"] for h in hs),
                     "best": s[best] if s else None, "ms": ms})
    return rows


def picks(r, tau):
    return r["best"] is not None and r["best"] >= tau


def correct(r, tau):
    return (picks(r, tau) and r["top_ok"]) if r["pos"] else not picks(r, tau)


def tune(rows):
    """tau with the most correct dev cases; ties -> the widest gap, at its geometric middle."""
    vals = sorted({r["best"] for r in rows if r["best"] is not None})
    lo, hi = min(vals + [0.0]), max(vals + [1.0])
    cuts = [lo] + [math.sqrt(a * b) if a > 0 and b > 0 else (a + b) / 2 for a, b in zip(vals, vals[1:])] + [hi + 1e-9]
    gap = {c: (min([v for v in vals if v >= c] or [hi]) - max([v for v in vals if v < c] or [lo])) for c in cuts}
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
        "ceiling": f'{sum(r.get("in_cands", True) for r in pos)}/{len(pos)}',
        "false_abstain": sum(1 for r in pos if not picks(r, tau)),
        "wrong_skill": sum(1 for r in picked if r["pos"] and not r["top_ok"]),
        "false_fire": sum(1 for r in picked if not r["pos"]),
        "p50_ms": round(ms[len(ms) // 2]), "p95_ms": round(ms[min(len(ms) - 1, math.ceil(len(ms) * 0.95) - 1)]),
    }


def run_name(model, src, k):
    return f"{model}@{src}@k{k}"


def first_rank(lst, gold):
    """0-based position of the first gold item in a ranked list, None if absent. A case whose
    expected ids are all missing from the registry has an empty gold set and counts as a miss."""
    return next((n for n, i in enumerate(lst) if i in gold), None)


def recall_at(ranks, k):
    return sum(1 for r in ranks if r is not None and r < k)


def run(bundle_path, model, out, src="bgem3", k="10"):
    k = int(k)
    name = run_name(model, src, k)
    t = time.time()
    fn = load(model)  # before the private data is in this process
    load_s = time.time() - t
    bundle = json.load(open(bundle_path))
    if model != "jev":  # a hosted API has nothing to warm up
        fn("warm up", [{"id": "a", "description": "warm up"}, {"id": "b", "description": "other"}])
    for split in ("dev", "held"):  # the chosen retrieval variant and candidate depth
        bundle[split]["cands"] = {cid: hs[:k] for cid, hs in bundle[split]["cands_by_src"][src].items()}
    dev, held = score_split(fn, bundle["dev"]), score_split(fn, bundle["held"])
    tau = tune(dev)
    res = {"model": name, "tau": tau, "load_s": round(load_s, 1),
           "dev": summary(dev, tau), "held": summary(held, tau), "rows": {"dev": dev, "held": held}}
    os.makedirs(out, exist_ok=True)
    json.dump(res, open(os.path.join(out, f"{name}.json"), "w"), indent=1)
    print(name, res["dev"]["all"], res["held"]["all"])
    if model == "jev":
        print(f"jev answers outside the option set: {JEV_OFF_SET[0]}")


# --- candidates -----------------------------------------------------------------------------

def rrf(lexical, dense, k=60.0):
    """Same as skill-hub dense.rs: 1/(K + rank + 1) summed over both lists, ties by index."""
    score = {}
    for lst in (lexical, dense):
        for r, i in enumerate(lst):
            score[i] = score.get(i, 0.0) + 1.0 / (k + r + 1.0)
    return sorted(score, key=lambda i: (-score[i], i))


QWEN_EMB_REPO = "Qwen/Qwen3-Embedding-0.6B"
CAND_K = 16  # the reference rerank server accepts at most 16 texts per request
RECALL_AT = (5, 10, 16, 30)


def cands(bundle_path, skill_hub, out):
    """Two retrieval variants, BM25 fused by RRF with bge-m3 (what skill-hub ships) or with
    Qwen3-Embedding as the dense half; recall@k of each list, numbers only."""
    import tempfile
    from sentence_transformers import SentenceTransformer
    dense_models = {"bgem3": SentenceTransformer("BAAI/bge-m3", device="cpu", revision=REVISIONS["BAAI/bge-m3"]),
                    "qwen3emb": SentenceTransformer(QWEN_EMB_REPO, device="cpu",
                                                    revision=REVISIONS[QWEN_EMB_REPO])}  # before the data
    bundle = json.load(open(bundle_path))
    reg = bundle.pop("registry")
    skills = reg["skills"]
    index = {s["id"]: i for i, s in enumerate(skills)}
    gold_rank = {}  # (split, list name) -> [rank of the first gold id per positive case, or None]
    with tempfile.TemporaryDirectory() as d:
        reg_path = os.path.join(d, "registry.json")  # no .emb sidecar: skill-hub search is BM25 only
        json.dump(reg, open(reg_path, "w"))
        # skill-hub embed_text: "name: description", first 2000 chars
        texts = [f'{s["name"]}: {s["description"]}'[:2000] for s in skills]
        sv = {}
        for k, m in dense_models.items():
            t0, full = time.time(), m.max_seq_length
            # ponytail: 512 tokens covers "name: description" for the registry only; the 8k/32k
            # defaults ran past the 60 min job limit on CPU (run 38393875479). Queries keep full length.
            m.max_seq_length = 512
            sv[k] = m.encode(texts, batch_size=32, normalize_embeddings=True, show_progress_bar=False)
            m.max_seq_length = full
            print(f"encoded {len(texts)} skills with {k} in {time.time() - t0:.0f}s", flush=True)
        for split in ("dev", "held"):
            by_src = {k: {} for k in dense_models}
            for c in bundle[split]["cases"]:
                # errors name the case id only: argv holds the private prompt, and a traceback
                # would put it into a public Actions log
                try:
                    p = subprocess.run([skill_hub, "search", "--registry", reg_path, "--top-k", "50",
                                        "--", c["prompt"]], capture_output=True, text=True, timeout=120)
                except subprocess.TimeoutExpired:
                    raise SystemExit(f"skill-hub search timed out on case {c['id']}") from None
                if p.returncode != 0:
                    raise SystemExit(f"skill-hub search failed on case {c['id']} (rc={p.returncode})")
                lex = json.loads(p.stdout or "[]")
                if any(h.get("id") not in index for h in lex):
                    raise SystemExit(f"skill-hub returned an unknown id on case {c['id']}")
                lex_idx = [index[h["id"]] for h in lex]
                bm25 = {index[h["id"]]: h["score"] for h in lex}
                lists = {"lexical": lex_idx}
                for k, m in dense_models.items():
                    qp = {"prompt": QWEN_EMB_PROMPT} if k == "qwen3emb" else {}
                    cos = sv[k] @ m.encode([c["prompt"]], normalize_embeddings=True, show_progress_bar=False,
                                           **qp)[0]
                    dense = sorted(range(len(skills)), key=lambda i: (-cos[i], i))[:200]
                    fused = rrf(lex_idx, dense) if c["prompt"].strip() else lex_idx
                    lists[f"dense-{k}"], lists[f"rrf-{k}"] = dense, fused
                    by_src[k][c["id"]] = [{"id": skills[i]["id"], "description": skills[i]["description"],
                                           "score": bm25.get(i, 0.0)} for i in fused[:CAND_K]]
                if c["expect"]:
                    gold = {index[e] for e in c["expect"] if e in index}
                    for name, lst in lists.items():
                        gold_rank.setdefault((split, name), []).append(first_rank(lst, gold))
            bundle[split]["cands_by_src"] = by_src
    json.dump(bundle, open(out, "w"), ensure_ascii=False)
    # recall@k: share of positives whose gold skill is within the first k of each list
    for (split, name), ranks in sorted(gold_rank.items()):
        cells = " ".join(f"@{k}={recall_at(ranks, k)}/{len(ranks)}" for k in RECALL_AT)
        print(f"{split} {name}: {cells}")


# --- report ---------------------------------------------------------------------------------

BASELINE = run_name("qwen3rr-skill", "bgem3", 10)  # what skill-hub ships after PR #17


def mcnemar(a, b):
    """Exact two-sided McNemar on paired correctness: (wins of a, losses of a, p)."""
    w = sum(1 for k in a if a[k] and not b[k])
    l = sum(1 for k in a if b[k] and not a[k])
    n = w + l
    p = min(1.0, 2 * sum(math.comb(n, i) for i in range(min(w, l) + 1)) / 2 ** n) if n else 1.0
    return w, l, p


def report(out):
    res = {}
    if not os.path.isdir(out):
        print("(no results: every model job failed or was skipped)")
        return
    for f in sorted(os.listdir(out)):
        if f.endswith(".json"):
            r = json.load(open(os.path.join(out, f)))
            res[r["model"]] = r
    lines = ["| config | tau | dev | **held** | held pos | held abstain | top-1 | ceiling | false abstain "
             "| wrong skill | false fire | p50/p95 ms | load s |", "|" + "---|" * 13]
    order = sorted(res, key=lambda m: -int(res[m]["held"]["all"].split("/")[0]))
    for m in order:
        r, d, h = res[m], res[m]["dev"], res[m]["held"]
        lines.append(f'| {m} | {r["tau"]:.4g} | {d["all"]} | **{h["all"]}** | {h["pos"]} | {h["abstain"]} '
                     f'| {h["top1"]} | {h["ceiling"]} | {h["false_abstain"]} | {h["wrong_skill"]} '
                     f'| {h["false_fire"]} | {h["p50_ms"]} / {h["p95_ms"]} | {r["load_s"]} |')
    ok = {m: {x["id"]: correct(x, res[m]["tau"]) for x in res[m]["rows"]["held"]} for m in res}
    lines += ["", "Paired exact McNemar on held-out (wins / losses of the row model, p):", "",
              f"| config | vs {BASELINE} | vs best |", "|---|---|---|"]
    best = order[0] if order else None
    for m in order:
        cells = []
        for ref in (BASELINE, best):
            if ref in ok and ref != m:
                w, l, p = mcnemar(ok[m], ok[ref])
                cells.append(f"{w} / {l}, p={p:.3f}")
            else:
                cells.append("—")
        lines.append(f"| {m} | {cells[0]} | {cells[1]} |")
    print("\n".join(lines))


if __name__ == "__main__":
    cmd, args = sys.argv[1], sys.argv[2:]
    {"cands": cands, "run": run, "report": report}[cmd](*args)
