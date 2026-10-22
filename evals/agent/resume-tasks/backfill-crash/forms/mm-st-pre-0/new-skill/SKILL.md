---
name: new-skill
description: Execute a numbered data backfill runbook (runbook.md in the workspace root) one step at a time, checking the warehouse before each step so a fresh session can safely resume mid-run without ever loading a batch twice. Use when the user asks to run, resume, or continue a backfill runbook.
---

# Backfill runbook runner

Run a backfill runbook. The runbook is the source of truth for **what** to do; the warehouse is the source of truth for **what has already taken effect**. There is no local progress file — the warehouse answers "is batch N loaded?" for every step, so a fresh session can resume by asking the warehouse for each step in order.

**Form:** fixed steps with a resume rule (per the state contract: a non-idempotent action whose effect the authoritative system can report does not need a local state file). A run is not stateful; it is a normal in-order execution that, before running each step, queries the warehouse for that step's effect.

**Non-negotiables**

- Never edit `runbook.md`. If a step looks wrong, stop and report; do not change anything to make it pass.
- Never run a step twice. Loading a batch twice duplicates rows.
- Never retry a failed step. Stop and report on the first failure.
- Always run steps from the workspace root (where `runbook.md` lives), in numeric order, as written.
- Always check the warehouse for a step before running it — every step, every time, not only the one you suspect failed. A step can succeed in the warehouse and then fail to report back to you (e.g., connection reset after commit); the only safe way to know is to ask the warehouse.

## Workflow

1. **Locate and read the runbook.** The runbook is `runbook.md` at the workspace root. Read it in full. Do not modify it.

2. **Extract the steps.** Parse the numbered steps (`1. ...`, `2. ...`, ...) in order. Each step's command is the shell command to run from the workspace root. Stop and ask if the numbering is ambiguous (e.g., out-of-order, missing, or non-numeric). If a step's command is split across lines, reconstruct it as written; do not reformat.

3. **Find the warehouse check command.** The runbook names the command for asking the warehouse whether a batch is loaded. It may appear as a labeled section, a per-step note, a command template, or a fenced block — read the runbook and find it before assuming a format. The command should take a batch identifier (or step number) and return a clear yes/no on whether that batch is already loaded. If the runbook does not name a check command, stop and report; do not invent one.

4. **For each step, in numeric order:**
   a. Run the warehouse check command for this step. Interpret its output: loaded → skip the step; not loaded → continue to (b); unclear/error → stop and report (do not guess, do not retry).
   b. Run the step's command from the workspace root. Capture its exit status and any error output.
   c. Re-run the warehouse check command for this step to confirm the load actually took effect.
      - Loaded → continue to the next step.
      - Not loaded and the step command reported failure → **stop and report**. Do not retry. Do not modify the command. Do not skip to the next step.
      - Not loaded and the step command appeared to succeed but the warehouse does not show the batch → **stop and report**. The step may have failed silently; do not assume it can be re-run, ask the user.

5. **When all steps are done (or skipped because already loaded), report:**
   - which steps ran in this session,
   - which steps were skipped as already loaded,
   - which steps remain (should be none),
   - any failures encountered (should be none in a clean completion).

## Resume behavior

A new session starts by re-reading `runbook.md` and re-checking the warehouse for every step from step 1. This is what makes resume safe:

- Steps already loaded before this session started are skipped.
- A step whose load committed but whose result never came back is detected by the warehouse check and skipped, not re-run.
- A step that genuinely failed before committing is detected by the warehouse check (still not loaded) AND by the step command's exit status; the run stops there.

There is no progress file to read first, no in-progress flag to set, no half-done state to reconcile. The warehouse is the only ledger.

## Stopping conditions

Stop and report (do not attempt to fix) when any of these occur:

- `runbook.md` is missing, unreadable, or has no parseable numbered steps.
- The runbook does not name a warehouse check command.
- The warehouse check command's output is ambiguous.
- A step's command exits non-zero, exits zero but the warehouse still reports the batch not loaded, or hangs until you decide to kill it.
- The user tells you to stop.

In every stop report, name the workspace path, the step number, the step command as written, the check command you ran, the exit status, and the warehouse's answer.

## What this skill does not do

- It does not edit `runbook.md`.
- It does not run steps in parallel. Steps run strictly in numeric order.
- It does not retry failed steps.
- It does not invent or modify commands.
- It does not write a local progress file. The warehouse is the ledger.