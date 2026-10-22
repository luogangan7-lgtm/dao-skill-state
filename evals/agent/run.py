#!/usr/bin/env python3
"""Agent evaluation harness: run Claude Code headless on frozen tasks, with and without skills,
and check the result with each task's own deterministic checker.

  run.py TASKS_DIR OUT_DIR [--tasks a,b] [--arms none,skills] [--reps 1] [--max-turns 30]

Arms: none (no skills), skills (installed as project skills; the host decides whether to use them),
hook (installed, plus the real skill-hub hook registered as a project UserPromptSubmit hook over
them; its log records what it injected).
A task is a directory: prompt.md (the user request), optional files/ (copied into the workspace),
optional setup.py (run in the workspace before the agent), optional skills/ and/or skills.json
([{repo, path}] resolved under $SKILL_SOURCES/<owner>_<repo>, third-party skills fetched by the
workflow at pinned commits, never committed) installed as project skills for the "skills" arm,
check.py (exit 0 = pass; reads the workspace path as argv[1]). Each run gets a fresh workspace and a fresh HOME, so no
user settings, memory or skills leak in. The model endpoint comes from the environment
(ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN / ANTHROPIC_MODEL); nothing here names a provider.

Writes OUT_DIR/runs.jsonl (one line per run: task, arm, rep, pass, turns, duration, tokens) and
prints a summary. Runs in CI only (.github/workflows/agent-eval.yml).

Depends on: the standard library and the `claude` CLI on PATH.
"""
import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time


KEEP_ENV = ("PATH", "LANG", "LC_ALL", "TMPDIR", "NODE_PATH")  # NODE_PATH: global npm libraries


def model_env(home):
    """Allowlist: the model endpoint and Claude Code switches, nothing else from the runner."""
    env = {k: v for k, v in os.environ.items()
           if k in KEEP_ENV or k.startswith(("ANTHROPIC_", "CLAUDE_CODE_", "DISABLE_"))}
    env["HOME"] = home
    return env


def redact(text):
    """The agent can read the model settings it is given (token, endpoint, any other key passed
    through model_env); never let their values reach results or artifacts."""
    for k, v in os.environ.items():
        if (k.startswith(("ANTHROPIC_", "CLAUDE_CODE_")) or k == "WAREHOUSE_AUDIT") and len(v) >= 8:
            text = text.replace(v, "[REDACTED]")
    return text


def install(task_dir, arm, ws):
    """Skills for the skills arm (task skills/ and pinned third-party refs), then setup.py."""
    if arm in ("skills", "hook"):
        dest = os.path.join(ws, ".claude", "skills")
        if os.path.isdir(os.path.join(task_dir, "skills")):
            shutil.copytree(os.path.join(task_dir, "skills"), dest, dirs_exist_ok=True)
        sj = os.path.join(task_dir, "skills.json")
        if os.path.exists(sj):
            root = os.path.realpath(os.environ["SKILL_SOURCES"])
            for ref in json.load(open(sj)):
                src = os.path.realpath(os.path.join(root, ref["repo"].replace("/", "_"), ref["path"]))
                if not src.startswith(root + os.sep):
                    raise ValueError("skill path escapes the sources")
                shutil.copytree(src, os.path.join(dest, os.path.basename(src)), dirs_exist_ok=True)
    if arm == "hook":
        # the real skill-hub hook (built by the workflow), indexed over this workspace's skills and
        # registered as a project UserPromptSubmit hook, exactly as a user installs it
        os.makedirs(os.path.join(ws, ".claude", "skills"), exist_ok=True)
        hub = os.environ["SKILL_HUB_BIN"]
        reg = os.path.join(ws, ".claude", "skill-hub-registry.json")
        subprocess.run([hub, "index", "--root", f"task={os.path.join(ws, '.claude', 'skills')}", "--out", reg],
                       check=True, capture_output=True, timeout=60)
        log = os.path.join(ws, ".claude", "skill-hub-hook.log")
        cmd = f"env SKILL_HUB_HOOK_LOG={shlex.quote(log)} {shlex.quote(hub)} hook --registry {shlex.quote(reg)}"
        json.dump({"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": cmd, "timeout": 10}]}]}},
                  open(os.path.join(ws, ".claude", "settings.json"), "w"))
    if os.path.exists(os.path.join(task_dir, "setup.py")):
        r = subprocess.run([sys.executable, os.path.join(task_dir, "setup.py")], cwd=ws, timeout=120,
                           capture_output=True, text=True,
                           env={**{k: os.environ[k] for k in KEEP_ENV if k in os.environ}, "HOME": ws})
        if r.returncode != 0:
            raise ValueError("setup.py: " + (r.stderr or r.stdout).strip()[-300:])


