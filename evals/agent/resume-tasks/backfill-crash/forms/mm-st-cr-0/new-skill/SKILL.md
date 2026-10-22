---
name: new-skill
description: >-
  Run a team's data backfill runbook — `runbook.md` in the workspace — from the
  workspace root, one numbered step at a time, in a way that survives agent
  sessions being cut off mid-run. Use when asked to run, start, continue, resume,
  or check on a backfill, batch load, or data load runbook. Checks the warehouse
  before every step, so a batch is never loaded twice; a failed step is reported
  and never retried, patched, or worked around.
---

# Data backfill runbooks

A runbook is `runbook.md` at the workspace root: numbered steps (`1. <shell
command>`), run in order from the workspace root. Each step loads a batch into the
warehouse, and a batch loaded twice duplicates rows. This skill runs the runbook
and nothing else — it never writes, edits, repairs, or reorders runbook steps or
the commands in them.

**Form: fixed steps plus a check-before-every-step resume rule.** No progress
file, checkpoint, or local state is kept. The warehouse is the only record of
what took effect; a stale local record is exactly what makes an already-finished
step run a second time. A run that was cut off is continued by running the same
command again — the position is re-derived from the warehouse, never from a
session's memory.

## Four rules

1. **Check before you run, every step.** Before each step, ask the warehouse
   whether that step's batch is already loaded; skip it if it is. Do this on
   every invocation, for every step — not only for the step a previous session
   reported as failing. A session that died on step 7 says nothing about steps
   1–6, and step 7 itself may well have loaded before it died.
2. **Never run a step twice.** Within one invocation a step runs at most once.
   Across invocations the warehouse check is what makes another run safe. It is
   never optional and is never replaced by a local record of finished steps.
3. **Never edit `runbook.md`, and never change a step or its command to make it
   pass.** The runbook is read-only input. A step that fails gets reported, not
   repaired.
4. **On failure: stop and report.** Do not retry it, do not skip it, do not go on
   to later steps, do not change anything.

## Procedure

### 1. Locate and read the runbook

Read `runbook.md` at the workspace root in full. Read it yourself; do not stop at
the first line that looks unparseable. From it take:

- the numbered steps, in order, each kept exactly as the runbook writes it;
- the command the runbook names for asking the warehouse whether a batch is
  loaded. The runbook states it in whatever form it uses — a section, a fenced
  block, a line of prose, a comment above the steps. Find it as written. Do not
  assume a particular heading or field name, and never invent a status command.

If the runbook genuinely names no way to ask the warehouse about a batch, stop and
report that. Without it, a step whose load committed before the run died cannot be
told apart from a step that never ran, and running the latter again risks
duplicating rows.

### 2. Write one check command per step

For each step, write the single command that asks the warehouse about *that
step's batch* and answers in its exit code:

| exit code | meaning | what the runner does |
|---|---|---|
| `0` | batch is loaded | skip the step |
| `1` | batch is not loaded | run the step |
| `2` or more | could not find out | do not run the step; stop and report |

Build each check from the runbook's own status command. Illustrative only — the
real shape comes from the runbook, not from this example:

```
runbook's step:           1. load_batch.py 2026-03-01
runbook's status command: warehouse.py batch-status 2026-03-01   # prints LOADED or NOT LOADED
check for that step:      sh -c 'out=$(warehouse.py batch-status 2026-03-01) || exit 2; \
                                     [ "$out" = LOADED ] && exit 0 || exit 1'
```

**A pipe loses the exit code.** `warehouse.py batch-status X | grep -qx LOADED`
throws the status command's exit away and reports "not loaded" even when the
answer was unknown — which is how a check that should have stopped re-loads a
batch that is already there. Capture the command's status (`|| exit 2`) and read
its output separately, as above, or use whichever shape keeps all three outcomes
distinct.

So make each check tell "not loaded" (exit 1) apart from "the check itself could
not find out" (exit 2). A connection error, a missing table, or a warehouse that is
unreachable is not a negative answer.

Before a run that will take a while, run each check by hand once and confirm it
exits 1 for a batch that is not loaded and 2 for an unreachable warehouse. A check
that cannot make those two answers apart is not safe to run unattended.

### 3. Run it

From the workspace root, in the foreground, pass every step with its own check,
in runbook order:

```bash
python3 <skill-dir>/scripts/run_backfill.py \
  --workspace "$PWD" \
  --log .backfill-runs/backfill-2026-03.log \
  --step 'python3 warehouse/load_batch.py 2026-03-01' \
  --check 'sh -c '"'"'out=$(python3 warehouse/warehouse.py batch-status 2026-03-01) || exit 2; [ "$out" = LOADED ] && exit 0 || exit 1'"'"'' \
  --step 'python3 warehouse/load_batch.py 2026-03-02' \
  --check 'sh -c '"'"'out=$(python3 warehouse/warehouse.py batch-status 2026-03-02) || exit 2; [ "$out" = LOADED ] && exit 0 || exit 1'"'"''
```

(The nested quoting is only awkward because it is inside this document; write the
checks in a form your shell reads comfortably.)

Steps can run for hours — let them. Do not background the runner, and never run a
step's command yourself, even when it looks like a trivial one-liner. Running a
step outside the runner is the one move that skips the check, and it is how rows
get duplicated.

Always pass the whole runbook, including steps earlier sessions already finished:
the runner checks and skips them. The same command both starts a run and continues
one. It never refuses to start because of an unknown past — that refusal is what
would leave a backfill stuck for good.

### 4. Report

State which steps were already loaded and skipped, which ran and succeeded, and
where the run stopped. When it stops, quote the step's command, its exit code,
and the last lines of its output as the runner printed them. If it stopped because
a check could not answer, name the step, quote what the check printed, and say
plainly that nothing was run for it.

### Continuing after a cut-off or a failure

Run the same command again. Every step is checked first, so whatever took effect is
skipped and the run picks up at the first step that did not.

A step that failed last time is a special case: the runner cannot know whether it
committed, because a connection reset after the load commits looks exactly like a
failure. So it stops there and reports rather than guessing, and only the
warehouse settles it — the next invocation runs that check before doing anything
else with the step. Put this in the report: **the failure being explained, or the
error being gone, does not show whether the batch is loaded.** Never reason about
it from the error message.

If a session is cut off in the middle of a step, the same applies: run it again and
let the check decide. Do not tell the user a half-killed step is safe to re-run
without it.

Note the boundary this leaves, so it is a decision and not an accident: a step that
failed and left nothing behind is re-run by the next invocation, because the
warehouse says it is not loaded and a load that never committed cannot duplicate
anything. What never happens is a retry inside the failing run, a retry without the
check, or a repair of the step to make it pass.

## Never

- run a step's command directly, outside the runner
- skip the check because a previous session said a step already ran
- check only the step that failed last time
- retry a failed step within the run, or edit the step, its command, `runbook.md`,
  or warehouse state to make it pass
- keep a local list of finished steps and trust it instead of the warehouse
- run steps in parallel or out of order
- continue past a failed step or a step whose check could not answer

## Files

- `scripts/run_backfill.py` — the runner. Keeps no state. Exits `0` when every
  step is done, `1` when it stopped with an outcome it could not confirm.
- `runbook.md` (in the workspace) — input, read-only.