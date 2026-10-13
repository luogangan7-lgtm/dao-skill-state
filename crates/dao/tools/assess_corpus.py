"""Edge-case corpus for check_assessment.py vs 'dao assess' (M3). Imported by diff_cli.py.

A case is {"name", "files": {relpath: [(bytes, repeat), ...]}, "dirs": [...], "fifos": [...],
"args": [...]}; the repeat counts keep huge inputs (10 MiB, 100k-deep nesting) tiny in fixtures.
Groups: t- the Python unit tests as CLI inputs, j- JSON syntax and positions, u- invalid UTF-8,
k- duplicate keys (repr), m- one field replaced by an edge value, x- field removed or unknown
field added, s- seeded random valid assessments (counting, fsum, pairing), n- big numbers,
c- CLI and filesystem, d- nesting depth.
"""
import copy
import json
import random
import sys

SHA = "a" * 64
KINDS = ("positive", "boundary", "nontrigger")
STATUSES = ("PASS", "FAIL", "UNKNOWN", "NOT_RUN", "INVALID")
TRUST = ("PASS", "CONDITIONAL", "FAIL", "UNKNOWN", "N/A")
MAX_BYTES = 10 * 1024 * 1024
DBL_MAX = str(int(sys.float_info.max))


def sample():
    return {
        "schema": "dao-assessment/v1",
        "identity": {"skill": "demo", "skill_sha256": SHA, "task_set_sha256": SHA,
                     "model": "model", "host": "local", "date": "2026-09-08", "rubric": "r1"},
        "arms": ["baseline", "candidate"], "repetitions": 1,
        "cases": [{"id": "p", "kind": "positive"}, {"id": "b", "kind": "boundary"}],
        "isolation": {"status": "PASS", "evidence": "isolated"}, "runs": [],
    }


def run(case_id, arm, status="PASS", trust="PASS", cost=None, rep=1):
    if cost is None:
        cost = {"seconds": 1.0, "input_tokens": 10, "output_tokens": 2, "cached_input_tokens": 0}
    return {"case_id": case_id, "arm": arm, "repetition": rep, "status": status,
            "trust": trust, "evidence": "recorded", "cost": cost}


def complete():
    data = sample()
    data["runs"] = [run(case, arm) for case in ("p", "b") for arm in data["arms"]]
    return data


def raw(name, *parts, args=("input.json",), **extra):
    """parts: bytes, or (bytes, repeat)."""
    parts = [p if isinstance(p, tuple) else (p, 1) for p in parts]
    return {"name": name, "files": {"input.json": parts} if parts else {}, "args": list(args), **extra}


def doc(name, data):
    return raw(name, json.dumps(data).encode())


def with_raw(data, path, token):
    """data with the value at path replaced by the literal JSON text token."""
    data = copy.deepcopy(data)
    target = data
    for k in path[:-1]:
        target = target[k]
    target[path[-1]] = "@@RAW@@"
    return json.dumps(data).replace('"@@RAW@@"', token).encode()


