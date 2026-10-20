---
name: new-skill
description: Run a multi-step warehouse backfill from `runbook.md` as a long-lived, resumable job. Use when a runbook lists numbered shell steps (1., 2., 3., …) and each step loads a batch whose rows must not duplicate; stop-and-report on failure rather than retry; let a fresh agent session continue a backfill that started days ago without re-running any step. Do not use for short, single-command jobs, for tasks where re-running is safe, or for editing runbooks.
---

# Backfill runbook runner

A backfill runbook is `runbook.md` at the workspace root: numbered steps of the form `1. <shell command>`, executed from the workspace root in order. A backfill can run for days; this skill is **stateful** so any agent session — including one that knows nothing about the last one — can continue it safely.

## What this skill guarantees

- **No step runs twice.** A step is executed at most once across all sessions.
- **A step with an unknown outcome is never silently rerun.** If a step may have committed before reporting failure, the run stops and asks a human.
- **`runbook.md` is never edited.** The skill only reads it.
- **On failure, the run stops and reports.** No retry, no patch, no "make it pass."

## Authoritative state

The source of truth for progress is a single JSON file at the workspace root:

```
.dao/state/backfill.json
```

This lives outside the skill directory so it survives skill edits, reinstalls, and session boundaries. Path and schema are fixed; do not relocate or rename it without a deliberate migration.

Schema (initial values shown):

```json
{
  "version": 1,
  "runbook_path": "runbook.md",
  "runbook_sha256": "<hex sha256 of runbook.md at start>",
  "started_at": "<ISO-8601 UTC>",
  "updated_at": "<ISO-8601 UTC>",
  "steps": {
    "1": "done",
    "2": "pending"
  }
}
```

Step status is one of:

- **absent** — never started.
- **`pending`** — the agent wrote this status *before* running the step, because the load may have committed and then the command failed or the session died. The step's true warehouse state is unknown. **Treat as ambiguous on resume.**
- **`done`** — the command exited 0 and the agent recorded it. Safe to skip on resume.

The file is the only progress ledger. Do not invent a second one (notes, README, chat history). A second source of truth diverges.

## Workflow

On every invocation, perform these steps **in order**:

### 1. Locate and read state

Read `.dao/state/backfill.json` if it exists. If it does not exist, this is a fresh backfill — start a new state file (see step 3). Do not treat absence as progress.

### 2. Reconcile any `pending` step first

Before doing anything else, scan `steps` for any entry with value `"pending"`. If one exists:

1. **Stop the run.** Do not run, skip, or "continue past" that step.
2. Report exactly: which step number is `pending`, when it was marked `pending`, and what the failure or cutoff looked like (look at the most recent terminal output or session transcript you can find — do not invent one).
3. Tell the human: the warehouse offers no way to ask which batches are loaded, so the agent cannot tell whether the load committed. **Rerunning would duplicate rows; skipping risks missing data.** A human must investigate and either mark it `done`, leave it `pending` for retry after manual cleanup, or abort the backfill.
4. Do not proceed to later steps. The backfill is in an unknown state and only a human can unblock it.

If no step is `pending`, resume continues at step 3.

### 3. Verify the runbook is unchanged

If state exists, recompute the SHA-256 of `runbook.md` and compare it to `runbook_sha256`. If they differ, the runbook was edited between sessions. **Stop and report.** A modified runbook can shift step numbers, insert steps, or change commands — none of which is safe to resume blind.

If state does not exist (fresh backfill), compute the SHA-256 of `runbook.md` and record it.

### 4. Parse the steps

Read `runbook.md`. Extract every step whose line matches `^(\d+)\.\s+(.+)$`. Record the step number and the shell command that follows the number-and-dot. Order by step number ascending. If the file is missing, empty, has no numbered steps, has duplicate step numbers, or has any line that looks like a step but is malformed, stop and report the exact issue.

### 5. Resume at the first unfinished step

Find the smallest step number `N` such that `steps[N]` is not `"done"`. If every step is `"done"`, report completion and stop — the backfill is already finished.

