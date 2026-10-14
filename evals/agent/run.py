#!/usr/bin/env python3
"""Agent evaluation harness: run Claude Code headless on frozen tasks, with and without skills,
and check the result with each task's own deterministic checker.

  run.py TASKS_DIR OUT_DIR [--tasks a,b] [--arms none,skills] [--reps 1] [--max-turns 30]

A task is a directory: prompt.md (the user request), optional files/ (copied into the workspace),
optional skills/ (installed as project skills for the "skills" arm), check.py (exit 0 = pass;
reads the workspace path as argv[1]). Each run gets a fresh workspace and a fresh HOME, so no
user settings, memory or skills leak in. The model endpoint comes from the environment
(ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN / ANTHROPIC_MODEL); nothing here names a provider.

Writes OUT_DIR/runs.jsonl (one line per run: task, arm, rep, pass, turns, duration, tokens) and
prints a summary. Runs in CI only (.github/workflows/agent-eval.yml).

Depends on: the standard library and the `claude` CLI on PATH.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time


KEEP_ENV = ("PATH", "LANG", "LC_ALL", "TMPDIR")


def model_env(home):
    """Allowlist: the model endpoint and Claude Code switches, nothing else from the runner."""
    env = {k: v for k, v in os.environ.items()
           if k in KEEP_ENV or k.startswith(("ANTHROPIC_", "CLAUDE_CODE_", "DISABLE_"))}
    env["HOME"] = home
    return env


def redact(text):
    """The agent can read its own token; never let it reach results or artifacts."""
    tok = os.environ.get("ANTHROPIC_AUTH_TOKEN")
    return text.replace(tok, "[REDACTED]") if tok else text


def run_one(task_dir, arm, max_turns, timeout_s):
    ws = tempfile.mkdtemp(prefix="ws-")
    home = tempfile.mkdtemp(prefix="home-")
    if os.path.isdir(os.path.join(task_dir, "files")):
        shutil.copytree(os.path.join(task_dir, "files"), ws, dirs_exist_ok=True)
    if arm == "skills" and os.path.isdir(os.path.join(task_dir, "skills")):
        shutil.copytree(os.path.join(task_dir, "skills"), os.path.join(ws, ".claude", "skills"))
    prompt = open(os.path.join(task_dir, "prompt.md"), encoding="utf-8").read()
    env = model_env(home)
    t0 = time.time()
    try:
        p = subprocess.run(["claude", "-p", prompt, "--output-format", "json", "--max-turns", str(max_turns),
                            "--dangerously-skip-permissions"],
                           cwd=ws, env=env, capture_output=True, text=True, timeout=timeout_s)
        out, rc = p.stdout, p.returncode
    except subprocess.TimeoutExpired:
        out, rc = "", "timeout"
    except OSError as e:  # e.g. claude not installed: a failed run, not a crashed harness
        out, rc = "", f"error: {e.strerror}"
    wall = time.time() - t0
    try:
        res = json.loads(out.strip().splitlines()[-1]) if out.strip() else {}
    except (json.JSONDecodeError, IndexError):
        res = {}
    try:  # the checker reads agent-written files: no model token in its environment
        chk = subprocess.run([sys.executable, os.path.join(task_dir, "check.py"), ws],
                             capture_output=True, text=True, timeout=120,
                             env={k: os.environ[k] for k in KEEP_ENV if k in os.environ})
        passed, note = chk.returncode == 0, (chk.stdout + chk.stderr).strip()[-300:]
    except subprocess.TimeoutExpired:
        passed, note = False, "checker timed out"
    u = res.get("usage") or {}
    return {"pass": passed, "check": redact(note),
            "rc": rc, "is_error": res.get("is_error"), "turns": res.get("num_turns"),
            "duration_s": round(wall, 1), "input_tokens": u.get("input_tokens"),
            "output_tokens": u.get("output_tokens"),
            "cache_read": u.get("cache_read_input_tokens"), "cache_write": u.get("cache_creation_input_tokens")}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("tasks_dir")
    ap.add_argument("out_dir")
    ap.add_argument("--tasks", default="")
    ap.add_argument("--arms", default="none,skills")
    ap.add_argument("--reps", type=int, default=1)
    ap.add_argument("--max-turns", type=int, default=30)
    ap.add_argument("--timeout", type=int, default=900)
    a = ap.parse_args()
    names = [t for t in a.tasks.split(",") if t] or sorted(
        d for d in os.listdir(a.tasks_dir) if os.path.isfile(os.path.join(a.tasks_dir, d, "prompt.md")))
    os.makedirs(a.out_dir, exist_ok=True)
    rows = []
    with open(os.path.join(a.out_dir, "runs.jsonl"), "a") as f:
        for name in names:
            for arm in a.arms.split(","):
                for rep in range(a.reps):
                    r = {"task": name, "arm": arm, "rep": rep,
                         **run_one(os.path.join(a.tasks_dir, name), arm, a.max_turns, a.timeout)}
                    rows.append(r)
                    f.write(json.dumps(r) + "\n")
                    f.flush()
                    print(f"{name} {arm} rep{rep}: {'PASS' if r['pass'] else 'FAIL'} turns={r['turns']} "
                          f"{r['duration_s']}s in={r['input_tokens']} out={r['output_tokens']} rc={r['rc']}",
                          flush=True)
    print("\n| task | arm | pass | runs |\n|---|---|---|---|")
    for name in names:
        for arm in a.arms.split(","):
            rs = [r for r in rows if r["task"] == name and r["arm"] == arm]
            print(f"| {name} | {arm} | {sum(r['pass'] for r in rs)} | {len(rs)} |")


if __name__ == "__main__":
    main()
