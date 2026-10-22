---
name: new-skill
description: Execute a long-running data warehouse backfill defined by runbook.md in the workspace, with crash-safe resumption across cut-off agent sessions and at-most-once execution of each batch-loading step.
---

# Backfill Runbook Runner

Execute every step of a backfill runbook, in order, safely across agent session boundaries. The **warehouse** — not this skill, not the agent, not any local progress file — is the source of truth for whether a step's batch has already loaded.

## When to use

Use this skill when **all** of the following hold:

- The workspace contains a `runbook.md` whose body is a numbered list of shell commands, each of which loads one batch into a data warehouse.
- The run may span many hours or days, and the agent session can be cut off at any moment.
- Loading a batch twice would duplicate rows, so each step must run at most once across the entire run (and across all sessions).
- The runbook names a command for asking the warehouse whether a given batch is already loaded.

Do not use this skill when the run is short, when steps are idempotent, or when there is no way to ask the warehouse whether a step has already loaded. In those cases, just run the commands.

## Inputs

- `runbook.md` in the workspace root — the runbook to execute. Treated as read-only.

## Procedure

### 1. Read the runbook

Open `runbook.md`. From it, identify:

- **The loaded-check command and how to interpret it.** The runbook names a command (and how to read its result) for asking the warehouse whether a batch is already loaded. It may appear as a comment, a code block, prose, a header, or anything else the runbook author chose. There is no fixed format — read the runbook to find it. Stop and ask the user only when the runbook truly has nothing you can use as a loaded check; do not invent one.

- **The numbered steps.** List items whose line begins with `N.` where `N` is a positive integer. The remainder of that line is the shell command for that step. Numbering starts at 1 and proceeds in order; steps must be taken in that order.

If the runbook has no numbered list, stop and ask the user. Do not try to guess step boundaries.

### 2. Pre-check every step before doing anything else

Before running any step, run the loaded-check for **every** step from 1 upward. Classify each:

- **Loaded** — this step's batch is already in the warehouse.
- **Not loaded** — this step's batch is not yet in the warehouse.
- **Ambiguous** — the check command failed, returned nothing you can map to yes/no, or its meaning is unclear.

For each step, record its classification. Steps marked **Loaded** are already done. The first **Not loaded** step is where work begins. An **Ambiguous** check anywhere stops the run — never guess on idempotency.

This pre-check is what makes the skill safe across sessions. On a resume, the agent has no memory of the previous session; it derives progress from the warehouse. Do not skip a pre-check just because the agent thinks it knows where it left off.

### 3. Run the not-loaded steps, in order

Starting from the first **Not loaded** step, for each step `N` with command `C`:

1. Run the pre-check for `N` again, immediately before running `C`. The warehouse state can change between the initial sweep and now; do not rely on the initial classification.
2. If `N` is now **Loaded**, skip — another run may have completed it — and move to step `N+1`.
3. If `N` is **Not loaded**, run `C` from the workspace root. Capture stdout and stderr.
4. If `N` is **Ambiguous**, stop and report. Do not run `C`.
5. After `C` finishes (regardless of its exit code), run the loaded-check for `N` again — the post-check.
   - **Loaded** → continue with `N+1`.
   - **Not loaded** and `C` exited 0 → silent failure. Stop and report.
   - `C` exited non-zero → stop and report. Do not retry `C`. Do not modify `C` to make it pass.
   - **Ambiguous** → stop and report.

### 4. Stop conditions

Stop the run and produce a report the moment any of these occur:

- A step's command exited non-zero.
- A pre-check or post-check returned something you cannot map to a clear *Loaded* or *Not loaded*.
- A step's command exited 0 but the post-check still says *Not loaded*.
- The runbook has no numbered list.
- The runbook names no loaded-check command, or names one you cannot interpret.

When you stop:

- Do not run any further step.
- Do not retry the failing step.
- Do not modify the runbook, the step command, or the loaded-check command.
- Do not skip ahead to a later step.

The report must contain:

- The runbook path and total step count.
- The number of the step that stopped the run, its full command, and its complete stdout/stderr.
- The output of every loaded-check run for that step (pre-check and post-check).
- Whether the failing step's batch is currently *Loaded* or *Not loaded* in the warehouse, if the post-check could determine it.
- The next step number that would have run, if any.
- A short note on which stop condition fired.

## What this skill guarantees

- Every step runs at most once across the entire run, including across sessions.
- A step whose load committed — even if the command later errored or the session was cut off — is not re-run; the post-check confirms it took effect.
- A step whose load did not commit is re-run by the next session, until the warehouse reports it as *Loaded*.
- No step is run out of order.
- `runbook.md` is read but never written. No other artifact is required for resume.