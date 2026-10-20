---
name: new-skill
description: Execute a team's numbered-step data backfill runbook across agent sessions without ever loading the same warehouse batch twice. Use when the user invokes the skill with a runbook.md at the workspace root and asks to run, continue, or resume a backfill; do not use to edit a runbook, dry-run steps, run an unrelated shell command, or fix a failing step.
---

# Run a Data Backfill Runbook

## Inputs

- `runbook.md` at the workspace root. Read it; **never** edit it. If the file is missing, stop and report — do not assume a different layout.

## When to invoke

- The user asks to run, continue, or resume a backfill runbook from `runbook.md`.
- A previous agent session ran part of a backfill and was cut off, and the user wants the backfill finished.

## Do not invoke when

- The user wants to **edit** the runbook — refuse and route them to the runbook owner. Editing a runbook risks changing batch scope; out of scope for this skill.
- The user wants a **dry run** or a one-off shell command — a dry run is not what this skill is for and may produce misleading reassurance.
- There is no `runbook.md` in the workspace root.

## Workflow

1. **Open the runbook.** Read `runbook.md` from the workspace root. Do not write to it.
2. **Find the loaded-batch check.** The runbook names a command for asking the warehouse whether a given batch is already loaded (for example under a heading like "Check whether a batch is loaded", "Verify a load", or as a documented `check <batch-id>` example). If the runbook does not name one, **stop and report** — do not invent a check. The only safe source of truth for "this batch is in the warehouse" is the warehouse's own answer to that specific command.
3. **Find how each step's batch is named.** The runbook also tells you how to map a numbered step to the identifier the loaded-batch check expects. That identifier may live in the step's arguments (`--batch=…`), in a label next to the step, or in a documented convention. Read that section. If the mapping is unclear for any step, stop and report before running anything.
4. **Enumerate the steps.** A step is a numbered list item (`1.`, `2.`, …) whose payload is a shell command. Steps run in numeric order, from the workspace root.
5. **Pre-flight every step in the runbook** before running anything. For each step, derive its batch identifier and invoke the loaded-batch check the runbook named. Collect a `done-already` set of steps whose batch the warehouse reports as already loaded.
   - This is the resume rule. A fresh session that knows nothing about the previous one must rely on the warehouse, not on a local progress file, because a step can fail **after** its load committed (for example a connection reset right after commit but before the process exited cleanly). That kind of failure does not always leave a local trail. Checking **every** step, not only the one assumed unfinished, is what keeps a partial run from reloading a batch.
6. **Run the remaining steps in numeric order.** For each step not in `done-already`:
   - Run that exact step's shell command from the workspace root. Do not edit, parameterize, "fix", or augment it.
   - **On clean success** (exit code 0, normal output): record `step N: done` and continue.
   - **On any failure** — non-zero exit, hung process, connection reset, partial or garbled output, unexpected prompt, anything other than a clean success — **stop immediately**.
     - Do not retry the failed step.
     - Do not edit the step's command, its arguments, its environment, or its inputs to make it pass.
     - Do not run any later step.
     - Do not re-run any earlier step a second time.
     - Report the failed step number, the runbook line you ran, the failure surface (exit code, last useful output line, error message), and that no retry or fix was attempted.
7. **Finish.** When the last numbered step completes cleanly, report the runbook path, every step in numeric order, which ones were skipped because the warehouse already had their batch, and which ones were run this session.

## Output

A plain-text report naming:
- the runbook path and the total step count,
- for each step: `skipped (already loaded)`, `done this session`, or `failed` (with the failure surface and the no-retry-no-fix note),
- the final state: complete, or stopped at step N with the reason.

## Pass / fail check

- **Pass** only if every numbered step in the runbook is either `done this session` or `skipped (already loaded)` after the warehouse pre-flight. A backfill completes when the warehouse already shows every batch loaded, even if this session ran zero steps.
- **Fail** as soon as one step does not return a clean success. The skill stops at that step; the next session resumes by running this workflow again.

## Hard boundaries

- **Never** edit `runbook.md`.
- **Never** retry a failed step in the same session.
- **Never** modify a step's command, arguments, environment, or inputs to make it pass.
- **Never** run any step a second time — not this session, not a future session. The warehouse's loaded-batch check is the single gate against duplicates.
- **Never** invent a loaded-batch check the runbook did not name.
- **Never** continue past a failed step.