def unit_tests():
    out = [doc("t-happy", complete())]
    d = sample(); d["arms"] = ["solo"]; d["runs"] = [run(c, "solo") for c in ("p", "b")]
    out.append(doc("t-single-arm", d))
    d = sample(); d["cases"] = [{"id": "p", "kind": "positive"}]
    d["arms"] = [f"arm-{i}" for i in range(8)]; d["runs"] = [run("p", "arm-7")]
    out.append(doc("t-many-arms", d))
    d = sample(); d["arms"] = [f"arm-{i}" for i in range(8)]; out.append(doc("t-arms-8", d))
    d["arms"].append("too-many"); out.append(doc("t-arms-9", d))
    d = complete(); d["runs"][2]["status"] = "FAIL"; out.append(doc("t-failure", d))
    d = complete(); d["runs"].pop(); out.append(doc("t-missing-run", d))
    for s in ("UNKNOWN", "INVALID"):
        d = complete(); d["runs"][-1]["status"] = s; out.append(doc(f"t-status-{s}", d))
    d = complete(); d["runs"][0]["trust"] = "CONDITIONAL"; out.append(doc("t-trust", d))
    muts = {
        "dup-case": lambda d: d["cases"].append({"id": "p", "kind": "positive"}),
        "dup-arm": lambda d: d["arms"].append("baseline"),
        "dup-run": lambda d: d["runs"].append(dict(d["runs"][0])),
        "unknown-case": lambda d: d["runs"][0].update(case_id="missing"),
        "unknown-arm": lambda d: d["runs"][0].update(arm="missing"),
        "rep-2": lambda d: d["runs"][0].update(repetition=2),
        "reps-true": lambda d: d.update(repetitions=True),
        "case-id-list": lambda d: d["runs"][0].update(case_id=[]),
        "rep-list": lambda d: d["runs"][0].update(repetition=[]),
    }
    for name, f in muts.items():
        d = complete(); f(d); out.append(doc(f"t-{name}", d))
    for field, token in (("seconds", "true"), ("seconds", "Infinity"), ("seconds", "1e400"),
                         ("seconds", "-1"), ("input_tokens", "true"), ("input_tokens", "1.5"),
                         ("output_tokens", "-1"), ("seconds", "1" + "0" * 1000)):
        out.append(raw(f"t-cost-{field}-{token[:8]}", with_raw(complete(), ["runs", 0, "cost", field], token)))
    d = sample(); d["repetitions"] = 100_001; out.append(doc("t-plan-limit", d))
    d = complete()
    for r in d["runs"]:
        r["cost"]["seconds"] = 1e308
    out.append(doc("t-nonfinite-sum", d))
    d = complete(); d["isolation"]["status"] = "UNKNOWN"; out.append(doc("t-iso-unknown", d))
    d = complete(); d["runs"][0]["cost"] = None; out.append(doc("t-cost-null", d))
    d = complete(); d["identity"]["date"] = "2026-02-30"; d["runs"][0]["evidence"] = " "; d["score"] = 100
    out.append(doc("t-identity-errors", d))
    out.append(raw("t-bad-json", b"{"))
    out.append(raw("t-missing", args=["missing.json"]))
    out.append(raw("t-dup-key", b'{"schema":"one","schema":"two"}'))
    d = sample(); d["identity"]["host"] = "\ud800"; out.append(doc("t-surrogate", d))
    out.append(raw("t-recursion", (b"[", 200_000), (b"]", 200_000)))
    out.append(raw("t-fifo", args=["input.fifo"], fifos=["input.fifo"]))
    out.append(raw("t-oversize", (b"\0", MAX_BYTES + 1)))
    return out


