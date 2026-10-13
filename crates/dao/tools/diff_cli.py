#!/usr/bin/env python3
"""Generic differential executor (M2): a Python CLI vs a 'dao' subcommand on the same inputs.

Each case is materialized twice, one tree per implementation, and both run with that tree as cwd
and the same argv. Compared: stdout bytes, stderr class (empty/usage/traceback/panic/other), exit
code, and the sha256 of every entry left in the tree (side effects). A Python traceback is
PY_CRASH: explained when Rust neither panicked nor crashed (each one is still listed by name).
Corpora: 'assess' (assess_corpus.py) and 'check' (check_skill_diff.gen(), reused as is).

Usage:
  python3 -I diff_cli.py --corpus assess|check --py SCRIPT --dao BIN --work DIR
      [--known name,name] [--jobs N] [--freeze FIXTURE.json]
--freeze writes every case plus the Python result (the baseline replayed by cargo test) and a
.meta.json beside it. Exit 0 when every difference is explained, 1 otherwise.
"""
import argparse
import base64
import concurrent.futures
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FREEZE_MAX = 64 * 1024  # encoded input bytes; bigger cases stay live-only


def corpus(name):
    sys.path.insert(0, HERE)
    if name == "assess":
        import assess_corpus
        return assess_corpus.cases()
    import check_skill_diff
    cases = [{"name": d, "files": {d + "/SKILL.md": [(raw, 1)]}, "args": [d]} for d, raw in check_skill_diff.gen()]
    root = {"SKILL.md": [(b"---\nname: x\ndescription: ok\n---\n", 1)]}
    return cases + [{"name": "missing-dir", "files": {}, "args": ["missing-dir"]},
                    {"name": "dot", "files": root, "args": ["."]}, {"name": "no-arg", "files": root, "args": []}]


