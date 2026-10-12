#!/usr/bin/env python3
"""Differential gate for the Rust port of check_skill (C4 · M0/M1).

Runs the frozen Python checker (in-process, its CLI `main`) and `dao check` on the same skill
directories and compares stdout byte-for-byte plus the exit code. Python tracebacks are recorded
as PY_CRASH (the Rust port reports those as invalid frontmatter; documented divergence).

Usage:
  python3 -I check_skill_diff.py --dao BIN --checker PATH/check_skill.py --work DIR
      [--corpus DIR ...]            # dirs whose immediate subdirs are skills (real corpora)
      [--generated]                 # the built-in edge-case corpus below
      [--freeze FIXTURES.json]      # write generated cases + Python results (M0 baseline)
Exit 0 when every difference is explained, 1 otherwise.
"""
import argparse
import base64
import contextlib
import importlib.util
import io
import itertools
import json
import os
import shutil
import subprocess
import sys

ap = argparse.ArgumentParser()
ap.add_argument("--dao", required=True)
ap.add_argument("--checker", required=True)
ap.add_argument("--work", required=True)
ap.add_argument("--corpus", action="append", default=[])
ap.add_argument("--generated", action="store_true")
ap.add_argument("--freeze")
ap.add_argument("--known", default="", help="comma-separated case dirs whose difference is documented")
a = ap.parse_args()

spec = importlib.util.spec_from_file_location("check_skill", a.checker)
py = importlib.util.module_from_spec(spec)
spec.loader.exec_module(py)


def run_py(path):
    out = io.StringIO()
    try:
        with contextlib.redirect_stdout(out):
            code = py.main([path])
    except Exception as e:  # the Python checker's uncaught exceptions
        return "PY_CRASH", f"{type(e).__name__}"
    return code, out.getvalue()


def run_rs(path):
    p = subprocess.run([a.dao, "check", path], capture_output=True)
    return p.returncode, p.stdout.decode("utf-8", "replace")