JSON = [
    b"", b" ", b"\n\n  ", b"{", b"[", b"}", b"]", b"{}", b"[]", b"null", b"true", b"false", b"0",
    b"-0", b"1.5", b'"x"', b"nul", b"tru", b"fals", b"n", b"t", b"f", b"-", b"--1", b"+1", b".5",
    b"01", b"1.", b"1e", b"1e+", b"1E-", b"1.e5", b"-.5", b"0x10", b"1_0", b"1e400", b"-1e-400",
    b"NaN", b"Infinity", b"-Infinity", b"[NaN]", b'{"a":-Infinity}', b"Nan", b"infinity", b"-Inf",
    b"[1,]", b"[1 2]", b"[,1]", b"[1,,2]", b'{"a":1,}', b'{"a":1 , }', b'{"a":1 "b":2}', b'{"a" 1}',
    b"{a:1}", b"{'a':1}", b'{"a":}', b'{"a"', b'{"a":1', b"[1", b"[1,", b'{"a":1,', b'{"a":1,]',
    b'[1}', b'{"a":[}', b'"abc', b'"\\', b'"\\x"', b'"\\u12"', b'"\\u12', b'"\\u1234', b'"\\u12g4"',
    b'"\\ud800\\u12"', b'"\\ud800\\uzzzz"', b'"\\ud800\\udc00"', b'"\\udc00\\ud800"', b'"\\ud800"',
    b'"\\ud800\\u0041"', b'"\\ud800\\ud800"', b'"\\ud800\\', b'"\\ud800\\u"', b'"\\ud800\\udc0"',
    b'"a\x01b"', b'"a\x1fb"', b'"a\x7fb"', b'"tab\tin"', b'"nl\nin"', b"{} x", b"{}  \n ", b"{}}",
    b"[] []", b"\xef\xbb\xbf{}", b" \xef\xbb\xbf{}", b"\t\r\n{}\t\r\n", b"\x0c{}", b"\xc2\xa0{}",
    b'{"a":1}\x00', b'{\n  "a": 1,\n  "b": \n}', b'{\n\n\n"x": tru}', b'["\xc3\xa9\xc3\xa9", x]',
    b'["\xf0\x9f\x98\x80", x]', b'["\xe9\x81\x93\n\xe9\x81\x93", ?]', b'{"a":1,"a":NaN}',
    b'{"a":{"b":1,"b":2},"c":NaN}', b'{"a":1,"a":2', b'[{"a":1,"a":1}, x]', b'{"a":1,"b":2,"a":3}',
    b'[1.0, 2.5e3, -0.0, 1E+2, 123456789012345678901234567890]', b'"\\/\\b\\f\\n\\r\\t\\"\\\\"',
    b'{"":1}', b'{"a":nulll}', b'{"a":truee}', b"[-]", b"[-a]", b"[0.]", b"[1e]", b"[1e5x]",
]

UTF8 = [
    b"\xff", b"\xfe\xff", b"\x80", b'{"a":"\xc3"}', b"\xc3", b"a\xe2\x82", b"\xe2\x82x", b"\xe2x",
    b"\xed\xa0\x80", b"\xed\xa0", b"\xed\xbf\xbf", b"\xf0\x9f\x98", b"\xf0\x9f", b"\xf0\x9fx",
    b"\xf0\x8f\xbf\xbf", b"\xf4\x90\x80\x80", b"\xf5", b"\xc0\xaf", b"\xc1\xbf", b"\xe0\x80\x80",
    b"\xe0\x9f\xbf", b"\xf8\x88\x80\x80\x80", b'"ok" \xff', b'{"k":"\xe9\x81\x93\xe9"}',
    b"\xf0\x9f\x98\x80\xf0\x9f\x98",
]

KEYS = [
    "it's", 'a\\"b', "it's \\\"x\\\"", "\\\\", "\\n\\t\\r", "\\u0000\\u007f\\u001b", "\\u00e9", "\\u200b",
    "\\u3000", "\\ud800", "\\udfff", "\\ud83d\\ude00", "\\udb40\\udc01", "\\u00a0", "\\u0378",
    "\\udbff\\udfff", "\\u00ad", "\\u0085", "\\u2028", "\\ue000", "\\u0600", "\\u0897", "\\u1c89",
    "\\ud83e\\udeea", "道", "\U0001F600", "\\u0020", "!#%&()*+,-./:;<=>?@[]^_|~", "\\u0100",
    "\\uffff", "\\ufeff", "\\u00ff", "\\u0024\\u0060\\u007b\\u007d",
]

PATHS = [
    ["schema"], ["identity"], ["identity", "skill"], ["identity", "skill_sha256"],
    ["identity", "task_set_sha256"], ["identity", "host"], ["identity", "date"], ["arms"], ["arms", 0],
    ["repetitions"], ["cases"], ["cases", 0], ["cases", 0, "id"], ["cases", 1, "kind"], ["isolation"],
    ["isolation", "status"], ["isolation", "evidence"], ["runs"], ["runs", 0], ["runs", 0, "case_id"],
    ["runs", 1, "arm"], ["runs", 0, "repetition"], ["runs", 2, "status"], ["runs", 3, "trust"],
    ["runs", 0, "evidence"], ["runs", 0, "cost"], ["runs", 0, "cost", "seconds"],
    ["runs", 1, "cost", "input_tokens"], ["runs", 3, "cost", "cached_input_tokens"],
]

