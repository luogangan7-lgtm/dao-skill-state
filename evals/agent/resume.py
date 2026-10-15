#!/usr/bin/env python3
"""Fixed steps vs stateful skill: the same interrupted task, resumed by a fresh session.

  resume.py RESUME_TASKS_DIR OUT_DIR [--tasks a,b] [--arms none,fixed,stateful] [--reps 1]

A task holds files/ (the workspace), forms/<arm>/ (one skill per form, identical rules except
that the stateful one keeps progress in a file), first.md and resume.md (the two prompts) and
check.py (the final state after both sessions). Session 1 runs until a step fails on an outage
(upstream.status says down); the harness then ends the outage and session 2, a fresh agent with
no memory of session 1, continues in the same workspace. Arm none has no skill.

Records per run: pass, steps rerun (a step that ran twice), whether session 1 tampered with the
outage, and turns / tokens / seconds of each session. Writes OUT_DIR/resume.jsonl and prints a
summary. CI only (.github/workflows/agent-eval.yml). Depends on: run.py (same directory).
"""
import argparse
import json
import os
import secrets
import shutil
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run  # noqa: E402

ALWAYS = "import sys\nsys.exit(0)\n"  # session 1 is judged only by the final check


def session(troot, arm, files, prompt, check):
    """A throwaway run.py task dir: workspace files, the arm's skill, one prompt, one checker."""
    t = tempfile.mkdtemp(prefix="resume-")
    shutil.copytree(files, os.path.join(t, "files"), ignore=shutil.ignore_patterns(".claude"))
    if arm != "none":
        shutil.copytree(os.path.join(troot, "forms", arm), os.path.join(t, "skills"))
        prompt = "Use the backfill-runbook skill.\n\n" + prompt
    open(os.path.join(t, "prompt.md"), "w").write(prompt)
    open(os.path.join(t, "check.py"), "w").write(check)
    return t


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
    bad = set(a.arms.split(",")) - {"none", "fixed", "stateful"}
    if bad:
        sys.exit(f"unknown arms: {sorted(bad)} (use none, fixed, stateful)")
    root = os.path.abspath(a.tasks_dir)
    names = [t for t in a.tasks.split(",") if t] or sorted(
        d for d in os.listdir(root) if os.path.isfile(os.path.join(root, d, "check.py")))
    os.makedirs(a.out_dir, exist_ok=True)
    log = open(os.path.join(a.out_dir, "resume.jsonl"), "a")
    rows = []
    for name in names:
        troot = os.path.join(root, name)
        for arm in a.arms.split(","):
            use = "none" if arm == "none" else "skills"
            for rep in range(a.reps):
                t1 = session(troot, arm, os.path.join(troot, "files"), open(os.path.join(troot, "first.md")).read(),
                             ALWAYS)
                if name.endswith("-external"):  # this run's key in the external store
                    open(os.path.join(t1, "files", ".warehouse_id"), "w").write(secrets.token_hex(8) + "\n")
                r1 = run.run_one(t1, use, a.max_turns, a.timeout)
                ws1 = r1["ws"]
                status = os.path.join(ws1, "upstream.status")
                tampered = not os.path.exists(status) or open(status).read().strip() != "down"
                after1 = steps_ran(ws1)
                progress1 = os.path.exists(os.path.join(ws1, ".backfill", "progress.json"))
                open(status, "w").write("up\n")  # the outage is over
                t2 = session(troot, arm, ws1, open(os.path.join(troot, "resume.md")).read(),
                             open(os.path.join(troot, "check.py")).read())
                r2 = run.run_one(t2, use, a.max_turns, a.timeout)
                ran = steps_ran(r2["ws"])
                row = {"task": name, "arm": arm, "rep": rep, "pass": bool(r2["pass"]) and not tampered,
                       "check": r2.get("check"), "tampered": tampered, "after_session1": after1, "ran": ran,
                       "clean_stop": after1 == [1, 2, 3, 4], "progress_file_after_s1": progress1,
                       "reruns": len(ran) - len(set(ran)), "missed": sorted(set(range(1, 9)) - set(ran)),
                       "s1": {k: r1.get(k) for k in ("turns", "duration_s", "skill_calls", "final", "error")},
                       "s2": {k: r2.get(k) for k in ("turns", "duration_s", "skill_calls", "final", "error", "bash")},
                       "s1_tokens": tokens(r1), "s2_tokens": tokens(r2)}
                rows.append(row)
                log.write(json.dumps(row) + "\n")
                log.flush()
                print(json.dumps({k: row[k] for k in ("task", "arm", "rep", "pass", "after_session1", "ran",
                                                      "tampered", "s2_tokens")}), flush=True)
    print("\n| task | arm | pass | pass after a clean stop (1-4) | runs with a rerun | runs that missed a step "
          "| session 1 tampered | mean turns s1 / s2 | mean fresh tokens s1 / s2 |")
    print("|---|---|---|---|---|---|---|---|---|")
    for name in names:
        for arm in a.arms.split(","):
            rs = [r for r in rows if r["task"] == name and r["arm"] == arm]
            if not rs:
                continue
            mean = lambda xs: sum(xs) / len(xs) if xs else 0
            turns = lambda s: [r[s]["turns"] for r in rs if r[s]["turns"] is not None]  # errored runs left out
            clean = [r for r in rs if r["clean_stop"]]
            print(f"| {name} | {arm} | {sum(r['pass'] for r in rs)}/{len(rs)} | {sum(r['pass'] for r in clean)}/{len(clean)} "
                  f"| {sum(r['reruns'] > 0 for r in rs)} | {sum(bool(r['missed']) for r in rs)} | {sum(r['tampered'] for r in rs)} "
                  f"| {mean(turns('s1')):.1f} / {mean(turns('s2')):.1f} "
                  f"| {mean([r['s1_tokens'] for r in rs]):.0f} / {mean([r['s2_tokens'] for r in rs]):.0f} |")


if __name__ == "__main__":
    main()
