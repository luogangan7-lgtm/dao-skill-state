---
name: new-skill
description: Run the team's data backfill runbooks (runbook.md in the workspace), one numbered step at a time, across sessions without ever loading a batch twice. Use when asked to run, start, continue, resume, or check on a backfill or batch-load runbook, or when picking up a backfill a previous session left mid-flight. Progress is durable on disk, so a fresh session with no memory of the last one resumes exactly where it stopped; a step whose outcome cannot be determined stops the run for a human instead of being retried. Not for editing runbooks, writing SQL, or querying the warehouse.
---

# Backfill runbooks

Run a backfill runbook to completion, one step at a time, across as many sessions as it takes.

`$SKILL_DIR` below is the directory holding this file.

## The one rule

**No step may ever run twice.** Every step loads a batch into the warehouse, and a second
load of the same batch duplicates rows. Two facts make this hard:

1. A step can exit non-zero **after** its load already committed (a dropped connection,
   say). A failure is not evidence that the batch is missing.
2. The warehouse **cannot be queried** for which batches are loaded. There is no way to
   tell from here whether a step took effect.

So the outcome of any step that did not cleanly exit 0 is permanently unknown to you.
Never resolve it by re-running.

## Procedure

Run commands from the **workspace root** — the directory containing `runbook.md`. Steps
run in `runbook.md`'s order, each with that directory as the working directory.

### 1. Every session starts here, before anything else

```
cd <workspace root>
python3 "$SKILL_DIR/scripts/runbook_state.py" status --runbook runbook.md
```

`status` runs nothing. It reads the durable state and tells you where the backfill is.

- **`BLOCKED at step N`** → go to *Blocked*, below. Stop.
- **`Runbook complete`** → the backfill is done. Report and stop.
- **`next step: N (ready to run)`** → go to procedure step 2 below.

### 2. Run the step it names

```
python3 "$SKILL_DIR/scripts/runbook_state.py" run --step N --runbook runbook.md
```

This is the **only** way a step may run. The script records the step as `pending` and
writes that to disk *before* the command starts, runs it, then records `done` only if it
exits 0. Use `--step N` so the run is explicit.

- **Exit 0** → step N is done. The script prints the next step number. **Keep going**
  with procedure step 2 for it, in this same session, until the runbook is finished or
  blocked. A session is not a stopping point; stop only when the script says so.
- **Exit 4** → the step command itself failed. Go to *Blocked*.
- **Exit 3** → refused. Read the message and do exactly what it says. Do not work
  around it. Report and stop.
- **Exit 1** → the runbook could not be read or the state file is unusable. Read
  `runbook.md` yourself, then report what is wrong and stop. Do not run steps read from
  a file this script could not parse.

Run steps **in the foreground, one at a time**. Never background a step and move on: you
would lose its outcome and break the runbook's order.

**One session at a time per runbook.** `run` holds a lock while it works and refuses to
start if another session is mid-step. `status` is always safe to read. If the user wants
two sessions on one backfill, say no and explain that two sessions could load the same
batch twice.

### 3. When it is finished

Report: every step recorded `done`, the count out of total, and where the state file is.

## Blocked

A step is blocked when it is `failed` (the command exited non-zero) or `pending` (a
session ended or was cut off while it was running — it may or may not have started).
Both mean **its load outcome is unknown**, and neither may be re-run.

When the script reports a block, **stop and report**. Do not:

- re-run the step, "just to see", or with adjusted flags, environment, or arguments
- edit `runbook.md` — ever, for any reason
- edit, delete, or hand-edit the state file
- run the step's command directly, around the script
- read the warehouse's own storage files to guess what loaded
- run any later step, which would leave the blocked step's batch unloaded

Report exactly this:

- the step number, its status, when it started, and the command text
- how many steps are `done` so far
- that the load outcome is **unknown**, and why it cannot be determined here
- that you did not retry, did not edit the runbook, and did not touch the state file
- what the human has to decide: whether that batch is in the warehouse

**Do not infer a step's outcome from the environment.** A log file, an output directory,
or the warehouse's own storage is not reconciliation — reading it to guess what loaded is
a guess, and guessing wrong duplicates rows. The only proof a step ran is a `done`
record in the state file, written by the script. If you did not read that record, you do
not know.

## Resuming after a session ends

The state file is the whole handoff. A fresh session needs nothing else: run `status`
first and follow it. A step left `pending` is **not** unfinished work to pick up — it is
unknown work, and it blocks the run until a human resolves it.

## State file

`<workspace root>/.backfill-state/runbook.md.state.json`, written atomically (temp file,
fsync, rename), so a crash mid-write leaves the previous good copy rather than a
truncated one.

```json
{
  "version": 1,
  "runbook_path": "/abs/path/runbook.md",
  "runbook_sha256": "…",
  "created_at": "2026-10-21T10:00:00Z",
  "updated_at": "2026-10-21T11:30:00Z",
  "steps": {
    "1": {"status": "done",    "command": "…", "started_at": "…", "finished_at": "…", "exit_code": 0},
    "2": {"status": "pending", "command": "…", "started_at": "…"},
    "3": {"status": "failed",  "command": "…", "started_at": "…", "finished_at": "…", "exit_code": 1, "note": "…"}
  }
}
```

`status` is `done` (the command exited 0), `pending` (claimed, outcome unknown) or
`failed` (command exited non-zero, outcome unknown). `python3
"$SKILL_DIR/scripts/runbook_state.py" show --step N` prints one step in full.

The script also refuses to run if `runbook.md`'s hash has changed since the run started —
renumbered steps would point at different commands. Report that; never resolve it by
editing either file.

Keep `.backfill-state/` local: it holds operational detail about the run, not something
to commit or publish. **You** never edit it. If a human has checked the warehouse
directly and decided what happened, they may correct the file themselves — that is their
call and theirs alone.

## Non-goals

Not for authoring or fixing runbooks, writing the load commands, or querying the
warehouse. This skill executes a runbook as written.