PALETTE = [
    "null", "true", "false", "0", "1", "-1", "-0", "2", "1.0", "-0.0", "0.5", "1e308", "1e400",
    "-1e400", "1E+2", "100001", "18446744073709551616", "1" + "0" * 400, DBL_MAX, DBL_MAX[:-1] + "9",
    "1e-320", '""', '" "', '"\\u3000\\u0085\\u001c"', '"\\u200b"', '"x"', '"PASS"', '"N/A"',
    '"positive"', '"baseline"', '"p"', '"\\ud800"', '"2024-02-29"', '"2023-02-29"', '"0000-01-01"',
    '"2026-1-01"', '"\\uff12\\uff10\\uff12\\uff16-01-01"', '"' + "A" * 64 + '"', '"' + "a" * 63 + '"',
    "[]", "{}", '["x"]', '{"a":1}', '"dao-assessment/v1"',
]


def mutations():
    out = []
    for path in PATHS:
        tag = ".".join(map(str, path))
        for k, token in enumerate(PALETTE):
            out.append(raw(f"m-{tag}-{k}", with_raw(complete(), path, token)))
    for path in [[], ["identity"], ["cases", 0], ["isolation"], ["runs", 0], ["runs", 1, "cost"]]:
        tag = ".".join(map(str, path)) or "root"
        for key in list(at(complete(), path)):
            d = complete()
            del at(d, path)[key]
            out.append(doc(f"x-del-{tag}-{key}", d))
        for key in ("zz", "\ud800", "￿", "\U0001F600", "é", "A", ""):
            d = complete()
            at(d, path)[key] = 1
            at(d, path)["\x7f"] = 2
            out.append(doc(f"x-add-{tag}-{ord(key[0]) if key else 0}", d))
    out.append(doc("x-root-empty", {}))
    d = complete(); del d["runs"]; del d["schema"]; d["zz"] = 1; out.append(doc("x-root-missing-unknown", d))
    for k, token in enumerate(["[]", "1", '"x"', "null", "true", "1.5"]):
        out.append(raw(f"x-root-{k}", token.encode()))
    return out


def at(d, path):
    for k in path:
        d = d[k]
    return d


def summaries():
    rng = random.Random(20261013)
    out = []
    names = ["baseline", "candidate", "道", "\ud800", "x" * 3, "\U0001F600", "arm 7", 'a"b']
    seconds = [1, 0, 0.1, 0.2, 0.3, 1e-300, 5e-324, 1e300, 1.7e308, -0.0, 2 ** 53 + 1, 1e16, 123456.789]
    for k in range(300):
        na = rng.choice([1, 2, 2, 2, 3, 8])
        arms = [names[i] if rng.random() < 0.5 else f"arm{i}" for i in range(na)]
        reps = rng.randint(1, 3)
        d = sample()
        d["arms"], d["repetitions"] = arms, reps
        d["cases"] = [{"id": f"c{i}", "kind": rng.choice(KINDS)} for i in range(rng.randint(1, 4))]
        d["isolation"]["status"] = rng.choice(["PASS", "PASS", "FAIL", "UNKNOWN"])
        if rng.random() < 0.3:
            items = list(d["identity"].items()); rng.shuffle(items); d["identity"] = dict(items)
        runs = []
        p_run = rng.choice([0.5, 0.9, 1.0, 1.0])
        for c in d["cases"]:
            for a in arms:
                for r in range(1, reps + 1):
                    if rng.random() > p_run:
                        continue
                    cost = None
                    if rng.random() < 0.9:
                        cost = {}
                        for f in ("seconds", "input_tokens", "output_tokens", "cached_input_tokens"):
                            if rng.random() < 0.08:
                                cost[f] = None
                            elif f == "seconds":
                                cost[f] = rng.choice(seconds + [rng.random() * 100, round(rng.random() * 10, 3)])
                            else:
                                cost[f] = rng.choice([0, 1, 7, 10 ** 30, rng.randint(0, 10 ** 6)])
                    status = rng.choices(STATUSES, weights=[6, 3, 1, 1, 1])[0]
                    trust = rng.choices(TRUST, weights=[6, 1, 1, 1, 2])[0]
                    runs.append(run(c["id"], a, status, trust, cost, r))
        rng.shuffle(runs)
        d["runs"] = runs
        out.append(doc(f"s-{k}", d))
    big = [("s-100k-rows", ["solo"], 100_000), ("s-50k-pairs", ["baseline", "candidate"], 50_000),
           ("s-8x12500", [f"a{i}" for i in range(8)], 12_500), ("s-over-limit", ["solo"], 100_001)]
    for name, arms, reps in big:
        d = sample(); d["arms"], d["repetitions"] = arms, reps
        d["cases"] = [{"id": "p", "kind": "positive"}]
        d["runs"] = [run("p", arms[-1], rep=r) for r in (1, reps)]
        out.append(doc(name, d))
    d = sample(); d["repetitions"] = 10; d["cases"] = [{"id": "p", "kind": "positive"}]
    tenth = {"seconds": 0.1, "input_tokens": 1, "output_tokens": 1, "cached_input_tokens": 1}
    d["runs"] = [run("p", a, cost=dict(tenth), rep=r) for a in d["arms"] for r in range(1, 11)]
    out.append(doc("s-fsum-tenths", d))
    return out


