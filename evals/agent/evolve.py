#!/usr/bin/env python3
"""Evaluation D: can a meta-skill evolve a skill from an observed failure without breaking it?

  evolve.py EVOLVE_TASKS_DIR OUT_DIR [--tasks a,b] [--arms none,skill-creator,dao] [--reps 1]
            [--use-arms hook]

A task holds skill/ (a skill with a seeded defect), failure.md (one observed failure: the request,
the wrong artifact and the reviewer's verdict; never the checker or expected answers) and usage/:
fail-case (the observed failure), old-* (passed with the original skill; their requests are shown)
and heldout* (never shown).

1. Evolve: Claude Code gets the skill as ./skill/, failure.md and the list of requests that already
   worked, and must write an improved copy into ./candidate/. Arms as in create.py: none,
   skill-creator (anthropics/skills, pinned), dao (this repository's dao-skill-state).
2. Admit independently: the harness replays every usage case with the original skill and with the
   candidate. Fixed = fail-case passes with the candidate; regressed = a case that passed with the
   original fails with the candidate; generalised = heldout passes with the candidate.

Writes OUT_DIR/evolve.jsonl and prints a summary. CI only (.github/workflows/agent-eval.yml).
Depends on: run.py and create.py (same directory).
"""
import argparse
import json
import os
import shutil
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import create  # noqa: E402
import run  # noqa: E402

REQUEST = """The skill in ./skill/ failed in use. The observation is below.

{failure}

These requests already worked with the skill and must keep working:
{old}

Improve the skill so this failure does not happen again. Write the complete improved skill
directly into ./candidate/ (./candidate/SKILL.md plus any files it needs; ./candidate/ is only a
staging folder, keep the skill's name) and leave ./skill/ unchanged. Change only what the evidence
supports."""


def replay(task_root, skill_dir, use_arm, max_turns, timeout):
    """Each usage case with skill_dir installed; returns {case: result}."""
    out = {}
    for case in sorted(os.listdir(os.path.join(task_root, "usage"))):
        udir = tempfile.mkdtemp(prefix="replay-")
        shutil.copytree(os.path.join(task_root, "usage", case), udir, dirs_exist_ok=True)
        shutil.copytree(skill_dir, os.path.join(udir, "skills", os.path.basename(task_root) + "-skill"))
        out[case] = run.run_one(udir, use_arm, max_turns, timeout)
    return out


