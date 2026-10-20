---
name: new-skill
description: Execute a team's numbered-step data backfill runbook (runbook.md at workspace root) exactly once per batch across sessions that may be cut off. Each numbered step loads a batch into the warehouse; rerunning a loaded step duplicates rows. Before every step the skill asks the warehouse whether the batch is already loaded (the runbook names the command) and only runs the step when it is not. A step that exits nonzero — even after the load actually committed — stops the run; the skill never retries, never edits the runbook, never alters a step to make it pass. Use when the user asks to run a backfill, continue a backfill, or check what's left of one. Do NOT use to create, edit, or validate runbook.md itself.
---

# Run a backfill runbook exactly once, across session cuts

## Form

Fixed steps plus a resume check rule. The warehouse is the authoritative source of what is loaded; a fresh session needs no progress file because it re-asks the warehouse for every step before deciding to run it. A local audit log is written so an operator can see history; the log is **not** the source of truth.

## Trigger

- User asks to run the runbook, continue it, kick it off, or "do the next step".
- User asks what is left, how far we got, or which step is current. The skill answers from the warehouse, not from memory.

## Non-trigger

- Creating, editing, reordering, or validating `runbook.md`. The runbook is not this skill's input to mutate. Refuse and point at whoever owns the runbook.

## Required input

- `runbook.md` at the workspace root, containing:
  - numbered steps of the form `1. <shell command>` (the user-specified shape),
  - the named warehouse-check command (and how to invoke it per step — the user says "each runbook names the command for that"; read runbook.md to find it, do not invent a format).

If `runbook.md` is missing, unreadable, or does not name a check command, stop and report what is missing. Do not guess.

## Decision workflow

1. **Establish ground truth once, at the start.**
   - Read `runbook.md`.
   - Extract, from runbook.md as written: the ordered numbered steps, the per-step batch identifier (the token that names the batch in the warehouse — typically a date, ID, or argument to the step's command; if the runbook does not make this clear, stop and report), and the check command that, given a batch identifier, tells you whether that batch is already loaded.
   - Do not invent identifiers, do not infer identifiers by reformatting dates, do not "normalize" the runbook.

2. **Write an audit log** at `.backfill-run.log` in the workspace root (append on each step). The log is for humans; it is allowed to lag reality.

3. **For each numbered step, in order, do exactly this loop:**

   a. **Ask the warehouse first.** Invoke the runbook's named check command with this step's batch identifier.
      - If it reports the batch is loaded: log "step N already loaded, skipping", continue to step N+1. Do not run the step.
      - If it reports the batch is not loaded: continue to (b).
      - If the check command itself fails or its answer is ambiguous: stop and report. Do not run the step. Rerunning a step whose loaded-state is unknown would duplicate.

   b. **Run the step's shell command exactly as written, from the workspace root.** The runbook is the source of truth for the command — do not edit it, do not add flags, do not redirect output to mask a failure.

   c. **If the command exits 0:** log success, continue to N+1.
      **If the command exits non-zero:** stop. Do **not** retry. Do **not** modify the command. Do **not** skip ahead. Do **not** mark it done by assumption. The step's true effect is unknown to you; the next session will check the warehouse and find out.

4. **Stop when:**
   - every step is loaded (success),
   - a step exits non-zero (failure — report and wait for instruction),
   - the check command cannot answer for some step (blocked — report and wait),
   - the user stops you.

## What the resume rule looks like in practice

A session that knows nothing about the last one lands here, reads `runbook.md`, and runs the same loop from step 1. Because step 3a runs on **every** step, not only the one that failed, the loop is safe to start over:

- A step that committed but whose process then died (e.g., connection reset after the warehouse commit) → check returns "loaded" → skipped → no duplicate.
- A step that did not commit and was never attempted → check returns "not loaded" → runs once.
- A step that has never been reached → never seen → no effect.

The cumulative effect across sessions is **at most one run per step**, even if sessions are cut off, even if the user restarts the agent a dozen times. This is the entire safety property; do not work around it.

## Stopping conditions (explicit)

- **All steps loaded** → report all green and exit.
- **Step exited non-zero** → report the step number, the exit code, the tail of stderr/stdout, the warehouse-check result for that step, and that nothing was retried or edited. Wait. Do not decide for the user.
- **Check command failed or returned an ambiguous answer** → report the step number and the failure. Do not run the step. Wait.
- **User says stop** → stop at the next safe boundary (between steps is safe; mid-step is not).

## What the skill must not do

- Edit, reorder, or "fix" `runbook.md`.
- Retry a failed step.
- Modify a step's command (adding `|| true`, swapping flags, hiding output) to make it exit 0.
- Skip a step whose loaded-state is unknown.
- Run steps out of order.
- Persist its own progress file as the source of truth. The warehouse is the source of truth.

## Pass / fail check

- **PASS** when every numbered batch is loaded exactly once, the audit log shows one executed attempt per batch that was not already loaded, and no batch appears twice in the warehouse.
- **FAIL** if any batch was loaded twice, if a failed step was retried or covered up, if `runbook.md` was edited, or if a step was skipped while its loaded-state was unknown.