def numbers():
    out = []
    for name, reps in (("n-reps-4300-nines", "9" * 4300), ("n-reps-4299-nines", "9" * 4299),
                       ("n-reps-4301-digits", "1" * 4301), ("n-reps-neg-4301", "-" + "1" * 4301),
                       ("n-reps-neg", "-5"), ("n-reps-neg-zero", "-0"), ("n-reps-float", "1.0")):
        out.append(raw(name, with_raw(complete(), ["repetitions"], reps)))
    for name, tok in (("n-tokens-4300-nines", "9" * 4300), ("n-tokens-4299-nines", "9" * 4299),
                      ("n-tokens-big", "1" + "0" * 100)):
        d = complete()
        for r in d["runs"]:
            r["cost"]["input_tokens"] = "@@T@@"
        out.append(raw(name, json.dumps(d).replace('"@@T@@"', tok).encode()))
    for name, tok in (("n-sec-dblmax", DBL_MAX), ("n-sec-dblmax-float", "1.7976931348623157e308"),
                      ("n-sec-2p53", str(2 ** 53 + 1)), ("n-sec-tiny", "4.9e-324"),
                      ("n-sec-exp", "1.5e-7"), ("n-sec-int-big", "1" + "0" * 300),
                      # repr breaks shortest-digit ties half-even
                      ("n-sec-tie-1e15", "1000000000000000.25"), ("n-sec-tie-5809", "580932385485918.25"),
                      ("n-sec-tie-2p49", "562949953421312.25"), ("n-sec-tie-1394", "1394089528363410.2"),
                      ("n-sec-pow2-m24", "5.960464477539063e-08"), ("n-sec-pow2-89", "6.189700196426902e+26")):
        out.append(raw(name, with_raw(complete(), ["runs", 0, "cost", "seconds"], tok)))
    # literals whose exponent only fits after folding in the digit count (Python: inf, 1.0, 0.0)
    z = (b"0", 700000)
    for name, head, tail in (("n-sec-long-inf", b"0.", b"1e700401"), ("n-sec-long-one", b"0.", b"1e700001"),
                             ("n-sec-long-zero", b"1", b"e-700401")):
        pre, post = with_raw(complete(), ["runs", 0, "cost", "seconds"], "@@").split(b"@@")
        out.append(raw(name, pre + head, z, tail + post))
    return out


