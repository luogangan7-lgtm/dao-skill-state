#!/usr/bin/env python3
"""Local cross-encoder reranker for `skill-hub hook` (SKILL_HUB_RERANK).

Serves text-embeddings-inference's `/rerank` API on loopback only:
  POST /rerank {"query": str, "texts": [str]}  ->  [{"index": i, "score": s}]  (input order)
Scores are the model's sigmoid relevance in [0, 1], the scale the hook's tau (default 0.02)
was tuned on in D7. Any TEI-compatible server works in its place.

Usage:
  python3 -I rerank_server.py [--port 8121] [--model BAAI/bge-reranker-v2-m3]
  export SKILL_HUB_RERANK=http://127.0.0.1:8121

Depends on: sentence-transformers (installed by the user; nothing is downloaded at import
time other than the model on first use, from the Hugging Face cache or hub).
"""
import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MAX_BODY = 1 << 20  # a hook request is ~10 short texts
MAX_TEXTS = 64


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8121)
    ap.add_argument("--model", default="BAAI/bge-reranker-v2-m3")
    ap.add_argument("--max-length", type=int, default=512)
    a = ap.parse_args()
    from sentence_transformers import CrossEncoder

    model = CrossEncoder(a.model, max_length=a.max_length)
    model.predict([("warm", "up")])  # first call pays the load, not the first prompt

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            if self.path != "/rerank":
                return self.send_error(404)
            n = int(self.headers.get("Content-Length") or 0)
            if n <= 0 or n > MAX_BODY:
                return self.send_error(413)
            try:
                req = json.loads(self.rfile.read(n))
                q, texts = req["query"], req["texts"]
                if not isinstance(q, str) or not isinstance(texts, list) or len(texts) > MAX_TEXTS \
                        or not all(isinstance(t, str) for t in texts):
                    raise ValueError("query must be a string and texts a list of strings")
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
