#!/usr/bin/env python3
"""Evaluation A3: are routing cards faithful to the skills they describe? (normalize invariant)

  card_fidelity.py STORE          check every card in STORE/cards/ against STORE/objects/<digest>/SKILL.md
  card_fidelity.py --selftest     show that the check fails on a paraphrased card

normalize.rs promises that every text field is copied, never paraphrased: name and description are
the YAML-decoded values with surrounding whitespace trimmed; use_when / not_for sentences are
verbatim parts of the description (which list a sentence lands in is not checked); headings are
`## ` lines of the body; invocation entries are whole frontmatter lines.
Prints the counts and every violation; exits 1 if there is any. Public third-party content only.

Depends on: PyYAML (the same decoder check_skill.py uses). Called by: the intake-bench workflow.
"""
import glob
import json
import os
import sys

import yaml


def split(text):
    """(frontmatter lines, body) exactly as normalize.rs: CRLF → LF, then split on LF only."""
    text = text.replace("\r\n", "\n")
    lines = text.split("\n")
    if lines[0] != "---":
        return [], text
    for i, line in enumerate(lines[1:], 1):
        if line == "---":
            return lines[1:i], "\n".join(lines[i + 1:])
    return [], text


def check_card(card, skill_md):
    """Violations of the copy-only invariant for one card with status ok."""
    fm_lines, body = split(skill_md)
    try:
        fm = yaml.safe_load("\n".join(fm_lines) + "\n")  # as check_skill.py feeds PyYAML
    except yaml.YAMLError:
        fm = None
    if not isinstance(fm, dict):
        return ["frontmatter is not a YAML mapping, yet the card is ok"]
    bad = []
    for key in ("name", "description"):
        want = fm.get(key).strip() if isinstance(fm.get(key), str) else None  # normalize trims
        if card.get(key) != want:
            bad.append(f"{key} differs from the YAML value")
    # sentences: copied from the description (which list a sentence lands in is not checked here)
    desc = card.get("description") or ""
    for key in ("use_when", "not_for"):
        bad += [f"{key} not in description: {s[:60]!r}" for s in card.get(key, []) if not s or s not in desc]
    # headings: a `## ` line of the body, as normalize.rs takes them
    heads = {l[3:].strip() for l in body.split("\n") if l.rstrip("\r").startswith("## ")}
    bad += [f"heading is not a ## line: {h[:60]!r}" for h in card.get("headings", []) if h not in heads]
    # invocation: whole consecutive frontmatter lines (trailing spaces trimmed)
    fm_r = [l.rstrip() for l in fm_lines]
    for v in card.get("invocation", []):
        want = v.split("\n")
        if not any(fm_r[i:i + len(want)] == want for i in range(len(fm_r))):
            bad.append(f"invocation is not frontmatter lines: {v[:60]!r}")
    return bad


def selftest():
    md = ("---\nname: a-skill\ndescription: >\n  Use when cleaning CSV.\n  Not for Excel.\n"
          "disable-model-invocation: true\n---\n## Steps\nUsage notes.\n")
    good = {"name": "a-skill", "description": "Use when cleaning CSV. Not for Excel.",
            "use_when": ["Use when cleaning CSV."], "not_for": ["Not for Excel."], "headings": ["Steps"],
            "invocation": ["disable-model-invocation: true"]}
    assert check_card(good, md) == [], check_card(good, md)
    assert check_card(good, md.replace("\n", "\r\n")) == []
    for field, value, msg in [
        ("name", "b-skill", "name differs"),
        ("description", "Use when cleaning CSV.", "description differs"),
        ("use_when", ["Use this to clean CSV files."], "use_when not in description"),
        ("not_for", ["Not for spreadsheets."], "not_for not in description"),
        ("headings", ["Usage"], "heading is not a ## line"),  # in the body, but not a heading
        ("invocation", ["disable-model-invocation: false"], "invocation is not frontmatter lines"),
    ]:
        got = check_card(dict(good, **{field: value}), md)
        assert len(got) == 1 and got[0].startswith(msg), (field, got)
    assert check_card(good, "---\n- a list\n---\nbody\n") == ["frontmatter is not a YAML mapping, yet the card is ok"]
    print("selftest ok")


def main(store):
    cards = sorted(glob.glob(os.path.join(store, "cards", "*.json")))
    checked, statuses, violations = 0, {}, []
    for path in cards:
        card = json.load(open(path, encoding="utf-8"))
        statuses[card["status"]] = statuses.get(card["status"], 0) + 1
        if card["status"] != "ok":
            continue
        with open(os.path.join(store, "objects", card["digest"], "SKILL.md"), encoding="utf-8", newline="") as f:
            md = f.read()
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