# ---------- generated edge cases: (dir name, raw SKILL.md bytes) ----------
def gen():
    vals = [
        "short", "'quoted'", '"dq \\t \\x41 \\u263A \\/ \\N \\_ \\L"', "yes", "No", "on", "OFF", "y", "n", "true",
        "~", "null", "", "''", "0", "017", "08", "0o17", "0x1F", "0b101", "0b", "1_000", "-1", "+0", "190:20:30",
        "1:60", "1.5", "1e5", "1.0e+5", ".inf", "-.INF", ".NaN", "2026-10-13", "2026-02-30", "2026-1-3 1:02:03",
        "2026-10-13T25:00:00Z", "2026-10-13 10:00:00 +24:00", "2026-10-13t01:02:03.5-05:30", "=", "<<",
        "!!str 12", "!!int '0b'", "!!int ''", "!!int ' 12 '", "!!int 0o17", "!!int 0b0b1", "!!float x", "!!float ' 1.5'",
        "!!float infinity", "!!bool maybe", "!!bool YES", "!!null x", "!!binary aGVsbG8=", "!!binary '@@'",
        "!!timestamp 2026-1-1", "!!timestamp nope", "!!set {a, b}", "!!omap [{a: 1}, {b: 2}]", "!!omap [a]",
        "!!pairs [{a: 1}]", "!!map ''", "!!map ab", "!!seq [a]", "!!seq x", "!!str [a]", "! 12", "!foo bar",
        "!<tag:yaml.org,2002:str> 5", "!!python/name:os.system", "[a, b]", "{}", "{a: 1}", "{a: b, a: c}",
        "|\n  ---\n  Read supplied text.", ">-\n  folded\n  text", "|+\n  keep\n\n", "\"multi\n  line\"",
        "plain\n  continued", "&a anchored", "*a", "'it''s'", "@reserved", "`tick", "a: b", "a #c", "a#b",
        "\t tab", "x" * 1024, "x" * 1025, "   ", "'   '", "\"\\u3000\"", "\"\\x1f\"", "é", "道", "\"\\uFEFF\"",
    ]
    names = [
        "{d}", "'{d}'", "{d}-x", "Upper", "a--b", "-a", "a-", "道-skill", "हिंदी", "ab_c", "a.b", "1-2", "Ⅻ-x",
        "ǅ", "ß", "x" * 64, "x" * 65, "12", "yes", "~", "[a]", "{d}\u0301",
    ]
    cases = []
    for i, v in enumerate(vals):
        d = f"v{i}"
        cases.append((d, f"---\nname: {d}\ndescription: {v}\n---\nbody\n"))
        cases.append((d + "m", f"---\nname: {d}m\ndescription: ok\nmetadata:\n  k: {v}\n---\n"))
        cases.append((d + "l", f"---\nname: {d}l\ndescription: ok\nlicense: {v}\ncompatibility: {v}\n---\n"))
        cases.append((d + "t", f"---\nname: {d}t\ndescription: ok\nallowed-tools: {v}\nx-{i}: {v}\n---\n"))
    for i, n in enumerate(names):
        d = n.replace("{d}", f"n{i}").strip("'")
        if any(c in d for c in "[]{}~/") or d in ("yes", "12"):
            d = f"n{i}"
        cases.append((d, f"---\nname: {n.replace('{d}', f'n{i}')}\ndescription: ok\n---\n"))
    structural = [
        "", "---\n", "---\n---\n", "---\nname: s\n", "--- \nname: s\n---\n", "\ufeff---\nname: s\ndescription: ok\n---\n",
        "---\r\nname: s\r\ndescription: ok\r\n---\r\n", "---\rname: s\rdescription: ok\r---\r",
        "---\nname: s\ndescription: ok\n...\n---\n", "---\nname: s\ndescription: ok\n--- x\n---\n",
        "---\nname: s\u2028description: ok\n---\n", "---\nname: s\x0bdescription: ok\n---\n",
        "---\nname: s\ndescription: ok\x85---\n", "---\nname: s\ndescription: ok\n---\n" + "a" * (1024 * 1024),
        "---\n- a\n- b\n---\n", "---\njust text\n---\n", "---\nname: s\ndescription: ok\n  bad: indent\n---\n",
        "---\nname: s\ndescription: ok\nmetadata:\n  nested:\n    bad: v\n---\n",
        "---\nname: s\ndescription: ok\nmetadata: &m\n  a: b\nother: *m\n---\n",
        "---\nname: s\ndescription: ok\nmetadata: {a: [*undefined]}\n---\n",
        "---\n%YAML 1.1\n---\nname: s\ndescription: ok\n---\n", "---\nname: s\ndescription: ok\n<<: {a: 1}\n---\n",
        "---\nname: s\ndescription: ok\n'<<': x\n---\n", "---\nname: s\ndescription: ok\n? complex\n: v\n---\n",
        "---\nname: s\ndescription: ok\n? [a]\n: v\n---\n", "---\nname: s\ndescription: ok\n12: v\n---\n",
        "---\nname: s\ndescription: ok\nnull: v\n---\n", "---\nname: s\ndescription: ok\n~: v\n---\n",
        "---\nname: s\ndescription: ok\n\"\": v\n---\n", "---\nname: s\ndescription: ok\na: &x {b: *x}\n---\n",
        "---\nname: s\ndescription: ok\na: &x [*x]\n---\n", "---\nname: s\ndescription: ok\na: &x {b: [*x]}\n---\n",
        "---\n{name: s, description: ok}\n---\n", "---\nname: s\ndescription: ok\nname: s\n---\n",
        "---\nname:s\ndescription: ok\n---\n", "---\nname: s\ndescription: ok # c\n---\n",
        "---\nname: s\ndescription:\n  - list\n---\n", "---\nname: s\ndescription: ok\n\tx: 1\n---\n",
        "---\nname: s\ndescription: \"unterminated\n---\n", "---\nname: s\ndescription: ok\n\x01: x\n---\n",
        "---\nname: s\ndescription: ok\nk: [a, {b: c}, [d]]\n---\n", "---\n&top\nname: s\ndescription: ok\n---\n",
        "---\n!!map\nname: s\ndescription: ok\n---\n", "---\n!!set {a}\n---\n", "---\n!!omap [{name: s}]\n---\n",
        "---\nname: s\ndescription: |\n  ---\n  Read.\ncustom-host-field: optional\n---\n",
    ]
    for i, s in enumerate(structural):
        cases.append(("s" if i % 2 else f"s{i}", s.replace("name: s", "name: s" if i % 2 else f"name: s{i}")))
    # pairwise combinations of a few values in two fields
    small = ["yes", "'x'", "1.0", "[a]", "{a: b}", "!!str 1", "", "道"]
    for i, (x, y) in enumerate(itertools.product(small, small)):
        cases.append((f"p{i}", f"---\nname: p{i}\ndescription: {x}\nlicense: {y}\n---\n"))
    return [(d, s.encode("utf-8")) for d, s in cases] + [
        ("b1", b"---\nname: b1\ndescription: \xff\n---\n"),
        ("b2", b"\xff\xfe"),
    ] + [(d, s.encode("utf-8")) for d, s in REVIEW]