def materialize(case, root):
    os.makedirs(root)
    for d in case.get("dirs", []):
        os.makedirs(os.path.join(root, d), exist_ok=True)
    for rel, parts in case.get("files", {}).items():
        path = os.path.join(root, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as f:
            for chunk, n in parts:
                f.write(chunk * n)
    for fifo in case.get("fifos", []):
        os.mkfifo(os.path.join(root, fifo))


def tree(root):
    out = {}
    for base, dirs, files in os.walk(root):
        for name in dirs + files:
            path = os.path.join(base, name)
            st = os.lstat(path)
            rel = os.path.relpath(path, root)
            if os.path.islink(path):
                out[rel] = "link:" + os.readlink(path)
            elif os.path.isdir(path):
                out[rel] = "dir"
            elif not os.path.isfile(path):
                out[rel] = "special:%o" % (st.st_mode >> 12)
            else:
                with open(path, "rb") as f:
                    out[rel] = hashlib.sha256(f.read()).hexdigest()
    return out


def stderr_class(err):
    text = err.decode("utf-8", "replace")
    if not text:
        return "empty"
    if "Traceback (most recent call last)" in text:
        return "traceback"
    if "panicked" in text:
        return "panic"
    return "usage" if text.startswith("usage:") else "other"


def execute(cmd, case, root):
    materialize(case, root)
    try:
        p = subprocess.run(cmd + case["args"], cwd=root, capture_output=True, timeout=300)
        result = (p.returncode, p.stdout, stderr_class(p.stderr), tree(root))
    except subprocess.TimeoutExpired:
        result = ("timeout", b"", "timeout", {})
    shutil.rmtree(root, ignore_errors=True)
    return result


def encode_arg(x):
    """argv as JSON: a str, or {"b": base64} when it carries surrogate-escaped non-UTF-8 bytes."""
    try:
        x.encode("utf-8")
        return x
    except UnicodeEncodeError:
        return {"b": base64.b64encode(os.fsencode(x)).decode()}


def compare(a, i, case):
    root = os.path.join(a.work, "%05d" % i)
    py = execute([sys.executable, "-I", a.py], case, root + "-py")
    rs = execute([a.dao, a.sub], case, root + "-rs")
    if rs[2] in ("panic", "traceback", "timeout") or rs[0] < 0:  # rs[0] < 0: killed by a signal
        verdict = "diff"
    elif py[2] == "traceback":
        verdict = "py_crash"
    elif py == rs:
        verdict = "same"
    else:
        verdict = "known" if case["name"] in a.known else "diff"
    return verdict, py, rs


def encode(parts):
    out = []
    for chunk, n in parts:
        try:
            out.append({"t": chunk.decode("utf-8"), "n": n})
        except UnicodeDecodeError:
            out.append({"b": base64.b64encode(chunk).decode(), "n": n})
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", choices=["assess", "check"], required=True)
    ap.add_argument("--py", required=True, help="the Python CLI script")
    ap.add_argument("--dao", required=True, help="the dao binary")
    ap.add_argument("--work", required=True)
    ap.add_argument("--known", default="", help="comma-separated case names whose difference is documented")
    ap.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    ap.add_argument("--freeze")
    ap.add_argument("--only", default="", help="comma-separated name prefixes (a quick subset; not with --freeze)")
    a = ap.parse_args()
    a.sub = a.corpus
    a.py, a.dao = os.path.abspath(a.py), os.path.abspath(a.dao)
    a.known = set(filter(None, a.known.split(",")))
    cases = [c for c in corpus(a.corpus) if not a.only or c["name"].startswith(tuple(a.only.split(",")))]
    shutil.rmtree(a.work, ignore_errors=True)
    os.makedirs(a.work)
    with concurrent.futures.ThreadPoolExecutor(a.jobs) as pool:
        results = list(pool.map(lambda ic: compare(a, ic[0], ic[1]), enumerate(cases)))
    stats = {"same": 0, "py_crash": 0, "known": 0, "diff": 0}
    shown = 0
    for case, (verdict, py, rs) in zip(cases, results):
        stats[verdict] += 1
        if verdict == "diff" and shown < 20:
            shown += 1
            print("DIFF %s args=%r\n  py %s %s %r\n  rs %s %s %r" % (case["name"], case["args"], py[0], py[2],
                  py[1][:300], rs[0], rs[2], rs[1][:300]))
            if py[3] != rs[3]:
                print("  trees differ: py %r rs %r" % (py[3], rs[3]))
    for verdict in ("py_crash", "known"):
        names = [c["name"] for c, r in zip(cases, results) if r[0] == verdict]
        if names:
            print("%s: %s" % (verdict, ", ".join(names)))
    stale = sorted(a.known - {c["name"] for c, r in zip(cases, results) if r[0] == "known"})
    if stale:
        print("known but not different: %s" % ", ".join(stale))
    print("%s: n=%d %s" % (a.corpus, len(cases), stats))
    if a.freeze:
        fixtures, live = [], []
        for case, (verdict, py, rs) in zip(cases, results):
            files = {rel: encode(parts) for rel, parts in case.get("files", {}).items()}
            if len(json.dumps(files)) > FREEZE_MAX:
                live.append(case["name"])
                continue
            out = py[1].decode("utf-8", "surrogateescape")
            fixtures.append({"name": case["name"], "args": [encode_arg(x) for x in case["args"]], "files": files,
                             "dirs": case.get("dirs", []), "fifos": case.get("fifos", []),
                             "python_exit": py[0], "python_stderr": py[2],
                             "python_stdout_sha256": hashlib.sha256(py[1]).hexdigest(),
                             "python_stdout": out if len(out) <= 4096 else None})
        with open(a.freeze, "w") as f:
            json.dump(fixtures, f, ensure_ascii=False, indent=0)
        corpus_file = os.path.join(HERE, "assess_corpus.py" if a.corpus == "assess" else "check_skill_diff.py")
        meta = {"python": platform.python_version(), "platform": platform.platform(),
                "checker_sha256": hashlib.sha256(open(a.py, "rb").read()).hexdigest(),
                "corpus_sha256": hashlib.sha256(open(corpus_file, "rb").read()).hexdigest(),
                "cases": len(fixtures), "live_only": live}
        with open(os.path.splitext(a.freeze)[0] + ".meta.json", "w") as f:
            json.dump(meta, f, indent=1)
        print("froze %d cases -> %s (live-only: %s)" % (len(fixtures), a.freeze, ", ".join(live) or "none"))
    sys.exit(0 if stats["diff"] == 0 else 1)


if __name__ == "__main__":
    main()
