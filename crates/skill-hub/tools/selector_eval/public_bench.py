#!/usr/bin/env python3
"""Public routing benchmark (evaluation A4): requests that need one of the 112 pinned third-party skills.

  public_bench.py gen REGISTRY OUT      ask a model for 2 requests per skill (CI, .github/workflows/public-bench.yml)
  public_bench.py bundle REGISTRY CASES OUT   eval.py bundle: registry + dev/held cases, expect names → ids

Replaces the private, encrypted selector data so anyone can rerun the selector eval and the tau
tuning. The skills come from the pinned sources in crates/dao/tools/intake_bench.json (built into a
registry in CI, never committed: their texts stay under their own licenses). Committed here
(public_cases.json): the generated requests, each naming its expected skill by name, and a fixed
list of requests that need no skill. A request that names its skill is dropped (too easy).

gen reads ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN / ANTHROPIC_MODEL (an Anthropic-compatible
endpoint); the token is never printed. Depends on: the standard library. Called by: the
public-bench and selector-eval workflows.
"""
import hashlib
import json
import os
import re
import sys
import urllib.request

ASK = """Here is an agent skill.
Name: {name}
Description: {description}

Write 2 different, realistic requests a user might type to a coding agent where this skill is the
right one to use. Do not mention the skill's name or the word "skill"; invent concrete details
(files, data, goals); each request is one message of 8 to 40 words. Reply with only a JSON array of
2 strings."""

# Requests that need no skill: small talk, general knowledge, and small code edits any agent does.
NONE = [
    "Thanks, that's all for today.", "Hi! How are you doing?", "What time zone is Tokyo in?",
    "Explain the difference between TCP and UDP in two sentences.", "What does HTTP status 418 mean?",
    "Rename the variable tmp to total in the function below.", "Why is the sky blue?",
    "Can you summarise what we did in this session?", "What is the capital of Australia?",
    "Convert 72 degrees Fahrenheit to Celsius.", "Is Python's list.sort stable?",
    "Fix the typo 'recieve' in this comment.", "What's a good name for a cat?",
    "How many bytes are in a kilobyte?", "Tell me a short joke about databases.",
    "What does the 'git status' command show?", "Add a docstring to this two-line helper.",
    "What is the time complexity of binary search?", "Good morning!", "Can you explain what a closure is?",
    "Reformat this JSON so it is indented with two spaces.", "What's the difference between let and const in JavaScript?",
    "Who wrote Pride and Prejudice?", "Is 97 a prime number?", "Remove the unused import on line 3.",
    "What does 'idempotent' mean?", "Thank you, that worked.", "Translate 'good night' into French.",
    "What year did the first iPhone come out?", "Make this sentence shorter: 'In order to proceed we need to'.",
    "Explain recursion to a ten-year-old.", "What is the default port for PostgreSQL?",
    "Sort these numbers: 5, 2, 9, 1.", "What does CSS flexbox do in one sentence?",
    "Can you rename this test so it reads better?", "What's 15% of 240?", "Never mind, I figured it out.",
    "What does the acronym API stand for?", "Change the log message to say 'started' instead of 'begin'.",
    "Which is bigger, a megabyte or a mebibyte?",
]


def ask(name, description):
    body = {"model": os.environ["ANTHROPIC_MODEL"], "max_tokens": 1024,
            "messages": [{"role": "user", "content": ASK.format(name=name, description=description)}]}
    req = urllib.request.Request(os.environ["ANTHROPIC_BASE_URL"].rstrip("/") + "/v1/messages",
                                 data=json.dumps(body).encode(), method="POST", headers={
                                     "content-type": "application/json", "anthropic-version": "2023-06-01",
                                     "authorization": "Bearer " + os.environ["ANTHROPIC_AUTH_TOKEN"]})
    out = json.load(urllib.request.urlopen(req, timeout=180))
    text = "".join(b.get("text", "") for b in out.get("content", []) if b.get("type") == "text")
    m = re.search(r"\[.*\]", text, re.S)
    got = json.loads(m.group(0)) if m else []
    return [s.strip() for s in got if isinstance(s, str) and s.strip()][:2]


def names_itself(prompt, name):
    p = prompt.lower()
    return name.lower() in p or name.lower().replace("-", " ") in p


def gen(registry, out):
    skills = json.load(open(registry))["skills"]
    names = sorted({s["name"] for s in skills if s.get("description")})
    desc = {s["name"]: s["description"] for s in skills if s.get("description")}
    cases, dropped, failed = [], 0, []
    for name in names:
        try:
            prompts = ask(name, desc[name])
        except Exception as e:  # noqa: BLE001 — record and go on; the summary shows the count
            failed.append(f"{name}: {type(e).__name__}")
            continue
        for p in prompts:
            if names_itself(p, name):
                dropped += 1
                continue
            cases.append({"prompt": p, "expect": [name]})
    cases += [{"prompt": p, "expect": []} for p in NONE]
    json.dump({"source": f"generated by {os.environ['ANTHROPIC_MODEL']} from the pinned skills' names and "
                         "descriptions; no-skill requests written by hand", "cases": cases},
              open(out, "w"), ensure_ascii=False, indent=1)
    print(f"{len(names)} skills, {len(cases) - len(NONE)} requests kept, {dropped} dropped (named the skill), "
          f"{len(NONE)} no-skill requests, {len(failed)} model calls failed")
    for f in failed:
        print("  failed:", f)
    return 1 if failed and len(failed) > len(names) // 10 else 0


def split_of(case):
    """dev or held, stable: a skill's requests stay together; no-skill requests by their text."""
    key = case["expect"][0] if case["expect"] else case["prompt"]
    return "dev" if hashlib.sha256(key.encode()).digest()[0] % 2 == 0 else "held"


def bundle(registry, cases_path, out):
    reg = json.load(open(registry))
    ids = {}
    for s in reg["skills"]:  # same-name skills from two sources are both right
        ids.setdefault(s["name"], []).append(s["id"])
    b = {"registry": reg, "dev": {"cases": []}, "held": {"cases": []}}
    missing = 0
    for i, c in enumerate(json.load(open(cases_path))["cases"]):
        expect = [x for n in c["expect"] for x in ids.get(n, [])]
        if c["expect"] and not expect:
            missing += 1  # a pinned skill is no longer in the registry: a broken benchmark
            continue
        b[split_of(c)]["cases"].append({"id": f"p{i:04d}", "prompt": c["prompt"], "expect": expect})
    json.dump(b, open(out, "w"), ensure_ascii=False)
    n = {k: (len(b[k]["cases"]), sum(1 for c in b[k]["cases"] if c["expect"])) for k in ("dev", "held")}
    print(f"bundle: dev {n['dev'][0]} cases ({n['dev'][1]} need a skill), held {n['held'][0]} ({n['held'][1]}); "
          f"{missing} expected skills missing from the registry")
    return 1 if missing else 0


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "gen" and len(sys.argv) == 4:
        sys.exit(gen(sys.argv[2], sys.argv[3]))
    if cmd == "bundle" and len(sys.argv) == 5:
        sys.exit(bundle(sys.argv[2], sys.argv[3], sys.argv[4]))
    sys.exit(__doc__)
