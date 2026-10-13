#!/usr/bin/env python3
"""Local reranker for `skill-hub hook` (SKILL_HUB_RERANK).

Serves text-embeddings-inference's `/rerank` API on loopback only:
  POST /rerank {"query": str, "texts": [str]}  ->  [{"index": i, "score": s}]  (input order)
Scores are relevance probabilities in [0, 1].

Default model: Qwen/Qwen3-Reranker-0.6B with a skill-routing instruction, scored as P(yes) from
its yes/no answer logits. D7 round 3 (CI run 38379553491, 112 held-out cases, same RRF top-10):
98/112 against 88/112 for bge-reranker-v2-m3 (paired exact McNemar 13 wins / 3 losses, p=0.021),
with every chit-chat prompt correctly answered by no skill. The hook's default tau (0.865) was
tuned for this model. `--model BAAI/bge-reranker-v2-m3` (or another sentence-transformers
cross-encoder) still works; set SKILL_HUB_RERANK_TAU=0.02 for bge, since tau is per model.

Usage:
  python3 -I rerank_server.py [--port 8121] [--model Qwen/Qwen3-Reranker-0.6B]
  export SKILL_HUB_RERANK=http://127.0.0.1:8121
Runs on the Apple GPU (MPS) or CUDA when available, else CPU. `--backend mlx` serves a 4-bit
MLX conversion of the same model on Apple Silicon (needs `mlx-lm`). On CPU a hook request (10
candidates) takes seconds: raise SKILL_HUB_RERANK_TIMEOUT_MS (default 3000) accordingly.

Depends on: torch + transformers (Qwen3) or sentence-transformers (other cross-encoders),
installed by the user. The model loads from the Hugging Face cache; only the first run
downloads it, at the pinned revision.
"""
import argparse
import json
import math
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Bounds on work, not just bytes: one model pass per text.
# A hook request is the prompt (<= 2,500 chars) and 10 "id: description" texts (<= ~450 chars).
MAX_BODY = 64 << 10
MAX_TEXTS = 16
MAX_QUERY_CHARS = 4000
MAX_TEXT_CHARS = 2000
SOCKET_TIMEOUT_S = 10  # a client that never sends its declared body can't hold a thread

QWEN = "Qwen/Qwen3-Reranker-0.6B"
QWEN_REVISION = "e61197ed45024b0ed8a2d74b80b4d909f1255473"  # the commit evaluated in D7 round 3
QWEN_MLX = "mlx-community/Qwen3-Reranker-0.6B-4bit"
QWEN_MLX_REVISION = "5f324548f1d20c2b5a450f126fc6ef2fb1126524"
QWEN_INSTRUCTION = ("Given a user's request to a coding agent, judge whether this skill (name: description) "
                    "should be loaded to carry out the request; small talk or general questions need no skill")


def device():
    import torch
    if torch.cuda.is_available():
        return "cuda"
    return "mps" if torch.backends.mps.is_available() else "cpu"


def cached_first(load):
    """The local cache first, so a normal start never contacts the hub; the hub only on first run."""
    try:
        return load(True)
    except OSError:
        print("model not cached; downloading it once", flush=True)
        return load(False)


