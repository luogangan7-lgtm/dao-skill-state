---
name: new-skill
description: Run the team's data backfill runbooks (runbook.md in the workspace) so no batch is ever loaded twice, across days, session cut-offs, crashes and mid-load connection resets. Use when asked to run, start, resume, continue, check or report a backfill or a runbook. Asks the warehouse about every batch before loading anything, resumes correctly in a session that knows nothing about the last one, and stops and reports on the first failure instead of retrying it or changing anything to make it pass.
---

# Data backfill runbooks

A backfill loads batches into a warehouse, one step at a time, in order. It can
run for days while sessions start and end. Loading a batch twice duplicates
rows, and a step can fail *after* its load committed (a connection reset after
the commit is the classic case), so a failure is not proof that nothing loaded.

Everything below exists to prevent one bad outcome: a batch loaded twice.

## Form

Stateful, in a specific way. The **warehouse is the authority for effect**: every
step has a check command that says whether its batch is loaded, and that command
is asked about *every* step on *every* pass — including steps a previous session
already finished, and including the step that failed. A small ledger at
`<workspace>/.backfill/<runbook>.json` records only what the warehouse cannot
answer: that a step failed and was not retried, and that a step was left
mid-flight. It never overrides the warehouse. Where they disagree, the run stops.

## Never

These are not preferences. They are how duplicate rows happen.

- **Never edit `runbook.md`.** Not to add a check command, not to skip a step, not
  to fix a typo. If the runbook is wrong, report that; a person edits it.
- **Never run a step's command by hand.** The check-before-load is the whole
  guarantee. A command typed straight into a shell has nothing deciding whether
  its batch is already in the warehouse.
- **Never edit the ledger by hand** to make a run proceed. Resolve the step it
  points at instead.
- **Never retry a failed step**, and never change anything (code, config, flags,
  environment) to make it pass. A failed step is reported to a person.
- **Never treat "the check says not loaded" as permission to reload a step that
  a previous run recorded as failed.** That is a decision for a person.

## Run it

1. **Read `runbook.md` yourself, in full.** It is the runbook; do not run it
   blind. You are looking for the numbered steps and, somewhere in it (it need
   not be a numbered step — it may be a "how to check" line, a comment, or prose
   beside a step), the command that asks the warehouse whether a batch is loaded.
   Do not invent that command if it is not there; see *When it stops*. A runbook
   that numbers two different steps the same, or numbers none at all, is refused
   rather than guessed at.

2. **Run the runner, from the workspace root.** One command starts a backfill and
   the same command continues it, after a failure or a week away:

   ```bash
   python3 <skill-dir>/scripts/backfill_run.py \
     --runbook runbook.md \
     --check-cmd '<the command the runbook names>'
   ```

   Placeholders the check command may use: `{step}` (the step number), `{batch}`
   (the batch id, see below), `{command}` (the step's own command). The check
   must print `LOADED` or `NOT_LOADED` on stdout and exit 0 — if the runbook's
   command answers some other way, wrap it so it does.

   Two adjustments, both still one command:
   - A step's batch id is usually not its step number. Pass it:
     `--batch 3=2024-01-15_users`. Repeat `--batch` per step. Required whenever
     the check command uses `{batch}`.
   - Different steps checked by different commands: `--checks-file checks.json`
     with `{"3": "<command>", "4": "<command>"}`. A plain `"3": "<command>"`
     works too.

3. **Read the result, then report it.** Exit codes:

   | code | meaning | what you do |
   |---|---|---|
   | 0 | every step is in the warehouse | report completion |
   | 2 | a step failed | report the failure verbatim; change nothing, rerun nothing |
   | 3 | the warehouse could not answer, or the ledger and warehouse disagree | report which step and why; rerun the same command once access is fixed |
   | 1 | bad arguments or unreadable inputs | fix the invocation, not the runbook |

`--dry-run` asks the warehouse about every step and runs nothing. Use it when
you want to see the plan, or to confirm the check command answers for every step
before committing to a days-long load.

## What the runner does, so you can explain it

For every step, in order: ask the warehouse. If loaded, skip it. If not loaded
and no failure is on record, mark it **pending** in the ledger (flushed to disk),
run it, and mark it **done** only after it exits 0. On a non-zero exit, mark it
**failed** and stop. At most one load is ever in flight, and a load only starts
when the warehouse has just said the batch is absent.

This is what makes the crash case safe. A session killed mid-load leaves the step
`pending`; a connection reset after commit leaves it `failed`. On the next run
both are resolved the same way — by asking the warehouse, not by trusting the
exit code or the ledger:

- failed **and** the batch is loaded → the load committed before the reset. It is
  recorded done and the run moves on. This is the case that duplicates rows if
  you trust the exit code.
- failed **and** the batch is not loaded → it really did fail. It is not retried.
  The run stops and says which step a person has to decide about.
- pending **and** the batch is not loaded → that attempt committed nothing, so
  running it now is its first effective run, not a retry.
- the check hangs, errors, or answers anything else → **UNKNOWN**, which is never
  read as "not loaded". The run stops at that step. An unanswered question must
  not become a second load.

## When it stops, report it — do not work around it

It stops at the *first* step it cannot resolve. Steps before that one, which the
warehouse confirmed were absent, are loaded normally: each load is checked on its
own, so making that progress cannot duplicate anything, and it stands — the next
run skips it.

- **A step failed.** Give the step number, its command, its exit code, the tail
  of its output, and the check command that will settle whether it committed on
  the next run. Say plainly that it was not retried and nothing was changed to
  make it pass.
- **A check could not answer.** Give the step number and what the check printed.
  Before concluding the runbook lacks a check, re-read `runbook.md` yourself: a
  check named in prose beside a step is easy to miss, and pass it with
  `--checks-file`. If the runbook truly names none, say so and stop — a backfill
  that cannot be verified cannot be run without risking duplicates.
- **The ledger and the warehouse disagree** (a step recorded loaded, warehouse
  says absent). Report it and stop. That is not something to reconcile by
  reloading or by editing the ledger.
- **`runbook.md` changed since the last run.** Report it and stop. A runbook
  edited mid-backfill shifts which batch a step number means.

## Reporting back

State: which steps are in the warehouse, which were loaded this run, which were
skipped because an earlier run loaded them, and where the run stopped and why.
The ledger path and each step's log file under `.backfill/logs/` are worth
passing on. A person picking this up days later needs the ledger path, the
step number, and the check command for it.

## Files

- `runbook.md` — read only. The source of every command and of the check command.
- `<workspace>/.backfill/<runbook>.json` — the ledger. Step status, exit codes,
  output tails, runbook digest. The only thing the runner writes.
- `<workspace>/.backfill/logs/` — full output of each load attempt.

Requires Python 3 and a POSIX shell. Steps run from the workspace root, in
runbook order.