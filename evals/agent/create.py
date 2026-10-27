#!/usr/bin/env python3
"""Evaluation B: does a meta-skill make better skills? Two phases per creation task and arm.

  create.py CREATE_TASKS_DIR OUT_DIR [--tasks a,b] [--arms none,skill-creator,dao] [--reps 1]
            [--use-arms skills,hook]

1. Create: Claude Code gets the task's request.md (team rules to capture) and must write the skill
   into its own folder under ./out/. Arm none has no meta-skill; skill-creator gets anthropics/skills'
   skill-creator (pinned, from $SKILL_SOURCES); dao gets this repository's dao-skill-state.
2. Use: the created skill is installed in a fresh workspace and each usage task (whose prompt never
   restates the rules) runs with it (arms from --use-arms), checked by the usage task's check.py.
   A baseline runs every usage task once with no skill, to show the rules cannot be guessed.

Also records whether the created skill is structurally valid (dao-skill-state's check_skill.py).
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
# the creator names the folder: a fixed staging name (./new-skill/) made dao's lint force that name
# onto the skill, and hosts choose skills by name and description (eval H, run 40445025600)
OUTPUT_RULE = ("\n\nWrite the skill into its own folder under ./out/, named after the skill "
               "(./out/<skill-name>/SKILL.md and any files it needs). "
               "Do not do the task the skill is about; only make the skill.")
CHECK_CREATED = '''import os, sys
out = os.path.join(sys.argv[1], "out")
dirs = os.listdir(out) if os.path.isdir(out) else []
sys.exit(0 if not os.path.islink(out) and len(dirs) == 1 and not os.path.islink(os.path.join(out, dirs[0]))
         and os.path.isfile(os.path.join(out, dirs[0], "SKILL.md")) else 1)
'''


META_NAME = {"skill-creator": "skill-creator", "dao": "dao-skill-state"}


def invoke(arm, prompt, how):
    """How the meta-skill is offered. explicit: the request names it, as its own docs ask
    (dao-skill-state is explicit-only: allow_implicit_invocation false, so skill-hub never surfaces
    it); hook / installed: left to the hook or the host."""
    if arm in META_NAME and how == "explicit":
        return f"Use the {META_NAME[arm]} skill for this.\n\n" + prompt
    return prompt


def meta_skill(arm, dest):
    """Install the arm's meta-skill as a project skill under dest/skills/."""
    if arm == "skill-creator":
        src = os.path.join(os.environ["SKILL_SOURCES"], "anthropics_skills", "skills", "skill-creator")
        shutil.copytree(src, os.path.join(dest, "skills", "skill-creator"))
    elif arm == "dao":
        shutil.copytree(os.path.join(REPO, "dao-skill-state"), os.path.join(dest, "skills", "dao-skill-state"),
                        ignore=shutil.ignore_patterns("tests", "__pycache__"))


def created_dir(ws):
    """The one skill folder the creator wrote under ws/out/ (what CHECK_CREATED accepted), or None."""
    out = os.path.join(ws, "out")
    dirs = os.listdir(out) if os.path.isdir(out) and not os.path.islink(out) else []
    d = os.path.join(out, dirs[0]) if len(dirs) == 1 else None
    return d if d and os.path.isdir(d) and not os.path.islink(d) else None


