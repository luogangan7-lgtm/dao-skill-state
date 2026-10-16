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
import ast
import hashlib
import json
import os
import re
import secrets
import shlex
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import create  # noqa: E402
import evolve  # noqa: E402
import run  # noqa: E402

ALWAYS = "import sys\nsys.exit(0)\n"  # session 1 is judged only by the final check
# run.py already redacts these fields; failures and successes retain the same maker evidence.
MAKE_EVIDENCE_FIELDS = ("turns", "duration_s", "skill_calls", "rc", "is_error", "error",
                        "tool_trace", "trace_truncated", "final", "check", "subtype", "errors", "diagnostics_truncated")


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
    c = run.run_one(t, "none" if creator == "none" else "skills", a.make_max_turns, a.make_timeout)
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
        shutil.copytree(made[0], os.path.join(t, "skills", made[1]),  # agent-written: links stay out
                        ignore=lambda d, names: [n for n in names if os.path.islink(os.path.join(d, n))])
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


def warehouse_events(ws):
    """The external audit also distinguishes a failed attempt from a committed action."""
    wid = os.path.join(ws, ".warehouse_id")
    if not os.path.isfile(wid):
        return []
    path = os.path.join(os.environ.get("WAREHOUSE_DIR", "/tmp/warehouse"),
                        open(wid).read().strip() + ".events.jsonl")
    return [json.loads(line) for line in open(path)] if os.path.isfile(path) else []


def file_hashes(root):
    """Bind fixtures and generated skills to the bytes used, without copying private run files."""
    return {os.path.relpath(os.path.join(d, n), root): hashlib.sha256(
            open(os.path.join(d, n), "rb").read()).hexdigest()
            for d, _, names in os.walk(root) for n in sorted(names)
            if not os.path.islink(os.path.join(d, n)) and "__pycache__" not in d}


