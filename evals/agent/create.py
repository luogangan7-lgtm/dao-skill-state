#!/usr/bin/env python3
"""Evaluation B: does a meta-skill make better skills? Two phases per creation task and arm.

  create.py CREATE_TASKS_DIR OUT_DIR [--tasks a,b] [--arms none,skill-creator,dao] [--reps 1]
            [--use-arms skills,hook]

1. Create: Claude Code gets the task's request.md (team rules to capture) and must write the skill
   into ./new-skill/. Arm none has no meta-skill; skill-creator gets anthropics/skills'
   skill-creator (pinned, from $SKILL_SOURCES); dao gets this repository's dao-skill-state.
2. Use: the created skill is installed in a fresh workspace and each usage task (whose prompt never
   restates the rules) runs with it (arms from --use-arms), checked by the usage task's check.py.
   A baseline runs every usage task once with no skill, to show the rules cannot be guessed.

Also records whether new-skill/ is a structurally valid skill (dao-skill-state's check_skill.py).
Writes OUT_DIR/create.jsonl and prints a summary. CI only (.github/workflows/agent-eval.yml).

Depends on: run.py (same directory) for running the agent and checkers.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUTPUT_RULE = ("\n\nWrite the skill into the directory ./new-skill/ (its SKILL.md and any files it needs). "
               "Do not do the task the skill is about; only make the skill.")
CHECK_CREATED = '''import os, sys
sys.exit(0 if os.path.isfile(os.path.join(sys.argv[1], "new-skill", "SKILL.md")) else 1)
'''


def meta_skill(arm, dest):
    """Install the arm's meta-skill as a project skill under dest/skills/."""
    if arm == "skill-creator":
        src = os.path.join(os.environ["SKILL_SOURCES"], "anthropics_skills", "skills", "skill-creator")
        shutil.copytree(src, os.path.join(dest, "skills", "skill-creator"))
    elif arm == "dao":
        shutil.copytree(os.path.join(REPO, "dao-skill-state"), os.path.join(dest, "skills", "dao-skill-state"),
                        ignore=shutil.ignore_patterns("tests", "__pycache__"))


def structure_ok(skill_dir):
    p = subprocess.run([sys.executable, os.path.join(REPO, "dao-skill-state", "scripts", "check_skill.py"), skill_dir],
                       capture_output=True, text=True, timeout=60)
    return p.returncode == 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("tasks_dir")
    ap.add_argument("out_dir")
    ap.add_argument("--tasks", default="")
    ap.add_argument("--arms", default="none,skill-creator,dao")
    ap.add_argument("--use-arms", default="skills,hook")
    ap.add_argument("--reps", type=int, default=1)
    ap.add_argument("--max-turns", type=int, default=40)
    ap.add_argument("--timeout", type=int, default=1200)
    a = ap.parse_args()
    if a.arms == "none,skills":  # the workflow's generic default: mean all creator arms here
        a.arms = "none,skill-creator,dao"
    bad = set(a.arms.split(",")) - {"none", "skill-creator", "dao"}
    if bad:
        sys.exit(f"unknown creator arms: {sorted(bad)} (use none, skill-creator, dao)")
    root = os.path.abspath(a.tasks_dir)
    names = [t for t in a.tasks.split(",") if t] or sorted(
        d for d in os.listdir(root) if os.path.isfile(os.path.join(root, d, "request.md")))
    os.makedirs(a.out_dir, exist_ok=True)
    out = open(os.path.join(a.out_dir, "create.jsonl"), "a")
    rows = []

    def emit(r):
        rows.append(r)
        out.write(json.dumps(r) + "\n")
        out.flush()
        print(json.dumps({k: r[k] for k in r if k in ("task", "arm", "phase", "usage", "use_arm", "rep", "pass",
                                                       "valid", "turns", "duration_s", "skill_calls", "hook")}),
              flush=True)

    for name in names:
        usage = sorted(os.listdir(os.path.join(root, name, "usage")))
        for u in usage:  # baseline: the rules are not guessable
            r = run.run_one(os.path.join(root, name, "usage", u), "none", a.max_turns, a.timeout)
            emit({"task": name, "phase": "baseline", "usage": u, **r})
        for arm in a.arms.split(","):
            for rep in range(a.reps):
                # phase 1: create, in a throwaway task dir holding the request and the meta-skill
                tdir = tempfile.mkdtemp(prefix="create-")
                open(os.path.join(tdir, "prompt.md"), "w").write(
                    open(os.path.join(root, name, "request.md")).read() + OUTPUT_RULE)
                open(os.path.join(tdir, "check.py"), "w").write(CHECK_CREATED)
                meta_skill(arm, tdir)
                c = run.run_one(tdir, "skills" if arm != "none" else "none", a.max_turns, a.timeout)
                created = os.path.join(c.get("ws", ""), "new-skill")
                valid = bool(c["pass"]) and structure_ok(created)
                emit({"task": name, "arm": arm, "rep": rep, "phase": "create", "valid": valid, **c})
                if not c["pass"]:
                    continue
                # phase 2: use the created skill on every usage task
                for u in usage:
                    udir = tempfile.mkdtemp(prefix="use-")
                    shutil.copytree(os.path.join(root, name, "usage", u), udir, dirs_exist_ok=True)
                    shutil.copytree(created, os.path.join(udir, "skills", "created-skill"))
                    for use_arm in a.use_arms.split(","):
                        r = run.run_one(udir, use_arm, a.max_turns, a.timeout)
                        emit({"task": name, "arm": arm, "rep": rep, "phase": "use", "usage": u,
                              "use_arm": use_arm, **r})
    print("\n| task | creator arm | created | valid | use: skills pass | use: hook pass | baseline pass |")
    print("|---|---|---|---|---|---|---|")
    for name in names:
        base = [r for r in rows if r["task"] == name and r["phase"] == "baseline"]
        for arm in a.arms.split(","):
            cr = [r for r in rows if r["task"] == name and r.get("arm") == arm and r["phase"] == "create"]
            us = {ua: [r for r in rows if r["task"] == name and r.get("arm") == arm and r["phase"] == "use"
                       and r["use_arm"] == ua] for ua in a.use_arms.split(",")}
            cell = lambda rs: f"{sum(r['pass'] for r in rs)}/{len(rs)}"
            print(f"| {name} | {arm} | {cell(cr)} | {sum(r['valid'] for r in cr)}/{len(cr)} | "
                  f"{cell(us.get('skills', []))} | {cell(us.get('hook', []))} | {cell(base)} |")


if __name__ == "__main__":
    main()
