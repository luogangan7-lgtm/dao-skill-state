#!/usr/bin/env python3
"""Evaluation A3: are routing cards faithful to the skills they describe? (normalize invariant)

  card_fidelity.py STORE          check every card in STORE/cards/ against STORE/objects/<digest>/SKILL.md
  card_fidelity.py --selftest     show that the check fails on a paraphrased card

normalize.rs promises that every text field is copied, never paraphrased: name and description are
the YAML-decoded values with surrounding whitespace trimmed; use_when / not_for sentences are verbatim parts of the description;
headings are verbatim parts of the body; invocation entries are verbatim frontmatter lines.
Prints the counts and every violation; exits 1 if there is any. Public third-party content only.

Depends on: PyYAML (the same decoder check_skill.py uses). Called by: the intake-bench workflow.
"""
import glob
import json
import os
import sys

import yaml


def split(text):
    """(frontmatter lines, body) the way normalize.rs and check_skill delimit SKILL.md."""
    lines = text.splitlines(keepends=True)
    if not lines or lines[0].rstrip("\r\n") != "---":
        return [], text
    used = len(lines[0])
    for i, line in enumerate(lines[1:], 1):
        used += len(line)
        if line.rstrip("\r\n") == "---":
            return [l.rstrip("\r\n") for l in lines[1:i]], text[used:]
    return [], text


def check_card(card, skill_md):
    """Violations of the copy-only invariant for one card with status ok."""
    fm_lines, body = split(skill_md)
    fm = yaml.safe_load("\n".join(fm_lines) + "\n") or {}  # as check_skill.py feeds PyYAML
    bad = []
    for key in ("name", "description"):
        want = fm.get(key).strip() if isinstance(fm.get(key), str) else None  # normalize trims
        if card.get(key) != want:
            bad.append(f"{key} differs from the YAML value")
    desc = card.get("description") or ""
    for key in ("use_when", "not_for"):
        bad += [f"{key} not in description: {s[:60]!r}" for s in card.get(key, []) if s not in desc]
    bad += [f"heading not in body: {h[:60]!r}" for h in card.get("headings", []) if h not in body]
    fm_text = "\n".join(l.rstrip() for l in fm_lines)
    bad += [f"invocation not in frontmatter: {v[:60]!r}" for v in card.get("invocation", []) if v not in fm_text]
    return bad


def selftest():
    md = "---\nname: a-skill\ndescription: >\n  Use when cleaning CSV.\n  Not for Excel.\n---\n## Steps\nDo it.\n"
    good = {"name": "a-skill", "description": "Use when cleaning CSV. Not for Excel.",
            "use_when": ["Use when cleaning CSV."], "not_for": ["Not for Excel."], "headings": ["Steps"],
            "invocation": []}
    assert check_card(good, md) == [], check_card(good, md)
    bad = dict(good, use_when=["Use this to clean CSV files."], headings=["Step list"])
    assert len(check_card(bad, md)) == 2, check_card(bad, md)
    assert check_card(dict(good, description="Use when cleaning CSV."), md)
    print("selftest ok")


def main(store):
    cards = sorted(glob.glob(os.path.join(store, "cards", "*.json")))
    checked, statuses, violations = 0, {}, []
    for path in cards:
        card = json.load(open(path, encoding="utf-8"))
        statuses[card["status"]] = statuses.get(card["status"], 0) + 1
        if card["status"] != "ok":
            continue
        md = open(os.path.join(store, "objects", card["digest"], "SKILL.md"), encoding="utf-8").read()
        checked += 1
        violations += [f"{card['digest'][:12]} {card.get('name')}: {v}" for v in check_card(card, md)]
    print(f"cards {len(cards)} ({', '.join(f'{k} {v}' for k, v in sorted(statuses.items()))}); "
          f"{checked} checked; {len(violations)} copy-only violations")
    for v in violations:
        print("  " + v)
    return 1 if violations or not checked else 0


if __name__ == "__main__":
    if sys.argv[1:] == ["--selftest"]:
        selftest()
    else:
        sys.exit(main(sys.argv[1]))
