---
name: backfill-runbook
description: Run a multi-day data backfill from a numbered runbook.md, resuming across sessions without ever loading the same batch twice. Use when asked to run, start, continue, or resume a backfill runbook, or to check where a backfill stopped. Verifies every step against the warehouse before loading anything, records progress for the next session, and stops on the first failure.
---

# Backfill runbook

Run the numbered steps of `runbook.md`, in order, from the workspace root, **loading each batch
exactly once** — including when an earlier session was cut off mid-step, and including when a step's
command reported failure after its load had already committed.

## The rule everything else serves

**Checking is free; loading is not.** The warehouse is the only authority on whether a batch is
loaded. Before this session runs any load command, it establishes the state of *every* step in the
runbook — not just the one it assumes is next. A step whose status is unknown is not a green light:
stop and report it.

## Form: fixed steps, with no local state of record

A run spans days and sessions, but this is deliberately **not** a stateful skill. Every step's
"did it take effect?" question has an answer in the warehouse, and that answer is re-read at every
session start, so a resume never depends on a file this skill wrote. A local progress file that
records "done" is actively dangerous here: a step can commit and *then* fail, so a done-only file
reloads a batch that is already in the warehouse.

`backfill-progress.md` (below) is a derived note — for humans, and to tell the next session what
already happened. **When it disagrees with the warehouse, the warehouse is right**: correct the
note, never believe it.

## Inputs

- `runbook.md` at the workspace root. Steps are numbered lines: `1. <shell command>`.
  **Never edit the runbook, and never edit, reformat, "fix", or extend a step's command.** Run each
  command exactly as written.
- The runbook also names the command that asks the warehouse whether a batch is loaded, and usually
  how to substitute a batch id. Read both out of the runbook.

Stop and ask the user — do not guess — when:

- there is no `runbook.md`, or it has no numbered steps
- the runbook does not name a check command
- it is not clear from the runbook which batch id a given step loads (a wrong guess here is the one
  mistake this skill exists to prevent)
- what the check command's output means is unclear. Do not invent an expected output.

## Procedure

### 1. Orient

Read `runbook.md` end to end. Note the ordered steps and their commands, the named check command and
how a step maps to its batch id, and anything the runbook says about expected output or failure
modes.

If `backfill-progress.md` exists, read it for orientation: where the last session got to, and any
step that failed and is waiting on the user.

### 2. Check every step against the warehouse

Build `.backfill/checks.json`: one entry per runbook step, in step order, where `check` is a
ready-to-run read-only command derived from the runbook's named check command.

```json
[
  {"step": 1, "batch": "customers_2019_q1", "check": "./scripts/check_batch.sh customers_2019_q1"},
  {"step": 2, "batch": "customers_2019_q2", "check": "./scripts/check_batch.sh customers_2019_q2"}
]
```

Then run the helper (from this skill's base directory):

```bash
python3 scripts/batch_status.py runbook.md --checks .backfill/checks.json \
  --out .backfill/status.json --expect-steps 2
```

It refuses to run unless the entries cover **every** step in the runbook. That refusal is the point:
a step nobody checked is a step that might already be loaded. Add the missing entry and re-run. Do
not run a runbook step to "find out".

Then classify each step by reading its check output against what the runbook says it means:

- **loaded** — the warehouse already has this batch
- **not loaded** — it does not; this one still needs to run
- **unknown** — the check failed, timed out, or its meaning is unclear

If any step is **unknown**, or a check command itself failed, stop. Report those steps and run
nothing. Determining a batch's real status comes before any new load, not after.

### 3. Run steps in order, one at a time

Start at the **first** step the warehouse reports as not loaded. Skip every step before it — those
batches are already in the warehouse and their commands must not be run again.

- Run each step's command exactly as written, from the workspace root, one step at a time. Never
  combine several steps into one shell call, and never run two at once.
- After each successful step, update `backfill-progress.md` **before** starting the next. That write
  is what makes a cut-off survivable.

If loaded steps are not a clean prefix — say steps 1–3 and 5 are loaded but 4 is not — say so in the
report, and proceed from step 4. Never re-run step 5 to "get back in order".

### 4. When a step fails: stop, check, report. Never retry.

1. **Do not re-run it.** Do not edit the command, the runbook, or the warehouse to make it pass.
2. A non-zero exit does **not** mean the load did not happen. A connection reset after commit, a
   dropped client, or a timeout all report failure on a batch that is already in the warehouse.
   Check that one step's batch in the warehouse before concluding anything.
3. Update `backfill-progress.md` with the step number, exit status, the error text, and what the
   warehouse shows for that batch.
4. **Stop the run.** Report:
   - which step, the command, and what it printed
   - whether the warehouse shows that batch loaded, and how you determined it
   - what the next session should do

   - *loaded* — the data is in. The failure is a reporting problem, not a data problem. Say that
     plainly; the step must not run again.
   - *not loaded* — the batch was not loaded. It needs the user's decision before anyone runs it
     again.
   - *unknown* — this step must be checked before anything else runs.

### 5. Finish

When the last step completes, mark the run done in `backfill-progress.md` and report the total step
count and the batches loaded.

## Never

- edit `runbook.md`, or a step's command, to make a step pass
- run a step the warehouse reports as loaded
- run a step whose status is unknown or unchecked
- re-run a failed step in the same session
- treat `backfill-progress.md` as proof of anything
- "make it pass": no dropped or recreated tables, no toggled flags, no skipping a check because it
  is slow or looks wrong
- run step commands in parallel or in a single combined shell call

## Files this skill writes

Only these, all in the workspace root — `runbook.md` is never touched:

- `.backfill/checks.json`, `.backfill/status.json` — throwaway check inputs and raw results. Safe to
  delete; `checks.json` is rebuilt from the runbook at every session start.
- `backfill-progress.md` — the derived human-facing note. Wiped by the warehouse on disagreement.

```markdown
# Backfill run status

Runbook: runbook.md
Updated: 2026-10-21T11:45Z
Warehouse status: `.backfill/status.json` from the check at 2026-10-21T11:45Z

- steps 1-6: loaded (warehouse confirmed 2026-10-21T11:45Z)
- step 7: FAILED 2026-10-21T11:02Z — exit 1, "connection reset"; warehouse shows NOT loaded;
  stopped and reported to user; needs operator decision before any retry
- next action: step 7
```

## See also

`references/resume-and-failure.md` — worked examples of a cut-off session, a crash after a
committed load, and an uncheckable batch.