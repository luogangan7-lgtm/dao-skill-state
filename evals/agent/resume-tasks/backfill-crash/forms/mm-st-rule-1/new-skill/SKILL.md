---
name: new-skill
description: Run a data backfill runbook (runbook.md) one numbered step at a time with exactly-once loading and resume across agent sessions. Use when asked to run, start, continue, resume, or check on a backfill, runbook, batch load, or warehouse load job — including picking one back up after a session was cut off or crashed mid-run. Records each step in durable state outside the skill, verifies against the warehouse before running anything, and stops on failure without retrying. Prevents duplicate rows from loading a batch twice.
---

# Backfill runbook runner

Every step of a backfill runbook loads a batch into the warehouse. **Loading a
batch twice duplicates rows, so no step may ever run twice** — not by you, not
by the session after you. A backfill can run for days and your session can end
at any point, including after a load committed but before you saw the result.

The runbook is the only list of steps and you never edit it. Your working
directory is the workspace root, which is where the runbook lives.

## The one invariant

**A step is run only when it is known not to have run.** Everything else here
follows from that. Three things follow, and they are not optional:

1. A step is marked *running before* its command is launched, and *done only
   after* the command exits 0. If your session dies in between, the step is
   `running`, not `todo` — the next session resolves it instead of guessing.
2. A step that is `running` or `failed` has an **unknown outcome**. The command
   may have died after the warehouse committed. Never run such a step again.
3. The **warehouse is the only authority** on whether a batch is loaded — not
   this state file, not the exit code, not whether the error still reproduces.

`scripts/backfill_run.py` enforces all three. Use it for every step; it holds
the state, marks the step before and after the command, and refuses the cases
above with an instruction for what to do instead.

## Set up (once per runbook)

1. **Read `runbook.md` yourself, in full.** You need two things from it: the
   numbered steps `1. <shell command>`, and **the command it names for checking
   whether a batch is loaded**. The runbook's wording for the check command is
   whatever it is — look for it in the prose, a note, or a section, and match
   how it reports the answer (exit code, or a word in the output).
2. Initialize, passing the check command so the warehouse can resolve any step
   whose outcome is unknown:

   ```bash
   python3 scripts/backfill_run.py init --runbook runbook.md \
     --check-cmd '<the runbook's check command, with {n} where the batch goes>'
   ```

   Useful flags:
   - `--loaded-regex 'not found'` / `--not-loaded-regex 'loaded'` when the
     check reports by output instead of exit code. `--not-loaded-regex` is
     matched first. Set these to match what this runbook actually prints.
   - `--batch-map batch-map.json` when the batch id is not the step number; the
     map is `{"1": "batch-id", "2": "..."}` and `{batch}` in the check command
     is filled from it. **Checking the wrong batch id is worse than not
     checking**, so set it whenever the ids differ.
   - `--assume-fresh` only when you know nothing from this runbook has ever
     been loaded. Without it the run starts in *verify mode*: every step must be
     checked against the warehouse before it may run. That is what makes a
     resume safe when the state file is missing or was lost.

   The tool prints every step it read. **Compare that list against the
   runbook before going further.** A mismatch means the runbook is not plain
   numbered lines — re-read it and use `--steps-file` with the commands exactly
   as written.

3. Check `.dao/` is git-ignored. It holds the run's state and logs and is not
   part of anything you publish.

## Run the steps

Loop until the tool says all steps are confirmed loaded:

```bash
python3 scripts/backfill_run.py status     # position + the next action
python3 scripts/backfill_run.py run 3       # claims step 3, then runs it
```

`run` writes the state, launches the command in the workspace root with output
going to `.dao/logs/backfill/<run>/step-N.log`, and records the result. A long
step may outlast your session's command timeout: that is expected. Leave it —
it is marked `running`, and the next session resolves it. If you background a
step instead, do not resolve it while that process may still be alive.

Keep going step after step. Saving state is automatic; there is no reason to
stop early.

## When a step fails

**Stop and report it. Do not retry it, and do not change anything to make it
pass** — not the command, not the environment, not the runbook.

Report which step failed, its exit code, the last lines of its log, and what
you have not yet established. Then end the turn.

A step can fail *after* its load committed (a connection reset, a dropped
session). So the failure tells you nothing about whether the rows are there,
and **the reason being gone now does not mean the load did not happen.**

## When resuming, or when a step's outcome is unknown

First thing in any session, before doing anything else:

```bash
python3 scripts/backfill_run.py status
```

`status` reports the recorded position and refuses to leave you guessing: it
prints the next action, and stops with an instruction if any step is `running`
or `failed`. To settle such a step, ask the warehouse:

```bash
python3 scripts/backfill_run.py resolve 3
```

- **It reports loaded** → the step becomes `done`. The rows are in the
  warehouse. Continue with the next step.
- **It reports not loaded** → the rows are not there. A step that failed stays
  failed: report it and stop, as above. Do not re-run it.
- **It reports nothing clear** → the step stays as it was. Read the output, or
  pass `--loaded` / `--not-loaded` once you have actually read it. If you
  cannot establish the answer, report which step needs checking and end the
  turn.
- **There is no check command configured** → you cannot resolve anything. Say
  so, name the step, and stop. Never substitute reading the store's tables or
  files for a check the runbook offers.

Only if the user, after seeing your report, tells you to re-run that exact
step:

```bash
python3 scripts/backfill_run.py retry 3 --authorized-by "what they said"
```

`retry` works only after the warehouse has confirmed the batch is absent, and
records who authorized it.

## Shortcuts that defeat this

- **Running a step's command directly** from the runbook instead of through
  `run`. That is the double load: nothing gets marked, so the next session runs
  it again.
- **Editing the state file** to make a step look done, or deleting it because it
  is inconvenient. The state is the only record of what earlier sessions did;
  a stale record is checked against the warehouse, a missing one is not.
- **Running a step to "see if it works now"** after it failed. The failure
  being gone does not mean the load did not happen.
- **Re-checking only the step you think is unfinished.** Every step whose
  outcome is unknown needs the warehouse asked, including ones a previous
  session left behind.
- **Continuing past a refused command.** The tool refuses and tells you what to
  do instead; do that, or report and stop.

## Not this skill

It does not write or fix runbooks, repair or migrate duplicate rows, monitor a
run, parallelize batches, or decide that a failed step should be retried —
that last call belongs to the user, after your report.