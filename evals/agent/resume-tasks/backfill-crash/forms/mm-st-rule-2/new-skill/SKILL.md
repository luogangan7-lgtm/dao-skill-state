---
name: new-skill
description: Run a data backfill runbook (runbook.md) one numbered step at a time from the workspace root, resumable across sessions and never loading a batch twice. Use when asked to run, start, continue, resume, restart, re-check, or report on a backfill runbook or a step of one, or to say how far a backfill has got. Position is recovered by asking the warehouse which batches are loaded, never from memory. Do not use to write, edit, reorder, or repair a runbook, or for work that is not a batch load.
---

# Run a data backfill runbook

Runs `runbook.md` in numbered order, one step at a time, so that a backfill that takes days
survives session cutoffs and never loads the same batch twice.

## Use this when

- The user asks to run, start, continue, resume, restart, re-check, or report on a backfill
  runbook, or on a step of one.
- `runbook.md` exists at the workspace root.

## Do not use this when

- The user wants a runbook written, edited, reordered, or repaired. The runbook is read-only here.
- The work is not a batch load into the warehouse — there would be nothing to reconcile against.

## Read before anything else

1. `runbook.md` at the workspace root, in full. Its numbered steps are the entire plan. Run them
   in order, one at a time, from the workspace root. Never edit the file, never reorder or
   renumber steps, never add, skip, or "fix" a command, and never run a command the runbook does
   not contain.
2. The command the runbook gives for asking the warehouse whether a batch is loaded. Find it by
   reading the runbook — the wording is whatever that particular runbook uses. If the runbook
   names no such command, stop and say so; do not go looking for another way into the warehouse.

You need a shell and the workspace's own tools: both the status command and the step commands are
shell commands, and both are run from the workspace root.

## The rule everything else serves

**No step may run twice. Loading a batch twice duplicates rows.**

The warehouse is the only record of what has happened. Your memory of the last step, a summary an
earlier session left, and your own sense of how far the run got are not records: a session can be
cut off at any moment, and a step can fail *after* its load committed — a dropped connection, a
timeout, a killed process. Never infer a step's status. Ask the warehouse, every session, for
every step.

## Find out where the run is (do this first, every session)

For each step in order, identify which batch it loads — take it from the runbook, either from the
batch id in the step's own command or from the runbook text that names it. Then run the runbook's
status command for that batch.

- **Loaded** → this step is done. Move to the next step. Do not run it.
- **Not loaded** → this is the next step to run. Stop scanning. Steps after it have not run, and
  must not run before it.
- **Cannot check** → the batch is not identifiable from the runbook, the status command fails, or
  its output does not clearly say loaded or not loaded. Do not run this step and do not run past
  it. Stop and report which step needs a check, and why you could not make it.

Scanning from the top every session is the point, not a formality. A step an earlier session
finished can look unfinished to you, and re-running it duplicates rows. Do not skip straight to
the step you remember failing.

## Run one step

1. Run the status command for that step's batch again, immediately before the load.
2. If it says **loaded** → the step is already done. Go back to "Find out where the run is" and
   continue scanning from the next step. Nothing was loaded.
3. If it says **not loaded** → run the step's command exactly as written, from the workspace root.
   Wait for it to finish. One step at a time; never overlap steps.
4. Exit status 0 → the step succeeded. Go to the next step.
5. Any other exit status, or a crash → go to "A step failed". Do not assume the load did not
   happen.

## A step failed

A failed exit status says the command failed. It does not say the load did not commit — a
connection reset during a load can arrive after the rows are in. So run the runbook's status
command for that step's batch and read the result:

- **Loaded** → the load committed before the failure. That step is finished for good: it must
  never run again, by you or by anyone continuing this run. Do not rerun it, do not rerun the
  runbook from the top, do not treat it as safe to retry.
- **Not loaded** → the load did not commit. Still do not rerun it. Steps are not retryable from
  here.
- **Cannot check** → the step's outcome is unknown, which is the worst case: the only safe move
  for a step that duplicates on repetition is not to run it.

Then **stop and report**, in all three cases. Never run the steps after the failed one. Never edit
the runbook, change the command, add a flag, or otherwise do anything to make the step pass.

Report, in this order:

- the step number and its command;
- the exit status, plus any error output you can quote;
- what the status command says about that batch, and the exact wording you based that on;
- which batches are now known to be loaded, and that you have run nothing else;
- if the outcome could not be checked, say plainly that it is unknown, and that this step must be
  checked in the warehouse before anything else happens to it.

Do not describe the run as finished. Do not propose a way to make the step pass.

## The user says continue

When the user tells you to continue after a stopped run, their word is not a status update. Check
the failed step against the warehouse before doing anything:

- **Loaded** → it did commit, and the failure came only after the load. Record it as done and
  continue with the next step, following the procedure above from there.
- **Not loaded** → tell the user the batch is confirmed not loaded, and that running it again
  would be the retry this procedure does not perform. The decision, and the command, stay with
  them: if they want it run, they run it, and you re-check the batch before continuing.
- **Cannot check** → stop again and report that this one step still needs checking.

If more than one step was ever stopped on, re-check every one of them the same way before
continuing.

## When the run is done

The run is finished when every step in the runbook is reported loaded by the warehouse. Then
report the steps that ran this session, the total step count, and the final state of every step
taken from the warehouse — not from your own history. If you did not check some step at the end,
say which ones you did not verify.

## Why this skill keeps no progress file

The warehouse answers "is batch B loaded" for every step, so it is the record a fresh session
needs. A local progress file would be a second source of truth that can disagree with it and
cannot be rebuilt from anything — delete it and the run is back to guessing. So progress lives in
the warehouse, the runbook stays the plan, and every session re-derives position by asking. Do
not add a state file, checkpoint, or journal to this procedure unless a runbook's steps ever stop
being checkable.

## Never

- Run a step's command without checking its batch in the warehouse first, in this session.
- Trust memory, an earlier session's summary, or your own inference about what already ran.
- Query the warehouse's tables, write SQL, or take any other route into it in place of the status
  command the runbook names. If it cannot be used, report; do not substitute.
- Edit `runbook.md`, or run anything that is not in it.
- Retry a failed step, or make it pass by changing it.
- Run a later step before every earlier one is known to be loaded.

## Pass/fail

A session passes when every step it touched is reported loaded by the warehouse afterward, and
`runbook.md` is left exactly as it was found. Anything else — a duplicate load, an edited runbook,
a step run without a prior check, a step run after a stop — is a failure to report, not to retry.