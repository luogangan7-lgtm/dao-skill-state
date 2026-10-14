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
On Apple Silicon with `mlx-lm` installed it serves a 4-bit MLX conversion of the same model
(D7 round 6: 97/112, as accurate as float16, ~1.3 s per request, 0.9 GB); otherwise torch on
the Apple GPU (float16), CUDA or CPU. Scores are shifted per variant so the hook's default
tau fits each. One model thread serves requests in arrival order (--max-batch-rows lets requests
share a pass on a GPU where that pays; on an M1 it did not); a request that waited past
--max-wait-ms gets 503 at once and the hook falls back instead of queueing. On CPU a hook request
(10 candidates) takes seconds: raise SKILL_HUB_RERANK_TIMEOUT_MS (default 3000) accordingly.

Depends on: mlx-lm (Apple Silicon), or torch + transformers (Qwen3) or sentence-transformers
(other cross-encoders), installed by the user. The model loads from the Hugging Face cache; only the first run
downloads it, at the pinned revision.
"""
import argparse
import importlib.util
import json
import math
import platform
import queue
import sys
import threading
import time
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
HOOK_TAU = 0.865  # skill-hub hook's default SKILL_HUB_RERANK_TAU (hook.rs RERANK_TAU)
# Each Qwen3 variant's tau tuned on the same dev set (D7 round 6, run 38451676603); the server
# shifts its scores so that tau reads as HOOK_TAU, and the hook default fits every backend.
TUNED_TAU = {"mlx": 0.345, "float16": 0.865, "float32": 0.865}
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


def split(pairs, flat):
    """Flat per-row scores back into one list per (query, texts) pair."""
    out, i = [], 0
    for _, texts in pairs:
        out.append(flat[i:i + len(texts)])
        i += len(texts)
    return out


def to_hook_scale(p, tau):
    """Shift a backend's probabilities so its own tuned tau lands on the hook's default
    (HOOK_TAU): monotonic, so rankings and every keep/abstain decision at tau are unchanged."""
    q = min(max(p, 1e-7), 1 - 1e-7)  # p first: max/min return it when it is NaN, so NaN survives to the guard
    logit = lambda x: math.log(x / (1 - x))
    y = 1 / (1 + math.exp(-(logit(q) - logit(tau) + logit(HOOK_TAU))))
    # rounding must not move a score across the threshold: at or above tau stays at or above the
    # hook's tau, below stays clearly below even after the hook reads it as an f32
    return max(y, HOOK_TAU) if p >= tau else min(y, HOOK_TAU - 1e-6)


CANARY = ("Set up a new Rust command-line tool with clap for argument parsing",
          ["rust-cli-scaffold: Scaffold a Rust command-line application with cargo and clap argument parsing."])
CANARY_TOL = 0.25  # logits; float16 and padding move it by < 0.04 (mps_probe.py)


def with_canary(score):
    """GPU self-check: every pass also scores a fixed row and compares it with the value from
    start-up. On GitHub's virtual Macs the Apple GPU sometimes failed a pass without raising and
    returned NaN or wrong finite scores (D7 rounds 6-7, PR #26-27); a failed check raises, the
    request gets 500, and the hook falls back instead of acting on corrupted scores."""
    logit = lambda p: math.log(max(p, 1e-12) / max(1 - p, 1e-12))
    ref = logit(score([CANARY])[0][0])

    def checked(pairs):
        out = score(list(pairs) + [CANARY])
        c = logit(out.pop()[0])
        if not abs(c - ref) <= CANARY_TOL:  # written so a NaN fails too
            raise RuntimeError("GPU self-check failed")
        return out
    return checked


class Batcher:
    """One model thread. Requests that arrive while it is busy run together in the next forward
    pass (up to max_rows texts), so a burst of agents costs a few passes, not one per agent.
    A request still waiting after max_wait_s is answered "busy": the hook stopped waiting at
    its own timeout, so computing it would only delay the requests behind it."""

    def __init__(self, score, max_rows, max_wait_s):
        self.score, self.max_rows, self.max_wait_s = score, max_rows, max_wait_s
        self.q = queue.Queue()
        threading.Thread(target=self._run, daemon=True).start()

    def __call__(self, q, texts):
        job = {"pair": (q, texts), "t": time.monotonic(), "done": threading.Event()}
        self.q.put(job)
        # the model thread only dies on a BaseException; never hang a request forever on it
        if not job["done"].wait(self.max_wait_s + 120):
            return None
        if "error" in job:
            raise job["error"]
        return job["out"]  # None: dropped as stale

    def _run(self):
        while True:
            batch = [self.q.get()]
            rows = len(batch[0]["pair"][1])
            while rows < self.max_rows:
                try:
                    job = self.q.get_nowait()
                except queue.Empty:
                    break
                batch.append(job)
                rows += len(job["pair"][1])
            now = time.monotonic()
            live = [j for j in batch if now - j["t"] <= self.max_wait_s]
            for j in batch:
                j["out"] = None
            try:
                if live:
                    for j, out in zip(live, self.score([j["pair"] for j in live])):
                        j["out"] = out
            except Exception as e:  # noqa: BLE001 — reported to each waiting request, never fatal
                for j in live:
                    j["error"] = e
            for j in batch:
                j["done"].set()


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

    def score(pairs):
        rs = [r for q, texts in pairs for r in rows(q, texts)]
        enc = tok.pad({"input_ids": rs}, padding=True, return_tensors="pt").to(dev)
        with torch.no_grad():
            lg = m(**enc, logits_to_keep=1).logits[:, -1, :].float()
        return split(pairs, torch.stack([lg[:, no], lg[:, yes]], dim=1).log_softmax(dim=1)[:, 1].exp().tolist())
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

    def score(pairs):
        rs = [r for q, texts in pairs for r in rows(q, texts)]
        # right padding: attention is causal, so a row's last real position never sees the padding
        n = max(len(r) for r in rs)
        x = mx.array([r + [0] * (n - len(r)) for r in rs])
        h = m.model(x)[mx.arange(len(rs)), mx.array([len(r) - 1 for r in rs])]
        last = out(h.astype(mx.float32))  # float32 answer logits: scores sit near a tuned tau
        return split(pairs, mx.sigmoid(last[:, yes] - last[:, no]).tolist())  # = softmax([no, yes])[1]
    return score


def cross_encoder_scorer(name, max_length, dev, revision=None):
    import torch
    from sentence_transformers import CrossEncoder
    m = cached_first(lambda local: CrossEncoder(name, max_length=max_length, device=dev, revision=revision,
                                                local_files_only=local))
    is_prob = isinstance(getattr(m, "activation_fn", None), torch.nn.Sigmoid)

    def score(pairs):
        raw = [float(x) for x in m.predict([(q, t) for q, texts in pairs for t in texts])]
        # overflow-safe logistic, so every score is a probability like Qwen3's
        return split(pairs, raw if is_prob else
                     [1 / (1 + math.exp(-x)) if x >= 0 else math.exp(x) / (1 + math.exp(x)) for x in raw])
    return score


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8121)
    ap.add_argument("--model", default=QWEN)
    ap.add_argument("--max-length", type=int, default=512, help="cross-encoders only")
    ap.add_argument("--revision", help="commit of a --model cross-encoder (Qwen3 is always pinned)")
    ap.add_argument("--backend", choices=["auto", "torch", "mlx"], default="auto",
                    help="auto: 4-bit MLX on Apple Silicon when mlx-lm is installed, else torch")
    ap.add_argument("--dtype", choices=["auto", "float32", "float16"], default="auto",
                    help="torch Qwen3: auto = float16 on the Apple GPU, float32 elsewhere")
    # PR #23 mac-concurrency (M1 GPU, MLX 4-bit): one request (10 texts) per pass ~1.3 s; six merged
    # (60 texts) took 6-14 s and more memory, so merging gains nothing there. Default: one request.
    ap.add_argument("--max-batch-rows", type=int, default=MAX_TEXTS,
                    help="texts per forward pass when requests arrive together (raise on a large GPU)")
    ap.add_argument("--max-wait-ms", type=int, default=3000,
                    help="answer busy (503) when a request waited longer (the hook's default timeout)")
    a = ap.parse_args()
    backend = a.backend
    if backend == "auto":
        apple = sys.platform == "darwin" and platform.machine() == "arm64"
        backend = "mlx" if a.model == QWEN and apple and importlib.util.find_spec("mlx_lm") else "torch"
    dev = "mlx" if backend == "mlx" else device()
    # D7 rounds 6-7: float32 on the Apple GPU scored 88 and 92/112 against 98 for float16 and for
    # float32 on CPU, because some requests came back NaN (run 38489180747), so it defaults to float16
    dtype = a.dtype if a.dtype != "auto" else ("float16" if dev == "mps" else "float32")
    if dev == "mps" and dtype == "float32":
        print("warning: float32 on the Apple GPU returned NaN scores for some requests (D7 round 7); "
              "those requests now fail over to the hook's fallback. float16 or MLX is recommended.",
              file=sys.stderr, flush=True)
    if dev == "cpu":
        print("warning: running on CPU; a hook request takes seconds, longer than the hook's default "
              "SKILL_HUB_RERANK_TIMEOUT_MS=3000, so raise it or the hook falls back", file=sys.stderr, flush=True)
    if backend == "mlx":
        if a.model != QWEN:
            raise SystemExit("--backend mlx serves Qwen3 only")
        model_score, variant = qwen3_mlx_scorer(), "mlx"
    elif a.model == QWEN:
        model_score, variant = qwen3_scorer(dev, dtype), dtype
    else:
        model_score, variant = cross_encoder_scorer(a.model, a.max_length, dev, a.revision), None
    tau = TUNED_TAU.get(variant) if a.model == QWEN else None
    if dev != "cpu":  # one extra row per pass (~10% at 10 candidates); CPUs did not show the fault
        model_score = with_canary(model_score)
    if tau is not None and tau != HOOK_TAU:
        raw = model_score
        model_score = lambda pairs: [[to_hook_scale(p, tau) for p in ps] for ps in raw(pairs)]
    desc = ("Use when the user asks to scaffold, configure, test or deploy a project; covers setup, "
            "conventions, common errors and the commands to run, with examples. ") * 3
    # first calls pay the load and the GPU's shape compilation, not the first prompts
    model_score([("warm up", ["warm up"])])
    model_score([("Set up a Rust command-line tool with argument parsing", [desc[:400]] * 10)] * 2)
    score = Batcher(model_score, a.max_batch_rows, a.max_wait_ms / 1000)

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
            try:
                scores = score(q, texts) if texts else []
            except Exception:  # noqa: BLE001 — the model or the GPU self-check failed; no details out
                return self.send_error(500, "model error")
            if scores is None:
                return self.send_error(503, "busy")
            if any(s != s for s in scores):
                # a NaN never clears tau, so it would read as "no skill" when the model failed
                # (seen with torch float32 on the Apple GPU, D7 round 7); the hook falls back instead
                return self.send_error(500, "model returned NaN")
            body = json.dumps([{"index": i, "score": float(s)} for i, s in enumerate(scores)]).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):  # prompts must not end up in a terminal log
            pass

    class Server(ThreadingHTTPServer):
        request_queue_size = 256  # many agents submitting at once; the default backlog of 5 drops them

    srv = Server(("127.0.0.1", a.port), Handler)
    print("rerank server on http://127.0.0.1:%d (%s, %s, %s)" % (a.port, a.model, dev,
          "4bit" if backend == "mlx" else dtype), flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