def qwen3_rows(tok):
    """Token rows for Qwen3-Reranker's yes/no judgement, shared by the torch and MLX backends:
    (yes id, no id, rows(query, texts)). Every row ends at the answer position."""
    yes, no = tok.convert_tokens_to_ids("yes"), tok.convert_tokens_to_ids("no")
    ids = lambda t: tok(t, add_special_tokens=False)["input_ids"]
    pre = ids("<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and "
              "the Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n"
              "<|im_start|>user\n")
    suf = ids("<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n")
    # pieces split before a space so " text" merges like whole-string tokenization does
    head = ids(f"<Instruct>: {QWEN_INSTRUCTION}\n<Query>:")
    mid = ids("\n<Document>:")
    budget = 1024 - len(pre) - len(head) - len(mid) - len(suf)

    def rows(q, texts):
        # query and document are cut separately (a long query can't crowd out the candidate);
        # a long prompt keeps its head and its tail, like the hook's bound_prompt (a request written
        # after a long paste survives); short prompts, as evaluated, are untouched
        q_ids, room = ids(" " + q), max(budget // 2, 2)
        if len(q_ids) > room:
            q_ids = q_ids[: room // 2] + q_ids[len(q_ids) - (room - room // 2):]
        return [pre + head + q_ids + mid + ids(" " + t)[: max(budget - len(q_ids), 1)] + suf for t in texts]
    return yes, no, rows


def qwen3_scorer(dev, dtype="float32"):
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    tok = cached_first(lambda local: AutoTokenizer.from_pretrained(
        QWEN, padding_side="left", revision=QWEN_REVISION, local_files_only=local))
    # On MPS the fused (sdpa) attention aborts the whole process on Qwen3's grouped-query
    # attention (16 query heads over 8 key/value heads: "mps.matmul … failed to infer result type",
    # torch 2.6, seen on the macos-latest CI runner); eager attention expands the key/value heads
    # first, which MPS handles. An abort cannot be caught, so it has to be avoided up front.
    attn = {"attn_implementation": "eager"} if dev == "mps" else {}
    m = cached_first(lambda local: AutoModelForCausalLM.from_pretrained(
        QWEN, revision=QWEN_REVISION, dtype=getattr(torch, dtype), local_files_only=local, **attn)).to(dev).eval()
    yes, no, rows = qwen3_rows(tok)

    def score(q, texts):
        enc = tok.pad({"input_ids": rows(q, texts)}, padding=True, return_tensors="pt").to(dev)
        with torch.no_grad():
            lg = m(**enc, logits_to_keep=1).logits[:, -1, :].float()
        return torch.stack([lg[:, no], lg[:, yes]], dim=1).log_softmax(dim=1)[:, 1].exp().tolist()
    return score


def qwen3_mlx_scorer():
    """Qwen3-Reranker-0.6B quantized to 4 bits for Apple's MLX (Apple Silicon only)."""
    import mlx.core as mx
    from huggingface_hub import snapshot_download
    from mlx_lm import load
    path = cached_first(lambda local: snapshot_download(QWEN_MLX, revision=QWEN_MLX_REVISION,
                                                        local_files_only=local))
    m, tok = load(path)
    yes, no, rows = qwen3_rows(getattr(tok, "_tokenizer", tok))  # the HF tokenizer inside mlx-lm's wrapper
    # Qwen3-0.6B ties its output layer to the embedding; project only the answer positions, never
    # the full (rows x tokens x 151k-vocabulary) logits
    out = m.lm_head if hasattr(m, "lm_head") else m.model.embed_tokens.as_linear

    def score(q, texts):
        rs = rows(q, texts)
        # right padding: attention is causal, so a row's last real position never sees the padding
        n = max(len(r) for r in rs)
        x = mx.array([r + [0] * (n - len(r)) for r in rs])
        h = m.model(x)[mx.arange(len(rs)), mx.array([len(r) - 1 for r in rs])]
        last = out(h).astype(mx.float32)
        return mx.sigmoid(last[:, yes] - last[:, no]).tolist()  # = softmax([no, yes])[1]
    return score


def cross_encoder_scorer(name, max_length, dev, revision=None):
    import torch
    from sentence_transformers import CrossEncoder
    m = cached_first(lambda local: CrossEncoder(name, max_length=max_length, device=dev, revision=revision,
                                                local_files_only=local))
    is_prob = isinstance(getattr(m, "activation_fn", None), torch.nn.Sigmoid)

    def score(q, texts):
        raw = [float(x) for x in m.predict([(q, t) for t in texts])]
        if is_prob:
            return raw
        # overflow-safe logistic, so every score is a probability like Qwen3's
        return [1 / (1 + math.exp(-x)) if x >= 0 else math.exp(x) / (1 + math.exp(x)) for x in raw]
    return score


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8121)
    ap.add_argument("--model", default=QWEN)
    ap.add_argument("--max-length", type=int, default=512, help="cross-encoders only")
    ap.add_argument("--revision", help="commit of a --model cross-encoder (Qwen3 is always pinned)")
    ap.add_argument("--backend", choices=["torch", "mlx"], default="torch",
                    help="mlx: Qwen3 quantized to 4 bits (Apple Silicon, needs mlx-lm)")
    ap.add_argument("--dtype", choices=["float32", "float16"], default="float32", help="torch Qwen3 only")
    a = ap.parse_args()
    dev = "mlx" if a.backend == "mlx" else device()
    if dev == "cpu":
        print("warning: running on CPU; a hook request takes seconds, longer than the hook's default "
              "SKILL_HUB_RERANK_TIMEOUT_MS=3000, so raise it or the hook falls back", file=sys.stderr, flush=True)
    if a.backend == "mlx":
        if a.model != QWEN:
            raise SystemExit("--backend mlx serves Qwen3 only")
        model_score = qwen3_mlx_scorer()
    else:
        model_score = (qwen3_scorer(dev, a.dtype) if a.model == QWEN
                       else cross_encoder_scorer(a.model, a.max_length, dev, a.revision))
    lock = threading.Lock()

    def score(q, texts):
        # one forward at a time: requests the hook already gave up on must not pile up in parallel
        with lock:
            return model_score(q, texts)

    score("warm up", ["warm up"])  # first call pays the load, not the first prompt

    class Handler(BaseHTTPRequestHandler):
        timeout = SOCKET_TIMEOUT_S

        def do_POST(self):
            if self.path != "/rerank":
                return self.send_error(404)
            try:
                n = int(self.headers.get("Content-Length") or 0)
            except ValueError:
                return self.send_error(400, "bad Content-Length")
            if n <= 0 or n > MAX_BODY:
                return self.send_error(413)
            try:
                req = json.loads(self.rfile.read(n))
                q, texts = req["query"], req["texts"]
                if not isinstance(q, str) or not isinstance(texts, list) or len(texts) > MAX_TEXTS \
                        or not all(isinstance(t, str) for t in texts):
                    raise ValueError("query must be a string and texts a list of at most %d strings" % MAX_TEXTS)
                if len(q) > MAX_QUERY_CHARS or any(len(t) > MAX_TEXT_CHARS for t in texts):
                    raise ValueError("query or text too long")
            except (ValueError, KeyError, TypeError) as e:
                return self.send_error(400, str(e))
            scores = score(q, texts) if texts else []
            body = json.dumps([{"index": i, "score": float(s)} for i, s in enumerate(scores)]).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):  # prompts must not end up in a terminal log
            pass

    srv = ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    print("rerank server on http://127.0.0.1:%d (%s, %s, %s)" % (a.port, a.model, dev,
          "4bit" if a.backend == "mlx" else a.dtype), flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