# Repros from the PR #6 reviews (independent reviewer + Codex): each was a divergence once.
REVIEW = [
    ("r-tabq", '---\nname: r-tabq\ndescription: "Does a thing."\t\n---\n'),
    ("r-tabs", "---\nname: r-tabs\ndescription: ok\nx: 'y'\t\n---\n"),
    ("r-tabk", '---\nname: r-tabk\ndescription: ok\n"k"\t: v\n---\n'),
    ("r-flowq", "---\nname: r-flowq\ndescription: ok\ntriggers: [how do I deploy?, rollback]\n---\n"),
    ("r-flowurl", "---\nname: r-flowurl\ndescription: ok\nmetadata: {homepage: https://example.com/docs?lang=en}\n---\n"),
    ("r-flowdesc", "---\n{name: r-flowdesc, description: Is it ok?}\n---\n"),
    ("r-flowcolon", "---\nname: r-flowcolon\ndescription: ok\nx: [:a, b]\n---\n"),
    ("r-flowbar", "---\nname: r-flowbar\ndescription: ok\nx: [|a, b]\n---\n"),
    ("r-flowtag", "---\nname: r-flowtag\ndescription: ok\nx: [!, a]\n---\n"),
    ("r-flowkey", '---\nname: r-flowkey\ndescription: ok\nx: {"a\n b": c}\n---\n'),
    ("r-longkey", "---\nname: r-longkey\ndescription: ok\n" + "k" * 1030 + ": v\n---\n"),
    ("r-anchorc", "---\nname: r-anchorc\ndescription: ok\nmetadata:\n  &author: someone\n---\n"),
    ("r-anchord", "---\nname: r-anchord\ndescription: &d: ok\n---\n"),
    ("r-docend", "---\n...\nname: r-docend\ndescription: ok\n---\n"),
    ("r-yaml2", "---\n%YAML 2.0\n--- \nname: r-yaml2\ndescription: ok\n---\n"),
    ("r-sqcont", "---\nname: r-sqcont\ndescription: ok\na: 'x\ny'\n---\n"),
    ("r-b64pad", "---\nname: r-b64pad\ndescription: ok\nx: !!binary aGk= b c\n---\n"),
    ("r-setmerge", "---\nname: r-setmerge\ndescription: ok\ncustom: !!set {<<: {a: !foo bar}}\n---\n"),
    ("r-setmerge2", "---\nname: r-setmerge2\ndescription: ok\ncustom: !!set {<<: {a: !!int x}}\n---\n"),
    ("r-setval", "---\nname: r-setval\ndescription: ok\ncustom: !!set {=: null}\n---\n"),
    ("r-strval", "---\nname: r-strval\ndescription: !!str {=: hello}\n---\n"),
    ("r-tsval", "---\nname: r-tsval\ndescription: ok\nx: !!timestamp {=: 2026-01-01}\n---\n"),
    ("r-nullnl", "---\nname: r-nullnl\ndescription: ! |\n  null\n---\n"),
    ("r-datenl", "---\nname: r-datenl\ndescription: ok\nx: ! |\n  2026-10-13\n---\n"),
    ("r-yesnl", '---\nname: r-yesnl\ndescription: ! "yes\\n"\n---\n'),
    ("r-arabic", "---\nname: r-arabic\ndescription: ok\ncustom: !!int '١٢'\n---\n"),
    ("r-arabicf", "---\nname: r-arabicf\ndescription: ok\ncustom: !!float '١.٥'\n---\n"),
    ("r-bomb", "---\nname: r-bomb\ndescription: ok\na0: &a0 {x: 1}\n"
     + "".join(f"a{i}: &a{i} {{p: *a{i-1}, q: *a{i-1}}}\n" for i in range(1, 40)) + "---\n"),
    ("r-emptyblk", "---\nname: r-emptyblk\ndescription: ok\nx: |\n---\n"),
] + [
    (f"r-depth-{style}-{n}", "---\nname: r-depth-%s-%d\ndescription: ok\n%s---\n" % (style, n, body))
    for n in (480, 489, 490, 491, 600, 5000)
    for style, body in (
        ("flow", "x: " + "{a: " * n + "1" + "}" * n + "\n"),
        ("seq", "x: " + "[" * n + "1" + "]" * n + "\n"),
        ("block", "x:\n" + "".join(" " * (i + 1) + "a:\n" for i in range(n)) + " " * (n + 1) + "1\n"),
    )
]