If `N` is absent from `steps` (never started), it is about to become `pending`.

### 6. For each unfinished step, run it exactly once

For step `N`:

1. **Mark `pending` and flush state.** Update `.dao/state/backfill.json`: set `steps[N] = "pending"`, refresh `updated_at`. Write the file. Only after the write succeeds may the command run.
2. **Execute the command.** Run the step's shell command from the workspace root using the Bash tool. Capture exit code, stdout, and stderr.
3. **Reconcile the result:**
   - **Exit code 0** → the load is assumed committed. Update the state file: set `steps[N] = "done"`, refresh `updated_at`. Move to step `N + 1`.
   - **Non-zero exit, timeout, tool error, or any other failure** → the load may or may not have committed; the warehouse cannot tell us. **Leave `steps[N]` as `"pending"`** (do not delete the entry, do not mark `done`, do not retry). Refresh `updated_at`. **Stop the run and report** the exact step number, exit code, command, and a short excerpt of stderr. Do not modify anything else.

### 7. Continue until done or stopped

After marking step `N` done, go back to step 5 and find the next unfinished step. The backfill continues step after step until every step is `done`, or a step fails (and is left `pending` while the run stops), or a human intervenes.

There is no internal step limit. Backfills are expected to run for days; an interrupted session is normal, not an error.

## Failure rules (apply at every step)

- **Never retry a failed step.** A retry is forbidden by the same rule that forbids running a step twice.
- **Never edit `runbook.md`.** Not to fix a typo, not to "skip" a step, not to add `--force`. The runbook is the contract; if it is wrong, a human must fix it and the next session will detect the SHA mismatch and stop.
- **Never mark `done` from inference.** Only a `0` exit code from the actual command can mark a step `done`. "It probably worked" is not evidence.
- **Never mark `done` after a failure.** Even if a second command succeeds, the first one's status remains whatever the first run recorded. Fix state in one place: at the moment of the actual exit code.
- **Never run a step that is already `done`.** Skipping is correct; re-running is a duplicate-load bug.
- **Never start at step 1 by default.** Resume from the highest `done` step plus one (or from any `pending`, which stops the run). A fresh backfill starts at step 1 because no state file exists.

## Stopping conditions (final list)

Stop and report — do not continue — when any of these is true:

- The runbook is missing, empty, unparseable, has duplicate step numbers, or its SHA has changed since the previous session.
- Any step is `pending` at the start of a run (resume-blocked; human required).
- A step exits non-zero, times out, or otherwise fails during the run.
- The user explicitly says to stop.

If none of these fire and the last step is marked `done`, report completion.

## Reporting format

When stopping, the report must include:

- The step number that caused the stop (or "all steps done" on completion).
- The exact command that was run, copied from `runbook.md`.
- Exit code and a short stderr excerpt (a few lines; do not dump megabytes).
- The current contents of `.dao/state/backfill.json` (or at least the `steps` object).
- What the human needs to do next.

Do not editorialize, do not propose fixes to `runbook.md`, and do not suggest retrying without first explaining why it is unsafe.

## Tools required

- `Read` — read `runbook.md` and `.dao/state/backfill.json`.
- `Write` — create/update the state file. **Always write the whole file;** do not use partial edits that race with the agent's own updates.
- `Bash` — run step commands from the workspace root and compute SHA-256 (`sha256sum runbook.md` or equivalent).
- No network access is required by this skill. Steps may need whatever their own commands need.

## What this skill deliberately does not do

- It does not parse or understand the contents of step commands. It treats every step as an opaque shell command from the workspace root.
- It does not consult the warehouse to verify a step's effect. The warehouse cannot answer, so the skill does not ask.
- It does not provide a "force re-run" or "skip step" mode. Both are unsafe by construction; if they are ever needed, they belong in a separate human-driven runbook revision.
- It does not migrate, archive, or rotate the state file. One file, one backfill, until a human ends it.
