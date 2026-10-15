#!/usr/bin/env python3
"""Fixed steps vs stateful skill: the same interrupted task, resumed by a fresh session.

  resume.py RESUME_TASKS_DIR OUT_DIR [--tasks a,b] [--arms none,fixed,stateful,...] [--reps 1]

A task holds files/ (the workspace), forms/<arm>/ (one skill per form, identical rules except
that the stateful one keeps progress in a file), first.md and resume.md (the two prompts) and
check.py (the final state after both sessions). Session 1 runs until a step fails on an outage
(upstream.status says down); the harness then ends the outage and session 2, a fresh agent with
no memory of session 1, continues in the same workspace. Arm none has no skill.

Arm `hook:<form>` tests routing instead of naming the skill: the prompts do not mention it, the
task's distractors.json skills (pinned third-party refs, run.py's skills.json format) are installed
beside it, and the real skill-hub hook is registered (run.py arm hook). The row records what the
hook picked in each session and whether session 2 loaded the runbook.

Arm `make:<creator>` (creator none, skill-creator or dao, as in create.py) tests form choice: before
each rep the creator writes the skill from the task's request.md, which describes the situation
(cut-off sessions, loads that must not repeat, whether the warehouse can be asked) but not the
mechanism; both sessions then run with that skill, named in the prompt. The skill is kept in
OUT_DIR/created/ for reading which form it chose.

Records per run: pass, steps rerun (a step that ran twice), whether session 1 tampered with the
outage, and turns / tokens / seconds of each session. Writes OUT_DIR/resume.jsonl and prints a
summary. CI only (.github/workflows/agent-eval.yml). Depends on: run.py (same directory).
"""
import argparse
import json
import os
import re
import secrets
import shutil
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import create  # noqa: E402
import evolve  # noqa: E402
import run  # noqa: E402

ALWAYS = "import sys\nsys.exit(0)\n"  # session 1 is judged only by the final check


def form_of(arm):
    """(form, routed by the hook?) for an arm name: none, <form> or hook:<form>."""
    return (arm[5:], True) if arm.startswith("hook:") else (arm, False)


def make_skill(troot, creator, keep, a):
    """Phase 0 of a make:<creator> arm: (skill dir, name, create run) or (None, None, run)."""
    t = tempfile.mkdtemp(prefix="make-")
    open(os.path.join(t, "prompt.md"), "w").write(create.invoke(
        creator, open(os.path.join(troot, "request.md")).read() + create.OUTPUT_RULE, "explicit"))
    open(os.path.join(t, "check.py"), "w").write(create.CHECK_CREATED)
    create.meta_skill(creator, t)
    c = run.run_one(t, "none" if creator == "none" else "skills", a.max_turns, a.timeout)
    d = os.path.join(c.get("ws", ""), "new-skill")
    if not c["pass"]:
        return None, None, c
    evolve.keep_candidate(d, keep)
    m = re.search(r"^name:\s*['\"]?([a-z0-9-]{1,64})['\"]?\s*$", open(os.path.join(d, "SKILL.md")).read(), re.M)
    return d, m.group(1) if m else "created-skill", c  # agent-written: only a plain name becomes a folder


def session(troot, arm, files, prompt, check, made=None):
    """A throwaway run.py task dir: workspace files, the arm's skill, one prompt, one checker."""
    t = tempfile.mkdtemp(prefix="resume-")
    shutil.copytree(files, os.path.join(t, "files"), ignore=shutil.ignore_patterns(".claude"))
    form, hook = form_of(arm)
    if made:  # (dir, name) of a skill a creator just wrote
        shutil.copytree(made[0], os.path.join(t, "skills", made[1]))
        prompt = f"Use the {made[1]} skill.\n\n" + prompt
    elif arm != "none":
        shutil.copytree(os.path.join(troot, "forms", form), os.path.join(t, "skills"))
    if made:
        pass  # named above
    elif hook:  # found by the hook among distractors, never named
        shutil.copy(os.path.join(troot, "distractors.json"), os.path.join(t, "skills.json"))
    elif arm != "none":
        prompt = "Use the backfill-runbook skill.\n\n" + prompt
    open(os.path.join(t, "prompt.md"), "w").write(prompt)
    open(os.path.join(t, "check.py"), "w").write(check)
    return t