def materialize(root, cases):
    out = []
    for i, (d, raw) in enumerate(cases):
        skill = os.path.join(root, f"{i:05d}", d)
        os.makedirs(skill)
        with open(os.path.join(skill, "SKILL.md"), "wb") as f:
            f.write(raw)
        out.append(skill)
    return out


shutil.rmtree(a.work, ignore_errors=True)
os.makedirs(a.work)
groups = []
if a.generated:
    cases = gen()
    groups.append(("generated", cases, materialize(os.path.join(a.work, "gen"), cases)))
for c in a.corpus:
    dirs = sorted(os.path.join(c, d) for d in os.listdir(c) if os.path.isdir(os.path.join(c, d)))
    groups.append((c, None, dirs))
# CLI edge paths (relative forms, missing, file-as-dir)
special = [os.path.join(a.work, "missing-dir"), os.path.join(a.work, "gen", "00000", ".", "")] if a.generated else []

ok = True
fixtures = []
known = set(filter(None, a.known.split(",")))
for name, cases, dirs in groups:
    stats = {"same": 0, "py_crash": 0, "diff": 0}
    for i, d in enumerate(dirs):
        pc, po = run_py(d)
        rc, ro = run_rs(d)
        if pc == "PY_CRASH":
            explained = rc == 1 and '"invalid YAML frontmatter' in ro
            stats["py_crash"] += 1
            if not explained:
                ok = False
                print(f"UNEXPLAINED {name} {d}: python crashed ({po}); rust exit {rc}")
        elif (pc, po) == (rc, ro):
            stats["same"] += 1
        elif os.path.basename(d) in known:
            stats.setdefault("known", 0)
            stats["known"] += 1
        else:
            stats["diff"] += 1
            ok = False
            if stats["diff"] <= 15:
                print(f"DIFF {name} {d}\n  py {pc}: {po.strip()[:400]}\n  rs {rc}: {ro.strip()[:400]}")
        if cases is not None and len(cases[i][1]) <= 64 * 1024:  # big inputs stay live-only
            dname, raw = cases[i]
            fixtures.append({"dir": dname, "skill_md_b64": base64.b64encode(raw).decode(),
                             "python_exit": pc, "python_stdout": po.replace(d, "{DIR}") if pc != "PY_CRASH" else None})
    print(f"{name}: n={len(dirs)} {stats}")
for d in special:
    pc, po = run_py(d)
    rc, ro = run_rs(d)
    same = (pc, po) == (rc, ro)
    ok &= same
    print(f"special {d!r}: {'same' if same else f'DIFF py={pc} {po[:200]!r} rs={rc} {ro[:200]!r}'}")
if a.freeze:
    with open(a.freeze, "w") as f:
        json.dump(fixtures, f, ensure_ascii=False, indent=0)
    import hashlib, platform, yaml
    with open(a.freeze.replace(".json", ".meta.json"), "w") as f:
        json.dump({"python": platform.python_version(), "pyyaml": yaml.__version__,
                   "checker_sha256": hashlib.sha256(open(a.checker, "rb").read()).hexdigest(),
                   "cases": len(fixtures)}, f, indent=1)
    print(f"froze {len(fixtures)} cases -> {a.freeze}")
sys.exit(0 if ok else 1)
