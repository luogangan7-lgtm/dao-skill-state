---
name: new-skill
description: Run the team's data backfill runbook (runbook.md) from the workspace root, one step at a time, safely across cut-off agent sessions. Use when asked to start, continue, resume, or check the status of a backfill, a batch load, or a warehouse load runbook. Every step is checked against the warehouse before it runs, so a batch that already committed is never loaded twice — even after a crash, a tool timeout, or a connection reset. Stops at the first failure and reports it without retrying or editing the runbook. Not for ordinary shell scripts, and not for runbooks that do not name a command for asking whether a batch is loaded.
---

# Run a backfill runbook

A runbook is `runbook.md` in the workspace. Its steps are numbered shell commands
(`1. <shell command>`) and run **in order, from the workspace root**. Never edit it.

## The one rule this skill exists to enforce

**Loading a batch twice duplicates rows. So never run a step without first asking the
warehouse whether that step's batch is already loaded.** The warehouse is the record of
what took effect — not this session's memory, and not any local file you write.

That rule holds in both directions:

- A step that **succeeded** is skipped on every later pass, even if you have no memory of
  running it.
- A step that **failed after the load actually committed** (connection reset, killed tool
  call, timeout) is skipped too. A scary error message is not evidence either way.

## There is no progress file

Do not keep a "done steps" list, a checkpoint, or a resume index. A local record of what
you think ran is exactly the thing that reloads a batch: it disagrees with the warehouse
the moment one of them is wrong, and you cannot tell which. Progress is re-derived from
the warehouse on every pass, by asking it, for **every** step.

The only file you write is a *check plan* (below): the per-step question to ask. It
records nothing about what has run. Treat it as disposable — regenerate it any time.

## Procedure

### 1. Find and read the runbook

`runbook.md` in the workspace root. If it is not there, stop and ask for it. Read the
whole file. From it, get:

- the numbered steps, and
- **the command the runbook names for asking whether a batch is loaded** — copy it as
  written, and note how it takes a batch identifier.

If the runbook names no such command, stop and report that. Do not invent a check. A
guessed check that wrongly says "loaded" silently skips the entire backfill; one that
wrongly says "not loaded" duplicates every batch.

### 2. Write the check plan

For each step, you need a *check*: a shell command that exits **0** if that step's batch
is already loaded and **1** if it is not. Anything else (≥2) means "cannot tell".

Write `.dao/backfill-plan.json` in the workspace root:

```json
{
  "status_command": "<the command the runbook names, verbatim>",
  "checks": {
    "1": "<command asking about the batch step 1 loads; 0 loaded, 1 not>",
    "2": "<...>"
  }
}
```

The runner supplies the *load commands* itself by reading `runbook.md`, so the plan can
never mis-transcribe or alter them. The plan only supplies questions.

### 3. Dry run

```bash
python3 <skill-dir>/scripts/run_backfill.py --dry-run
```

Read the listing. It prints every step command exactly as the runner will execute it,
next to its check. Confirm the commands match the runbook you read. Fix the plan, not
the runbook. Show the listing to the user before a multi-day backfill starts.

If the runner refuses with `step numbers are not 1..N in order`, the runbook has a
numbered list in its prose that looks like the steps. Read the runbook yourself, find
the line that is the real step 1, and re-run with `--steps-begin-at <that number>`. It
counts numbered lines: the runner still takes the commands verbatim, and the check rule
still covers every step it selects, so this cannot load a batch twice.

### 4. Run

```bash
python3 <skill-dir>/scripts/run_backfill.py
```

Always this one command — to start, to continue, to resume after a cut-off session, and
after any failure. It starts at step 1 every time and checks every step before running
it, so it is correct from a fresh session that knows nothing about the last one. Steps
the warehouse already has are printed as `SKIP`. It stops at the first problem.

If a load takes longer than your tool timeout, run the same command in the background
and read its output when it finishes. A cut-off mid-step is safe: the next pass asks the
warehouse about that batch before deciding.

### 5. Report

State plainly: which steps were skipped as already loaded, which ran, and — if it stopped
— **which step failed and its output, verbatim**. Then stop. Do not diagnose the cause,
propose a fix, or offer to continue.

## When the runner refuses

It refuses when a step's status cannot be determined, or when a step fails. Both end the
run and the turn. Report which step needs checking, and let the user decide.

To resolve a stopped step, **ask the warehouse about that step's batch** (the runner's own
check, or the runbook's status command). If it is loaded, re-run the same runner command
and it will skip that step and continue. If it is not loaded, re-run the same runner
command and it will run the step. The runner never refuses to start or continue — the
same command is always the whole answer.

Do not defeat the check:

- **Do not run a step's command directly**, by hand, outside the runner.
- **Do not mark a step done** by editing the plan, a log, or any local file. The plan
  holds no progress; the warehouse holds progress.
- **Do not read the warehouse's internal tables** to decide whether a batch landed, when
  the runbook offers a status command to ask. Use the check the system offers.
- **Do not treat the failure being over** — the connection coming back, the job looking
  idle — as evidence about whether the load committed. Only the warehouse's answer is.
- **Do not retry a failed step in the same run**, and do not change anything to make it
  pass. Re-running the runner is a new pass, not a retry: it re-asks the warehouse.

## Stopping conditions

| Situation | What to do |
|---|---|
| `runbook.md` absent | Stop, ask for it |
| No status command named in the runbook | Stop, report; never run a step |
| Check exits 0 (loaded) | Skip the step, do not run it |
| Check exits 1 (not loaded) | Run the step |
| Check exits ≥2, or cannot run | Stop. Report the step whose status is unknown. Do not run it. |
| Step command exits non-zero | Stop immediately. Report the step and its output. Do not retry, do not fix, do not edit the runbook. |
| All steps loaded or run successfully | Report the summary and stop |

A step that failed may still have committed its load. That is why the next pass asks
instead of assuming. Leave the resolution to the next pass and the user — the runner is
the only thing that may clear a step.

## Non-goals

- Editing, reordering, or "fixing" `runbook.md`. Ever.
- Retrying, wrapping, or working around a failing step command.
- Choosing a check the runbook does not support, or verifying loads by reading
  warehouse tables by hand.
- Managing anything the runbook does not describe.