def probed(*runs):
    """Count observed backend/oracle access in all phases; this bounded heuristic is not a sandbox.
    Inspect execution/path fields, never descriptions, agent prompts or Write/Edit contents.
    Trial warehouse.txt/mock modules and the task's own helpers remain ordinary task inputs.
    """
    forbidden = (r"/tmp/warehouse(?=/|[\s'\";$*]|$)|\$\{?WAREHOUSE_DIR\b|"
                 r"(?:site|dist)-packages/warehouse(?:\.py|/)|"
                 r"(?:^|/)evals/agent(?=/|[\s'\";]|$)|"
                 r"/tmp/(?:resume|make)-[A-Za-z0-9_]{8}(?=/|[\s'\";]|$)")
    count = 0
    for run in runs:
        for event in run.get("tool_trace") or []:
            if not isinstance(event, dict) or not isinstance(event.get("input"), dict):
                count += 1  # An unreadable observation cannot establish a clean boundary.
                continue
            inputs, name = event.get("input") or {}, event.get("name")
            if name == "Bash":
                command = str(inputs.get("command", ""))
                # A cat heredoc creates mock code/data; its body is not executed Python.
                command = re.sub(r"(^[^\n]*\bcat\b[^\n]*<<-?\s*['\"]?(\w+)['\"]?[^\n]*\n).*?^\s*\2\s*$",
                                 lambda match: match[1], command, flags=re.M | re.S)
                try:
                    words = shlex.split(command, comments=True)
                except ValueError:
                    words = []
                codes = [words[i + 2] for i, word in enumerate(words[:-2])
                         if re.fullmatch(r"(?:.*/)?python[\d.]*", word) and words[i + 1] == "-c"]
                codes += [match[2] for match in re.finditer(
                    r"\bpython[\d.]*\b[^\n]*<<-?\s*['\"]?(\w+)['\"]?[^\n]*\n(.*?)^\s*\1\s*$",
                    command, flags=re.M | re.S)]
                imported = False
                for code in codes:
                    try:
                        imported |= any((isinstance(node, ast.Import) and any(a.name == "warehouse" for a in node.names))
                                        or (isinstance(node, ast.ImportFrom) and node.module == "warehouse")
                                        for node in ast.walk(ast.parse(code)))
                    except SyntaxError:
                        pass
                discovery = "find" in words and any(word in ("/", "/tmp", "/opt", "/usr") for word in words) and "warehouse.py" in words
                count += bool(imported or discovery or re.search(forbidden, command))
            elif name in ("Read", "Grep", "Glob", "Write", "Edit"):
                path = str(inputs.get("file_path") or inputs.get("path") or "")
                if name in ("Glob", "Grep"):
                    pattern = inputs.get("pattern", "") if name == "Glob" else inputs.get("glob", "")
                    path = os.path.join(path, str(pattern))
                count += bool(re.search(forbidden, path))
    return count


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
    ap.add_argument("--make-max-turns", type=int, help="maker only; default: --max-turns")
    ap.add_argument("--make-timeout", type=int, help="maker only; default: --timeout")
    a = ap.parse_args()
    a.make_max_turns = a.max_turns if a.make_max_turns is None else a.make_max_turns
    a.make_timeout = a.timeout if a.make_timeout is None else a.make_timeout
    if min(a.max_turns, a.timeout, a.make_max_turns, a.make_timeout) <= 0:
        ap.error("phase budgets must be positive integers")
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
    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    producer = {
        "sha": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip(),
        "authoring_sha256": hashlib.sha256(open(os.path.join(repo, "dao-skill-state", "references", "authoring.md"), "rb").read()).hexdigest(),
        "fixtures": {name: file_hashes(os.path.join(root, name)) for name in names},
        "tasks": names, "arms": a.arms.split(","), "reps": a.reps,
        "budgets": {"make": {"max_turns": a.make_max_turns, "timeout_s": a.make_timeout},
                    "s1": {"max_turns": a.max_turns, "timeout_s": a.timeout},
                    "s2": {"max_turns": a.max_turns, "timeout_s": a.timeout}},
        "github_run_id": os.environ.get("GITHUB_RUN_ID"),
        "github_run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
    }
    producer_bytes = (json.dumps(producer, sort_keys=True, indent=2) + "\n").encode("utf-8")
    with open(os.path.join(a.out_dir, "resume-producer.json"), "wb") as receipt:
        receipt.write(producer_bytes)
    producer_hash = hashlib.sha256(producer_bytes).hexdigest()
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
                               "probes": probed(mk),
                               "producer_sha256": producer_hash,
                               "make": {k: mk.get(k) for k in MAKE_EVIDENCE_FIELDS}}
                        rows.append(row)  # a creator that wrote no skill fails the rep
                        log.write(json.dumps(row) + "\n")
                        log.flush()
                        print(json.dumps({**{k: row[k] for k in ("task", "arm", "rep", "pass", "made")},
                                          "make": {k: row["make"][k] for k in ("turns", "rc", "is_error", "error", "final", "check", "trace_truncated", "subtype", "errors", "diagnostics_truncated")}}), flush=True)
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
                events1 = warehouse_events(ws1)
                pj = os.path.join(ws1, ".backfill", "progress.json")
                progress1 = os.path.exists(pj)
                progress_text = open(pj).read()[:300] if progress1 else None  # the mechanism under test
                open(status, "w").write("up\n")  # the outage is over
                # A task-system change (e.g. an operator pause), never an agent-created approval.
                resume_files = os.path.join(troot, "resume-files")
                if os.path.isdir(resume_files):
                    shutil.copytree(resume_files, ws1, dirs_exist_ok=True)
                t2 = session(troot, arm, ws1, open(os.path.join(troot, "resume.md")).read(),
                             open(os.path.join(troot, "check.py")).read(), made)
                r2 = run.run_one(t2, use, a.max_turns, a.timeout)
                ran = steps_ran(r2["ws"])
                events2 = warehouse_events(r2["ws"])
                row = {"task": name, "arm": arm, "rep": rep, "pass": bool(r2["pass"]) and not tampered,
                       "producer_sha256": producer_hash,
                       "check": r2.get("check"), "tampered": tampered, "after_session1": after1, "ran": ran,
                       "clean_stop": after1 == expect_s1(troot), "progress_file_after_s1": progress1,
                       "progress_after_s1": progress_text,
                       "reruns": len(ran) - len(set(ran)), "missed": sorted(set(range(1, 9)) - set(ran)),
                       "events_after_s1": events1, "events": events2,
                       "s1": {k: r1.get(k) for k in ("turns", "duration_s", "skill_calls", "final", "error", "bash", "rc", "is_error", "tool_trace", "trace_truncated", "subtype", "errors", "diagnostics_truncated")},
                       "probes": probed(mk or {}, r1, r2),
                       "s2": {k: r2.get(k) for k in ("turns", "duration_s", "skill_calls", "final", "error", "bash", "rc", "is_error", "tool_trace", "trace_truncated", "subtype", "errors", "diagnostics_truncated")},
                       "s1_tokens": tokens(r1), "s2_tokens": tokens(r2),
                       "hook": [r1.get("hook"), r2.get("hook")],
                       "s2_loaded_runbook": (made[1] if made else "backfill-runbook") in json.dumps(r2.get("skill_calls"))}
                if mk is not None:
                    row.update(made=True, skill_name=made[1],
                               make={k: mk.get(k) for k in MAKE_EVIDENCE_FIELDS},
                               skill_hashes=file_hashes(made[0]), make_tokens=tokens(mk))
                rows.append(row)
                log.write(json.dumps(row) + "\n")
                log.flush()
                print(json.dumps({k: row[k] for k in ("task", "arm", "rep", "pass", "after_session1", "ran",
                                                      "tampered", "s2_tokens", "hook", "s2_loaded_runbook")}), flush=True)
    print("\n| task | arm | pass | pass after a clean stop | runs with a rerun | runs that missed a step "
          "| session 1 tampered | session 2 loaded the runbook | runs that probed the store | mean turns s1 / s2 | mean fresh tokens s1 / s2 |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
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
                  f"| {sum(r['reruns'] > 0 for r in ran)} | {sum(bool(r['missed']) for r in ran)} | {sum(r['tampered'] for r in ran)} | {sum(r['s2_loaded_runbook'] for r in ran)} | {sum(r['probes'] > 0 for r in ran)} "
                  f"| {mean(turns('s1')):.1f} / {mean(turns('s2')):.1f} "
                  f"| {mean([r['s1_tokens'] for r in ran]):.0f} / {mean([r['s2_tokens'] for r in ran]):.0f} |")


if __name__ == "__main__":
    main()