def cli():
    good = json.dumps(complete()).encode()
    odd = ["it" + chr(39) + "s.json", "é" + chr(34) + "x", "a" + chr(92) + "b" + chr(127) + chr(9),
           "道" + chr(0x200B) + ".json"]

    def named(name, fname, args):
        return {"name": name, "files": {fname: [(good, 1)]}, "args": args}

    return [
        raw("c-missing-norm", args=["./sub//missing.json"]),
        raw("c-dir", args=["d"], dirs=["d"]), raw("c-dir-slash", args=["d/"], dirs=["d"]),
        raw("c-empty-arg", args=[""]), raw("c-dot", args=["."]),
        named("c-notdir", "f.json", ["f.json/x"]),
        raw("c-trailing-slash", good, args=["input.json/"]),
        raw("c-dot-slash", good, args=["./input.json"]),
    ] + [raw(f"c-quote-{i}", args=[a]) for i, a in enumerate(odd)] + [
        named("c-dash-file", "-x", ["--", "-x"]), named("c-neg-num", "-1", ["-1"]),
        named("c-neg-dot", "-.5x", ["-.5x"]), named("c-space-opt", "-a b", ["-a b"]),
        raw("c-help", args=["-h"]), raw("c-help-long", args=["--help"]), raw("c-help-prefix", args=["--he"]),
        raw("c-help-late", good, args=["input.json", "-h"]),
        raw("c-usage-none", args=[]), raw("c-usage-two", good, args=["input.json", "b"]),
        raw("c-usage-opt", args=["-x"]), raw("c-usage-opt-file", good, args=["-x", "input.json"]),
        raw("c-usage-dd-two", args=["--", "a", "b"]), raw("c-usage-dd", args=["--"]),
        raw("c-help-hx", args=["-hx"]), raw("c-help-hspace", args=["-h x"]), raw("c-help-hh", args=["-hh"]),
        raw("c-usage-heq", args=["-h=x"]), raw("c-usage-hdash", args=["-h-"]), raw("c-usage-heeq", args=["--he=x", "-h"]),
        raw("c-usage-long", args=["--hx"]), raw("c-dd-dd", good, args=["--", "--"]),
        raw("c-maxsize", good, (b" ", MAX_BYTES - len(good))),
        raw("c-maxsize-plus", good, (b" ", MAX_BYTES - len(good) + 1)),
        raw("c-empty-file", (b"", 1)),
        # argparse: -\.?\d with Unicode Nd digits, or a space (long options too), means positional
        raw("c-udigit", args=["-\u0661"]), raw("c-udot", args=["-.\u0661"]), raw("c-udigit-x", args=["-\u0661x"]),
        raw("c-superscript", args=["-\u00b2"]), raw("c-long-space", args=["--x y"]), raw("c-eq-help", args=["x", "--=y", "-h"]), raw("c-eq-empty", args=["--="]), raw("c-deq-help", args=["x", "-=y", "-h"]), raw("c-deq-bare", args=["x", "-=", "-h"]), raw("c-deq-space", args=["-=1x "]),
        # a non-UTF-8 argv byte reaches Python as a surrogate escape (frozen as {"b": base64})
        raw("c-nonutf8-missing", args=["x\udcff.json"]),
    ]


def depth():
    out = []
    for n in (1000, 99_999, 100_000, 100_001, 116_000, 300_000, 3_000_000):
        out.append(raw(f"d-arr-{n}", (b"[", n), (b"]", n)))
    for n in (100_000, 100_001, 200_000):
        out.append(raw(f"d-obj-{n}", (b'{"a":', n), b"1", (b"}", n)))
    out.append(raw("d-arr-err", (b"[", 5000), b"x"))
    out.append(raw("d-mismatch", (b"[", 1000), (b"}", 1000)))
    out.append(raw("d-in-field", with_raw(complete(), ["identity", "skill"], "[" * 50_000 + "]" * 50_000)))
    out.append(raw("d-dup-deep", (b'{"a":', 1000), b'{"k":1,"k":2}', (b"}", 1000)))
    return out


def cases():
    keys = [raw(f"k-{i}", ('{"' + k + '":1,"x":2,"' + k + '":3}').encode()) for i, k in enumerate(KEYS)]
    return (unit_tests() + [raw(f"j-{i}", b) for i, b in enumerate(JSON)]
            + [raw(f"u-{i}", b) for i, b in enumerate(UTF8)] + keys
            + mutations() + summaries() + numbers() + cli() + depth())