def keep_candidate(src, dest, exts=(".md", ".py", ".json", ".yaml", ".yml", ".txt"), max_bytes=256 * 1024):
    """Copy the candidate's small text files into the uploaded artifact, so a regression can be traced
    to the rule that caused it. The folder is agent-written: symlinks and other or large files are
    skipped, so nothing outside it (or a runner file it links to) can reach the artifact."""
    if os.path.islink(src):
        return
    for root, _, files in os.walk(src):  # os.walk does not follow symlinked folders
        for f in files:
            p = os.path.join(root, f)
            if os.path.islink(p) or not os.path.isfile(p) or not f.endswith(exts) or os.path.getsize(p) > max_bytes:
                continue
            out = os.path.join(dest, os.path.relpath(p, src))
            os.makedirs(os.path.dirname(out), exist_ok=True)
            shutil.copyfile(p, out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("tasks_dir")
    ap.add_argument("out_dir")
    ap.add_argument("--tasks", default="")
    ap.add_argument("--arms", default="none,skill-creator,dao")
    ap.add_argument("--use-arms", default="hook")
    ap.add_argument("--meta", default="explicit", choices=["explicit", "hook", "installed"],
                    help="how the meta-skill is offered (see create.py)")
    ap.add_argument("--reps", type=int, default=1)
    ap.add_argument("--max-turns", type=int, default=40)
    ap.add_argument("--timeout", type=int, default=1200)
    a = ap.parse_args()
    if a.arms == "none,skills":  # the workflow's generic default
        a.arms = "none,skill-creator,dao"
    bad = set(a.arms.split(",")) - {"none", "skill-creator", "dao"}
    if bad:
        sys.exit(f"unknown evolver arms: {sorted(bad)} (use none, skill-creator, dao)")
    root = os.path.abspath(a.tasks_dir)
    names = [t for t in a.tasks.split(",") if t] or sorted(
        d for d in os.listdir(root) if os.path.isfile(os.path.join(root, d, "failure.md")))
    os.makedirs(a.out_dir, exist_ok=True)
    log = open(os.path.join(a.out_dir, "evolve.jsonl"), "a")
    summary = []

    def emit(r):
        log.write(json.dumps(r) + "\n")
        log.flush()
        print(json.dumps({k: r[k] for k in r if k in ("task", "arm", "rep", "phase", "case", "use_arm", "pass",
                                                       "turns", "duration_s", "skill_calls", "hook")}), flush=True)

    for name in names:
        troot = os.path.join(root, name)
        for use_arm in a.use_arms.split(","):
            base = replay(troot, os.path.join(troot, "skill"), use_arm, a.max_turns, a.timeout)
            for case, r in base.items():
                emit({"task": name, "phase": "base", "case": case, "use_arm": use_arm, **r})
            for arm in a.arms.split(","):
                for rep in range(a.reps):
                    run.sweep()
                    tdir = tempfile.mkdtemp(prefix="evolve-")
                    old = "\n".join("- " + open(os.path.join(troot, "usage", c, "prompt.md")).read().strip()
                                    for c in sorted(os.listdir(os.path.join(troot, "usage"))) if c.startswith("old-"))
                    open(os.path.join(tdir, "prompt.md"), "w").write(create.invoke(arm, REQUEST.format(
                        failure=open(os.path.join(troot, "failure.md")).read().strip(), old=old), a.meta))
                    open(os.path.join(tdir, "check.py"), "w").write(
                        "import os, sys\nsys.exit(0 if os.path.isfile(os.path.join(sys.argv[1], 'candidate', "
                        "'SKILL.md')) else 1)\n")
                    shutil.copytree(os.path.join(troot, "skill"), os.path.join(tdir, "files", "skill"))
                    create.meta_skill(arm, tdir)
                    e = run.run_one(tdir, ("hook" if a.meta == "hook" else "skills") if arm != "none" else "none",
                                    a.max_turns, a.timeout)
                    emit({"task": name, "arm": arm, "rep": rep, "phase": "evolve", "use_arm": use_arm, **e})
                    if not e["pass"]:
                        summary.append((name, arm, use_arm, None, base))
                        continue
                    keep_candidate(os.path.join(e["ws"], "candidate"),
                                   os.path.join(a.out_dir, "candidates", f"{name}-{arm}-{use_arm}-{rep}"))
                    cand = replay(troot, os.path.join(e["ws"], "candidate"), use_arm, a.max_turns, a.timeout)
                    for case, r in cand.items():
                        emit({"task": name, "arm": arm, "rep": rep, "phase": "candidate", "case": case,
                              "use_arm": use_arm, **r})
                    summary.append((name, arm, use_arm, cand, base))
    print("\n| task | evolver | use | candidate made | fixed | regressed | held-out passes | base (cases in name order) |")
    print("|---|---|---|---|---|---|---|---|")
    for name, arm, use_arm, cand, base in summary:
        b = "/".join("P" if base[c]["pass"] else "F" for c in sorted(base))
        if cand is None:
            print(f"| {name} | {arm} | {use_arm} | no | - | - | - | {b} |")
            continue
        regressed = [c for c in base if base[c]["pass"] and not cand[c]["pass"]]
        print(f"| {name} | {arm} | {use_arm} | yes | {'yes' if cand['fail-case']['pass'] else 'no'} | "
              f"{','.join(regressed) or 'none'} | {'yes' if cand.get('heldout', {}).get('pass') else 'no'} | {b} |")


if __name__ == "__main__":
    main()