QUOTA_STREAK = [0]  # consecutive runs the host ended at once with an API error


def guard_quota(r):
    """Three runs in a row that end within seconds on an API error mean the model endpoint is
    refusing (quota window, rate limit): stop instead of recording them as task failures."""
    # only runs where the host actually answered (rc an int): setup failures are not the endpoint
    instant = (isinstance(r.get("rc"), int) and r.get("is_error") and (r.get("turns") or 0) <= 1
               and (r.get("duration_s") or 0) < 10)
    QUOTA_STREAK[0] = QUOTA_STREAK[0] + 1 if instant else 0
    if QUOTA_STREAK[0] >= 3:
        print(f"model endpoint refusing (3 instant API errors; last: {r.get('error')}); stopping", flush=True)
        sys.exit(3)
    return r


def run_one(task_dir, arm, max_turns, timeout_s):
    return guard_quota(_run_one(task_dir, arm, max_turns, timeout_s))


def _run_one(task_dir, arm, max_turns, timeout_s):
    ws = tempfile.mkdtemp(prefix="ws-")
    home = tempfile.mkdtemp(prefix="home-")
    if os.path.isdir(os.path.join(task_dir, "files")):
        shutil.copytree(os.path.join(task_dir, "files"), ws, dirs_exist_ok=True)
    try:
        install(task_dir, arm, ws)
    except (OSError, KeyError, ValueError, subprocess.SubprocessError) as e:
        return {"pass": False, "check": redact(f"setup failed: {type(e).__name__}: {e}"[-400:]), "rc": "setup",
                "is_error": True, "skill_calls": [], "tools": {}, "ws": ws, "hook": None,
                "turns": None, "duration_s": 0, "input_tokens": None, "output_tokens": None,
                "cache_read": None, "cache_write": None}
    prompt = open(os.path.join(task_dir, "prompt.md"), encoding="utf-8").read()
    env = model_env(home)
    t0 = time.time()
    try:
        # stream-json: every assistant message, so we can see which tools and skills were used
        p = subprocess.run(["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
                            "--max-turns", str(max_turns), "--dangerously-skip-permissions"],
                           cwd=ws, env=env, capture_output=True, text=True, timeout=timeout_s)
        out, rc = p.stdout, p.returncode
    except subprocess.TimeoutExpired as e:  # keep the partial trace: timeouts are the runs worth reading
        raw = e.stdout or b""
        out, rc = (raw.decode("utf-8", "replace") if isinstance(raw, bytes) else raw), "timeout"
    except OSError as e:  # e.g. claude not installed: a failed run, not a crashed harness
        out, rc = "", f"error: {e.strerror}"
    wall = time.time() - t0
    res, tools, skill_calls, bash = {}, {}, [], []
    cmd_of, bash_errors = {}, []  # tool_use id -> command; failed Bash calls and what they said
    for line in out.splitlines():
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        if ev.get("type") == "result":
            res = ev
        elif ev.get("type") == "user":
            for item in (ev.get("message") or {}).get("content") or []:
                if (isinstance(item, dict) and item.get("type") == "tool_result" and item.get("is_error")
                        and item.get("tool_use_id") in cmd_of and len(bash_errors) < 20):
                    out_text = item.get("content")
                    out_text = out_text if isinstance(out_text, str) else json.dumps(out_text)
                    bash_errors.append(redact(cmd_of[item["tool_use_id"]])[:80] + " -> " + redact(out_text)[:200])
        elif ev.get("type") == "assistant":
            for item in (ev.get("message") or {}).get("content") or []:
                if isinstance(item, dict) and item.get("type") == "tool_use":
                    name = item.get("name", "?")
                    tools[name] = tools.get(name, 0) + 1
                    if name == "Bash":
                        inp = item.get("input") or {}
                        cmd_of[item.get("id")] = ("[unsandboxed] " if inp.get("dangerouslyDisableSandbox") else "") + str(inp.get("command", ""))
                    if name == "Bash" and len(bash) < 40:  # what the extra turns were spent on
                        bash.append(redact(cmd_of[item.get("id")])[:120])
                    if name == "Skill":
                        skill_calls.append(str((item.get("input") or {}).get("skill") or (item.get("input") or {}).get("command") or "?"))
                    elif name == "Read" and "/.claude/skills/" in str((item.get("input") or {}).get("file_path", "")):
                        skill_calls.append("read:" + str(item["input"]["file_path"]).split("/.claude/skills/")[1])  # which file
    try:  # the checker reads agent-written files: no model token in its environment
        chk = subprocess.run([sys.executable, os.path.join(task_dir, "check.py"), ws],
                             capture_output=True, text=True, timeout=120,
                             env={k: os.environ[k] for k in KEEP_ENV + ("WAREHOUSE_AUDIT",) if k in os.environ})
        passed, note = chk.returncode == 0, (chk.stdout + chk.stderr).strip()[-300:]
    except subprocess.TimeoutExpired:
        passed, note = False, "checker timed out"
    hook_log = os.path.join(ws, ".claude", "skill-hub-hook.log")
    hook = None
    if arm == "hook":
        lines = open(hook_log).read().splitlines() if os.path.exists(hook_log) else []
        hook = [{"via": e.get("via"), "picked": e.get("picked")} for e in map(json.loads, lines)] or "did not run"
    u = res.get("usage") or {}
    return {"pass": passed, "check": redact(note), "hook": hook, "ws": ws,
            "rc": rc, "is_error": res.get("is_error"), "turns": res.get("num_turns"),
            "skill_calls": skill_calls, "tools": tools, "bash": bash, "bash_errors": bash_errors,
            # on an API error (quota window, rate limit) keep what the host said, redacted
            # the host's last message: why a run stopped (asked a question, wrote elsewhere, gave up)
            "final": redact(str(res.get("result") or ""))[-600:],
            # what the agent left in the workspace (paths only), to see where outputs went
            "files": sorted(os.path.relpath(os.path.join(d, f), ws) for d, _, fs in os.walk(ws)
                            if "/.claude" not in d + "/" and "/node_modules" not in d for f in fs)[:60],
            "error": (redact(str(res.get("result") or ""))[:300] or f"rc={rc}") if res.get("is_error")
            else (f"no result (rc={rc})" if not res else None),
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
    a.tasks_dir = os.path.abspath(a.tasks_dir)  # setup.py runs with the workspace as cwd
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
                    print(f"{name} {arm} rep{rep}: {'PASS' if r['pass'] else 'FAIL'} skill={','.join(r['skill_calls']) or '-'} turns={r['turns']} "
                          f"{r['duration_s']}s in={r['input_tokens']} out={r['output_tokens']} rc={r['rc']}",
                          flush=True)
    print("\n| task | arm | pass | runs | runs that used a skill | mean turns |\n|---|---|---|---|---|---|")
    for name in names:
        for arm in a.arms.split(","):
            rs = [r for r in rows if r["task"] == name and r["arm"] == arm]
            turns = [r["turns"] for r in rs if r["turns"] is not None]
            print(f"| {name} | {arm} | {sum(r['pass'] for r in rs)} | {len(rs)} | "
                  f"{sum(1 for r in rs if r['skill_calls'])} | {sum(turns) / len(turns):.1f} |" if turns else
                  f"| {name} | {arm} | {sum(r['pass'] for r in rs)} | {len(rs)} | - | - |")


if __name__ == "__main__":
    main()