def expect_s1(troot):
    """The steps a session 1 that follows the runbook leaves done (task file expect_s1.json)."""
    p = os.path.join(troot, "expect_s1.json")
    return json.load(open(p)) if os.path.exists(p) else [1, 2, 3, 4]


def steps_ran(ws):
    """The audit: in the workspace (.audit/steps.log), or in the external warehouse store."""
    wid = os.path.join(ws, ".warehouse_id")
    if os.path.exists(wid):
        p = os.path.join(os.environ.get("WAREHOUSE_DIR", "/tmp/warehouse"), open(wid).read().strip() + ".log")
    else:
        p = os.path.join(ws, ".audit", "steps.log")
    return [int(x) for x in open(p).read().split()] if os.path.exists(p) else []


def tokens(r):
    """Fresh tokens: input + output + cache writes (cache reads are cheap and would swamp the sum)."""
    return sum(r.get(k) or 0 for k in ("input_tokens", "cache_write", "output_tokens"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("tasks_dir")
    ap.add_argument("out_dir")
    ap.add_argument("--tasks", default="")
    ap.add_argument("--arms", default="none,fixed,stateful")
    ap.add_argument("--reps", type=int, default=1)
    ap.add_argument("--max-turns", type=int, default=40)
    ap.add_argument("--timeout", type=int, default=1200)
    a = ap.parse_args()
    if a.arms == "none,skills":  # the workflow's generic default
        a.arms = "none,fixed,stateful"
    makes = {x for x in a.arms.split(",") if x.startswith("make:")}
    bad = {x for x in makes if x[5:] not in ("none", "skill-creator", "dao")}
    bad |= {x for x in set(a.arms.split(",")) - makes
            if x != "none" and not form_of(x)[0].replace("-", "").isalnum()}
    if bad:
        sys.exit(f"bad arm names: {sorted(bad)} (none or a forms/<name> of the task)")
    root = os.path.abspath(a.tasks_dir)
    names = [t for t in a.tasks.split(",") if t] or sorted(
        d for d in os.listdir(root) if os.path.isfile(os.path.join(root, d, "check.py")))
    unknown = {x for x in makes if not any(os.path.exists(os.path.join(root, n, "request.md")) for n in names)}
    unknown |= {x for x in set(a.arms.split(",")) - makes if x != "none"
               and not any(os.path.isdir(os.path.join(root, n, "forms", form_of(x)[0])) for n in names)}
    unknown |= {x for x in a.arms.split(",") if form_of(x)[1]
                and not any(os.path.exists(os.path.join(root, n, "distractors.json")) for n in names)}
    if unknown:  # a typo would otherwise produce no rows and no error
        sys.exit(f"arms in no selected task: {sorted(unknown)}")
    os.makedirs(a.out_dir, exist_ok=True)
    log = open(os.path.join(a.out_dir, "resume.jsonl"), "a")
    rows = []
    for name in names:
        troot = os.path.join(root, name)
        for arm in a.arms.split(","):
            form, hook = form_of(arm)
            use = "none" if arm == "none" else "hook" if hook else "skills"
            if arm.startswith("make:"):
                if not os.path.exists(os.path.join(troot, "request.md")):
                    continue  # nothing to create from
            elif arm != "none" and not os.path.isdir(os.path.join(troot, "forms", form)):
                continue  # this task has no such form
            if hook and not os.path.exists(os.path.join(troot, "distractors.json")):
                continue  # routing needs something to route between
            for rep in range(a.reps):
                made, mk = None, None
                if arm.startswith("make:"):
                    tag = f"{name}-{arm[5:]}-{rep}"
                    d, sname, mk = make_skill(troot, arm[5:], os.path.join(a.out_dir, "created", tag), a)
                    if d is None:
                        row = {"task": name, "arm": arm, "rep": rep, "pass": False, "made": False,
                               "make": {k: mk.get(k) for k in ("turns", "duration_s", "error", "final")}}
                        rows.append(row)  # a creator that wrote no skill fails the rep
                        log.write(json.dumps(row) + "\n")
                        log.flush()
                        continue
                    made = (d, sname)
                t1 = session(troot, arm, os.path.join(troot, "files"), open(os.path.join(troot, "first.md")).read(),
                             ALWAYS, made)
                step = os.path.join(troot, "files", "step.py")
                if os.path.exists(step) and "import warehouse" in open(step).read():  # this run's key in the store
                    open(os.path.join(t1, "files", ".warehouse_id"), "w").write(secrets.token_hex(8) + "\n")
                r1 = run.run_one(t1, use, a.max_turns, a.timeout)
                ws1 = r1["ws"]
                status = os.path.join(ws1, "upstream.status")
                tampered = not os.path.exists(status) or open(status).read().strip() != "down"
                after1 = steps_ran(ws1)
                pj = os.path.join(ws1, ".backfill", "progress.json")
                progress1 = os.path.exists(pj)
                progress_text = open(pj).read()[:300] if progress1 else None  # the mechanism under test
                open(status, "w").write("up\n")  # the outage is over
                t2 = session(troot, arm, ws1, open(os.path.join(troot, "resume.md")).read(),
                             open(os.path.join(troot, "check.py")).read(), made)
                r2 = run.run_one(t2, use, a.max_turns, a.timeout)
                ran = steps_ran(r2["ws"])
                row = {"task": name, "arm": arm, "rep": rep, "pass": bool(r2["pass"]) and not tampered,
                       "check": r2.get("check"), "tampered": tampered, "after_session1": after1, "ran": ran,
                       "clean_stop": after1 == expect_s1(troot), "progress_file_after_s1": progress1,
                       "progress_after_s1": progress_text,
                       "reruns": len(ran) - len(set(ran)), "missed": sorted(set(range(1, 9)) - set(ran)),
                       "s1": {k: r1.get(k) for k in ("turns", "duration_s", "skill_calls", "final", "error")},
                       "s2": {k: r2.get(k) for k in ("turns", "duration_s", "skill_calls", "final", "error", "bash")},
                       "s1_tokens": tokens(r1), "s2_tokens": tokens(r2),
                       "hook": [r1.get("hook"), r2.get("hook")],
                       "s2_loaded_runbook": (made[1] if made else "backfill-runbook") in json.dumps(r2.get("skill_calls"))}
                if mk is not None:
                    row.update(made=True, skill_name=made[1],
                               make={k: mk.get(k) for k in ("turns", "duration_s", "skill_calls")}, make_tokens=tokens(mk))
                rows.append(row)
                log.write(json.dumps(row) + "\n")
                log.flush()
                print(json.dumps({k: row[k] for k in ("task", "arm", "rep", "pass", "after_session1", "ran",
                                                      "tampered", "s2_tokens", "hook", "s2_loaded_runbook")}), flush=True)
    print("\n| task | arm | pass | pass after a clean stop | runs with a rerun | runs that missed a step "
          "| session 1 tampered | session 2 loaded the runbook | mean turns s1 / s2 | mean fresh tokens s1 / s2 |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for name in names:
        for arm in a.arms.split(","):
            rs = [r for r in rows if r["task"] == name and r["arm"] == arm]
            if not rs:
                continue
            mean = lambda xs: sum(xs) / len(xs) if xs else 0
            ran = [r for r in rs if r.get("made", True)]  # a make: rep whose creator wrote no skill ran no sessions
            turns = lambda s: [r[s]["turns"] for r in ran if r[s]["turns"] is not None]  # errored runs left out
            clean = [r for r in ran if r["clean_stop"]]
            print(f"| {name} | {arm} | {sum(r['pass'] for r in rs)}/{len(rs)} | {sum(r['pass'] for r in clean)}/{len(clean)} "
                  f"| {sum(r['reruns'] > 0 for r in ran)} | {sum(bool(r['missed']) for r in ran)} | {sum(r['tampered'] for r in ran)} | {sum(r['s2_loaded_runbook'] for r in ran)} "
                  f"| {mean(turns('s1')):.1f} / {mean(turns('s2')):.1f} "
                  f"| {mean([r['s1_tokens'] for r in ran]):.0f} / {mean([r['s2_tokens'] for r in ran]):.0f} |")

if __name__ == "__main__":
    main()
