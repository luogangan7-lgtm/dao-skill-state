#!/usr/bin/env python3
"""Concurrency benchmark for rerank_server.py: many agents submitting prompts at the same moment.

  rerank_bench.py PORT SERVER_PID [LEVELS]   e.g. rerank_bench.py 8121 4242 1,2,4,8,16,32,64,100

At each level N, N hook-sized requests (a prompt + 10 skill texts of ~400 chars) start together;
reports per-request latency, how many answered within the hook's default 3 s timeout (the rest
fall back to the lexical rule), throughput, and the server's peak memory footprint while busy.
Synthetic public texts only. Runs in CI (rerank-e2e.yml), not on a developer machine.

Depends on: the standard library and macOS `footprint`/`ps` for memory.
"""
import json
import re
import subprocess
import sys
import threading
import time
import urllib.request

HOOK_TIMEOUT_MS = 3000
DESC = ("Use when the user asks to scaffold, configure, test or deploy a project of this kind; covers "
        "setup, conventions, common errors and the commands to run, with examples and checks. ") * 2
TEXTS = [f"skill-{i}: {DESC[:400]}" for i in range(10)]


def footprint_mb(pid):
    """Physical footprint (includes Apple GPU memory in unified memory); RSS if unavailable."""
    try:
        out = subprocess.run(["footprint", "-p", str(pid)], capture_output=True, text=True, timeout=20).stdout
        m = re.search(r"(?i)footprint:\s*([\d.]+)\s*([KMG])B", out)
        if m:
            return float(m.group(1)) * {"K": 1 / 1024, "M": 1, "G": 1024}[m.group(2).upper()]
    except (OSError, subprocess.SubprocessError):
        pass
    rss = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return int(rss) / 1024 if rss else 0.0


def one(port, n, out):
    body = json.dumps({"query": f"Set up a new Rust CLI tool with clap for argument parsing (agent {n})",
                       "texts": TEXTS}).encode()
    t = time.time()
    try:
        r = json.load(urllib.request.urlopen(
            urllib.request.Request(f"http://127.0.0.1:{port}/rerank", body), timeout=900))
        ok = len(r) == 10 and all(0 <= x["score"] <= 1 for x in r)
    except Exception:  # noqa: BLE001 — a refused or dropped request is a result, not a crash
        ok = False
    out.append(((time.time() - t) * 1000, ok))


def level(port, pid, n):
    out, peak, stop = [], [footprint_mb(pid)], threading.Event()

    def sample():
        while not stop.wait(0.5):
            peak.append(footprint_mb(pid))
    s = threading.Thread(target=sample, daemon=True)
    s.start()
    ts = [threading.Thread(target=one, args=(port, i, out)) for i in range(n)]
    t = time.time()
    for x in ts:
        x.start()
    for x in ts:
        x.join()
    wall = time.time() - t
    stop.set()
    s.join()
    ms = sorted(m for m, _ in out)
    pct = lambda p: ms[min(len(ms) - 1, max(0, round(p * len(ms)) - 1))]
    fails = sum(1 for _, ok in out if not ok)
    in_time = sum(1 for m, ok in out if ok and m <= HOOK_TIMEOUT_MS)
    return (f"N={n:>3}  p50 {pct(.5):>7.0f} ms  p95 {pct(.95):>7.0f} ms  max {ms[-1]:>7.0f} ms  "
            f"within 3 s {in_time:>3}/{n:<3}  errors {fails}  {n / wall:5.2f} req/s  "
            f"peak footprint {max(peak):6.0f} MB")


def main():
    port, pid = sys.argv[1], int(sys.argv[2])
    levels = [int(x) for x in (sys.argv[3] if len(sys.argv) > 3 else "1,2,4,8,16,32,64,100").split(",")]
    print(f"idle footprint {footprint_mb(pid):.0f} MB", flush=True)
    for n in levels:
        print(level(port, pid, n), flush=True)


if __name__ == "__main__":
    main()
