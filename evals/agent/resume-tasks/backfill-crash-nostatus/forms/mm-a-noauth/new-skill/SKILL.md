---
name: new-skill
description: Run a numbered-step runbook (runbook.md) at the workspace root for a multi-session data backfill. Every step loads a batch into the warehouse; reruns duplicate rows. Tracks per-step status in an on-disk state file so a fresh session resumes cleanly and never re-executes a completed step. On any step failure, interruption, or uncertain outcome, stops and reports — never retries and never edits the runbook.
---

> **Note:** the directory `new-skill/` was specified by the requester, so the skill `name` is set to match. The directory is descriptive of placement only; pick a clearer name if this skill moves.
---

# Runbook backfill driver

## Form: stateful (not fixed steps)

A backfill runs across sessions and can be cut off mid-step. Steps are non-idempotent (loading a batch twice duplicates warehouse rows) and the warehouse has no API to ask which batches were already loaded.

The skill therefore:
- records state **outside this skill directory** (inside the workspace being run),
- resumes by reading that state file first,
- never re-runs an action whose outcome it cannot verify — it stops and reports.

## Trigger

Use this skill when:
- a `runbook.md` exists at the workspace root, and
- the user asks to start, continue, or check on the backfill.

Do not use when `runbook.md` is missing — stop and confirm the path with the user.

## Required input

- `<workspace>/runbook.md`: one step per line, format `N. <shell command>` (a number, a dot, a single space, then the rest of the line is the command to run). Continuation lines without a number prefix are not part of any step.
- `<workspace>` itself is the working directory. Every command runs from there.
- `runbook.md` is **read-only**. The skill must never edit, append to, or rewrite it — not to "fix" a failing step, not to skip one, not to add notes.

## State file

Path: `<workspace>/.backfill/state.json`

Schema:
```json
{
  "runbook": "runbook.md",
  "started_at": "<iso8601 UTC>",
  "updated_at": "<iso8601 UTC>",
  "next_step": 1,
  "completed": false,
  "steps": [
    {
      "n": 1,
      "command": "<exact text after the leading `N. `>",
      "status": "untouched | pending | done | failed",
      "started_at": "<iso8601 UTC or null>",
      "finished_at": "<iso8601 UTC or null>",
      "exit_code": null,
      "note": "<short optional note>"
    }
  ]
}
```

Status meanings:
- `untouched` — never started in this recorded run.
- `pending` — `started_at` is set; the command was launched but no terminal outcome was recorded.
- `done` — exit code 0, confirmed.
- `failed` — non-zero exit, signal, or any other terminal failure. Per the runbook policy, a failed step is **never** retried by this skill.

Atomic writes only: write to `.backfill/state.json.tmp`, then rename over `state.json`. Save the file after every status change so a cut-off never leaves the on-disk truth behind the in-memory truth.

## Resume protocol (read this first, every session)

1. Open `<workspace>/.backfill/state.json`.
2. If it is missing → this is a fresh run. Initialize it from `runbook.md` and start at step 1.
3. If `completed == true` → report that the backfill is already complete and stop. Do not re-run anything.
4. Walk the steps in numeric order. The first step that is not `done` is the resume point. Specifically:
   - `done` → skip without re-executing. (`done` steps must never run a second time.)
   - `pending` → **stop and report**. The previous session launched this step but did not record an outcome; the warehouse cannot answer whether the batch loaded. Do not retry.
   - `failed` → **stop and report**. Do not retry, do not modify the command, do not skip ahead.
   - `untouched` → this is the next step to run.

## Step lifecycle

For the step the resume protocol picks:

1. **Start.** Set `status = "pending"`, `started_at = now`, `exit_code = null`, save.
2. **Execute.** Run the exact command (no substitution, no injected env) from `<workspace>` using `bash -c "<command>"` or the host equivalent. Capture exit code and the last few lines of output.
3. **Reconcile.**
   - `exit_code == 0` → set `status = "done"`, `exit_code = 0`, `finished_at = now`, save. Advance `next_step` and continue.
   - Any other exit, signal, timeout, or **the session ends while `status == pending`** → set `status = "failed"`, `finished_at = now`, record exit code (or the literal note `interrupted`), save, **stop and report**. Never retry, never "fix", never skip.
4. **End of runbook.** When every step is `done`, set `top.completed = true`, save, and report success with the per-step timing.

## What the failure report must say

When the run stops, the report includes, in plain prose:
- the step number that stopped the run,
- the exact command that was attempted,
- the exit code (or the literal word `interrupted` if the session ended mid-step),
- the last few lines of captured output,
- the list of step numbers confirmed `done` so far,
- the path to the state file,
- the recommendation that the user manually check the warehouse for that batch and decide whether to skip or dedupe.

The skill never proposes retries, retries-with-flags, or edits to the runbook. Those decisions belong to the user.

## Non-goals

- Editing `runbook.md`.
- Retrying or "fixing" failing steps.
- Querying the warehouse to learn which batches loaded (it cannot answer).
- Reformatting, summarizing, or reordering the runbook.
- Prompts during a step — a step runs to completion (success or failure) without asking the user mid-run.
