#!/usr/bin/env python3
"""Local cross-encoder reranker for `skill-hub hook` (SKILL_HUB_RERANK).

Serves text-embeddings-inference's `/rerank` API on loopback only:
  POST /rerank {"query": str, "texts": [str]}  ->  [{"index": i, "score": s}]  (input order)
Scores are the model's sigmoid relevance in [0, 1], the scale the hook's tau (default 0.02)
was tuned on in D7. Another TEI-compatible server works in its place only if it returns that
same scale (TEI's default for this model); raw logits would make tau meaningless.

Usage:
  python3 -I rerank_server.py [--port 8121] [--model BAAI/bge-reranker-v2-m3]
  export SKILL_HUB_RERANK=http://127.0.0.1:8121

Depends on: sentence-transformers (installed by the user; nothing is downloaded at import
time other than the model on first use, from the Hugging Face cache or hub).
"""
import argparse
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Bounds on work, not just bytes: one cross-encoder pass per text, each up to --max-length tokens.
# A hook request is the prompt (<= 2,500 chars) and 10 "id: description" texts (<= ~450 chars).
MAX_BODY = 64 << 10
MAX_TEXTS = 16
MAX_QUERY_CHARS = 4000
MAX_TEXT_CHARS = 2000
SOCKET_TIMEOUT_S = 10  # a client that never sends its declared body can't hold a thread


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8121)
    ap.add_argument("--model", default="BAAI/bge-reranker-v2-m3")
    ap.add_argument("--max-length", type=int, default=512)
    a = ap.parse_args()
    # the model comes from the local cache; don't phone the hub on every start (override to fetch)
    os.environ.setdefault("HF_HUB_OFFLINE", "1")
    from sentence_transformers import CrossEncoder

    model = CrossEncoder(a.model, max_length=a.max_length)
    model.predict([("warm", "up")])  # first call pays the load, not the first prompt

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
            scores = model.predict([(q, t) for t in texts]) if texts else []
            body = json.dumps([{"index": i, "score": float(s)} for i, s in enumerate(scores)]).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):  # prompts must not end up in a terminal log
            pass

    srv = ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    print("rerank server on http://127.0.0.1:%d (%s)" % (a.port, a.model), flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
