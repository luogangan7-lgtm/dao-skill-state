#!/usr/bin/env python3
"""Summarise a `dao` store after the intake benchmark (.github/workflows/intake-bench.yml).

  intake_bench_report.py STORE OUT_DIR

Reads only what the pipeline wrote (locks/, scans/, cards/, relations.json, evals/, admissions/)
and prints a markdown report: skills per source, scan verdicts and top rules, card status,
relation kinds, routing-eval pass rates, admission outcomes and the gates that refused. Writes
OUT_DIR/summary.json for later rounds to compare against. Public third-party content only.

Depends on: the standard library. Called by: the intake-bench workflow.
"""
import collections
import glob
import json
import os
import sys


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def main(store, out):
    os.makedirs(out, exist_ok=True)
    digests = sorted(d for d in os.listdir(os.path.join(store, "objects")) if len(d) == 64)
    source_of = {}
    for d in digests:
        locks = glob.glob(os.path.join(store, "locks", d, "*.json"))
        repo = "?"
        for lp in locks:
            lk = load(lp)
            repo = (lk.get("source") or lk.get("url") or lk.get("git") or repo)
            if isinstance(repo, dict):
                repo = repo.get("url") or repo.get("git") or "?"
        source_of[d] = str(repo).replace("https://github.com/", "")

    scans = {d: load(p) for d in digests if os.path.exists(p := os.path.join(store, "scans", f"{d}.json"))}
    cards = {d: load(p) for d in digests if os.path.exists(p := os.path.join(store, "cards", f"{d}.json"))}
    adm = {d: load(p) for d in digests if os.path.exists(p := os.path.join(store, "admissions", f"{d}.json"))}
    rel_path = os.path.join(store, "relations.json")
    rels = load(rel_path) if os.path.exists(rel_path) else []
    if isinstance(rels, dict):
        rels = rels.get("relations", [])
    receipts = sorted(glob.glob(os.path.join(store, "evals", "receipt-*.json")), key=os.path.getmtime)
    receipt = load(receipts[-1]) if receipts else None

    verdicts = collections.Counter(s["verdict"] for s in scans.values())
    rules = collections.Counter(f["rule"] for s in scans.values() for f in s.get("findings", []))
    status = collections.Counter(c["status"] for c in cards.values())
    kinds = collections.Counter(r["kind"] for r in rels)
    admitted = sum(1 for a in adm.values() if a["admitted"])
    refused_gates = collections.Counter(g.get("name", g.get("gate", "?")) for a in adm.values()
                                        for g in a["gates"] if not g["passed"])
    per_source = collections.defaultdict(collections.Counter)
    for d in digests:
        s = per_source[source_of[d]]
        s["staged"] += 1
        s[scans[d]["verdict"] if d in scans else "not scanned"] += 1
        s["card ok"] += d in cards and cards[d]["status"] == "ok"
        s["admitted"] += d in adm and adm[d]["admitted"]

    lines = ["## Third-party skill intake benchmark", "",
             f"{len(digests)} skills staged; {len(scans)} scanned; {len(cards)} normalized; "
             f"{len(adm)} admission decisions, {admitted} admitted.", "",
             "| source | staged | CLEAN | REVIEW_REQUIRED | UNRESOLVED | BLOCK | card ok | admitted |",
             "|---|---|---|---|---|---|---|---|"]
    for src in sorted(per_source):
        c = per_source[src]
        lines.append(f"| {src} | {c['staged']} | {c['CLEAN']} | {c['REVIEW_REQUIRED']} | {c['UNRESOLVED']} | "
                     f"{c['BLOCK']} | {c['card ok']} | {c['admitted']} |")
    lines += ["", "Scan verdicts: " + ", ".join(f"{k} {v}" for k, v in verdicts.most_common()),
              "Top scan rules: " + ", ".join(f"{k} {v}" for k, v in rules.most_common(10)),
              "Card status: " + ", ".join(f"{k} {v}" for k, v in status.most_common()),
              "Relations: " + (", ".join(f"{k} {v}" for k, v in kinds.most_common()) or "none")]
    if receipt:
        lines.append("Routing eval (" + str(receipt.get("n_skills")) + " skills): " + ", ".join(
            f"{k} {v['passed']}/{v['total']} (unreachable {v['unreachable']})"
            for k, v in receipt.get("kinds", {}).items()))
    lines.append("Refusing gates: " + (", ".join(f"{k} {v}" for k, v in refused_gates.most_common()) or "none"))
    print("\n".join(lines))
    json.dump({"staged": len(digests), "verdicts": verdicts, "rules": rules, "card_status": status,
               "relations": kinds, "admitted": admitted, "decisions": len(adm), "refused_gates": refused_gates,
               "eval": receipt and receipt.get("kinds"),
               "per_source": {k: dict(v) for k, v in per_source.items()}},
              open(os.path.join(out, "summary.json"), "w"), indent=1, sort_keys=True)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