def structure_ok(skill_dir):
    """dao-skill-state's format lint, on a copy named after the skill (the lint requires the folder
    name to equal the frontmatter name; the creator may have chosen another folder name)."""
    import re
    md = os.path.join(skill_dir, "SKILL.md")
    m = re.search(r"^name:\s*['\"]?([^'\"\n]+)", open(md, encoding="utf-8").read(), re.M) if os.path.exists(md) else None
    name = m.group(1).split("#")[0].strip() if m else ""
    # the name is agent-written: only a plain skill name may become a folder (the lint rejects others anyway)
    if not re.fullmatch(r"[a-z0-9-]{1,64}", name):
        return False
    tmp = tempfile.mkdtemp(prefix="lint-")
    try:
        shutil.copytree(skill_dir, os.path.join(tmp, name))
        p = subprocess.run([sys.executable, os.path.join(REPO, "dao-skill-state", "scripts", "check_skill.py"),
                            os.path.join(tmp, name)], capture_output=True, text=True, timeout=60)
        return p.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("tasks_dir")
    ap.add_argument("out_dir")
    ap.add_argument("--tasks", default="")
    ap.add_argument("--arms", default="none,skill-creator,dao")
    ap.add_argument("--use-arms", default="skills,hook")
    ap.add_argument("--meta", default="explicit", choices=["explicit", "hook", "installed"],
                    help="how the meta-skill is offered: named in the request (default), surfaced by the "
                         "skill-hub hook, or only installed")
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
        d for d in os.listdir(root) if os.path.isfile(os.path.join(root, d, "request.md"))
        or os.path.isdir(os.path.join(root, d, "replay")))
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
            run.sweep()
            r = run.run_one(os.path.join(root, name, "usage", u), "none", a.max_turns, a.timeout)
            run.keep_files(os.path.join(root, name, "usage", u), r["ws"], os.path.join(a.out_dir, "kept", f"{name}-baseline-{u}"))
            emit({"task": name, "phase": "baseline", "usage": u, **r})

        def use(arm, rep, created, use_arms):
            """Phase 2: install the created skill and run every usage task with it."""
            for u in usage:
                udir = tempfile.mkdtemp(prefix="use-")
                shutil.copytree(os.path.join(root, name, "usage", u), udir, dirs_exist_ok=True)
                shutil.copytree(created, os.path.join(udir, "skills", os.path.basename(created)),  # links stay out
                                ignore=lambda d, names: [n for n in names if os.path.islink(os.path.join(d, n))])
                for use_arm in use_arms:
                    r = run.run_one(udir, use_arm, a.max_turns, a.timeout)
                    run.keep_files(udir, r["ws"], os.path.join(a.out_dir, "kept", f"{name}-{arm}-{rep}-{u}-{use_arm}"))
                    emit({"task": name, "arm": arm, "rep": rep, "phase": "use", "usage": u, "use_arm": use_arm, **r})

        replay = os.path.join(root, name, "replay")
        if os.path.isdir(replay):  # skills saved from an earlier run: use phase only, loaded from the skill list
            for label in sorted(os.listdir(replay)):
                skills = [d for d in os.listdir(os.path.join(replay, label))
                          if os.path.isfile(os.path.join(replay, label, d, "SKILL.md"))]
                if len(skills) != 1:
                    sys.exit(f"{replay}/{label}: expected one skill folder holding SKILL.md, found {skills}")
                skill = skills[0]
                for rep in range(a.reps):
                    run.sweep()
                    use(label, rep, os.path.join(replay, label, skill), ["skills"])
            continue
        for arm in a.arms.split(","):
            for rep in range(a.reps):
                run.sweep()
                # phase 1: create, in a throwaway task dir holding the request and the meta-skill
                tdir = tempfile.mkdtemp(prefix="create-")
                open(os.path.join(tdir, "prompt.md"), "w").write(invoke(
                    arm, open(os.path.join(root, name, "request.md")).read() + OUTPUT_RULE, a.meta))
                open(os.path.join(tdir, "check.py"), "w").write(CHECK_CREATED)
                meta_skill(arm, tdir)
                c = run.run_one(tdir, ("hook" if a.meta == "hook" else "skills") if arm != "none" else "none",
                                a.max_turns, a.timeout)
                created = created_dir(c.get("ws", ""))
                valid = bool(c["pass"]) and created is not None and structure_ok(created)
                emit({"task": name, "arm": arm, "rep": rep, "phase": "create", "valid": valid, **c})
                # keep the created skill, redacted, so a reviewer can see why a use run did or did not load it
                for d, _, fs in os.walk(created) if created else ():
                    for f in fs:
                        src = os.path.join(d, f)
                        if os.path.islink(src) or os.path.getsize(src) > 1 << 20:
                            continue
                        dst = os.path.join(a.out_dir, "kept", f"{name}-{arm}-{rep}-skill", os.path.relpath(src, created))
                        os.makedirs(os.path.dirname(dst), exist_ok=True)
                        with open(src, errors="replace") as fi, open(dst, "w") as fo:
                            fo.write(run.redact(fi.read()))
                if not c["pass"] or created is None:
                    continue
                use(arm, rep, created, a.use_arms.split(","))
    print("\n| task | creator arm | created | valid | use: skills pass | use: hook pass | baseline pass |")
    print("|---|---|---|---|---|---|---|")
    for name in names:
        base = [r for r in rows if r["task"] == name and r["phase"] == "baseline"]
        for arm in dict.fromkeys(r["arm"] for r in rows if r["task"] == name and "arm" in r):  # replay: labels
            cr = [r for r in rows if r["task"] == name and r.get("arm") == arm and r["phase"] == "create"]
            us = {ua: [r for r in rows if r["task"] == name and r.get("arm") == arm and r["phase"] == "use"
                       and r["use_arm"] == ua] for ua in a.use_arms.split(",")}
            cell = lambda rs: f"{sum(r['pass'] for r in rs)}/{len(rs)}"
            print(f"| {name} | {arm} | {cell(cr)} | {sum(r['valid'] for r in cr)}/{len(cr)} | "
                  f"{cell(us.get('skills', []))} | {cell(us.get('hook', []))} | {cell(base)} |")


if __name__ == "__main__":
    main()
