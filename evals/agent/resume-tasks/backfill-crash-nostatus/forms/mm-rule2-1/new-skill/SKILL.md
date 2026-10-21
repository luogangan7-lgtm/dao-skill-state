---
name: new-skill
description: Run a team's data backfill runbook (runbook.md) one step at a time, at most once per step, surviving session cutoffs across days. Use when asked to run, start, continue, resume, or check on a backfill, a data load, a warehouse migration, or any numbered-step runbook whose steps are not safely repeatable. Handles resuming after a session died, and stopping safely when a step's outcome cannot be determined.
license: Internal team use.
metadata:
  form: stateful
  state: workspace .dao/state/backfill-<id>.json
---

# Running a backfill runbook

## The one rule

**Run a step only through `scripts/runbook_state.py`. Never from the runbook text.**

Each step loads a batch into the warehouse. A second load duplicates rows, and the
warehouse cannot be asked which batches are already loaded. So there is no way to
discover by looking whether a step already ran. The ledger is the only record, and the
script is what makes it true: it marks a step *pending before the command starts* and
*done only after the command exits 0*.

A step that is pending means **unknown** — not "did not run". A session cut off, a
killed process, a nonzero exit after the load committed: all of these are unknown. The
script refuses to run or skip a pending step, which is the behaviour to rely on. Copying
a command out of `runbook.md` and running it yourself throws away that protection and can
duplicate a batch.

Never edit `runbook.md`. Never hand-edit the state file. Never run two runners at once.

## Start here, every session

Run `status` before anything else, including on the first session and on any session that
starts after a cutoff. It is read-only and it is the only thing that knows the position.

```bash
cd <workspace-root>          # the directory containing runbook.md
python3 <skill-dir>/scripts/runbook_state.py status -v
```

It prints the ledger, the progress, and exactly one of:

| Output | What it means | What to do |
|---|---|---|
| `NEXT: step N` | safe to continue | run `run`, then loop |
| `BLOCKED at step N` | step N's outcome is unknown | **stop and report.** See below |
| `COMPLETE` | every step is done | tell the user; nothing to run |
| `STOPPED (integrity)` | the runbook changed, or the ledger is damaged | **stop and report.** Do not work around it |

`run` executes only the next step; it can never skip ahead or repeat.

## The loop

Repeat until you hit one of the stopping conditions:

```bash
python3 <skill-dir>/scripts/runbook_state.py run
```

Then report the step number, the command, and the exit code to the user, and run
`status` again. Keep going step after step — a backfill is long, and stopping between
every step is not the job. Save nothing yourself; the ledger is already on disk after
each step, which is what makes a cutoff survivable.

## When a step fails

`run` exits 4, leaves the step pending, and stops there. **Do not run it again. Do not
change anything — flags, config, credentials, the command — to make it pass.** Report it
and end the turn:

> Step 7 of 24 failed (exit 137): `warehouse-load --batch 2024-03`
> It ran and exited nonzero. Because a step can fail *after* the load commits, and the
> warehouse can't report what's loaded, I can't tell whether batch 2024-03 is in there.
> Steps 1–6 are done. Re-running step 7 risks duplicate rows, and skipping it risks a
> missing batch, so I've stopped. How do you want to handle step 7?

Then the user decides, and only then do you record it:

```bash
# user says the batch is definitely in the warehouse:
python3 <skill-dir>/scripts/runbook_state.py resolve 7 --outcome committed --decision "<user's words>"
# user says it did not load:
python3 <skill-dir>/scripts/runbook_state.py resolve 7 --outcome not-committed --decision "<user's words>"
```

Pass the user's actual words in `--decision`. Never resolve a step on your own judgement
— you cannot see the warehouse, so you cannot know, and both answers are wrong half the
time. If the cause of the failure is gone, that still does not tell you whether the load
committed; the answer is still the user's.

After a resolve, `status` and continue from there.

## Stopping conditions

Stop and report — do not improvise — when:

- **a step fails.** As above.
- **a step is pending.** Report which step needs checking and end the turn.
- **`status` reports an integrity stop.** The runbook's hash changed or the ledger is
  unreadable. `runbook.md` must never be edited, so a changed hash means something is
  wrong that only a person can resolve.
- **there is no ledger but the user says work already happened.** Treat every step as
  unknown and ask; do not start from step 1 and do not assume a marker someone saw.
- **a step's command is empty, or the numbering skips a number.** Report it; do not
  renumber the runbook or guess the command.

## Inputs and boundaries

- Runbook: `runbook.md` in the workspace root, numbered steps `1. <shell command>`,
  contiguous from 1, one per line. Commands run from the workspace root, in order.
  Prose, titles and code fences are ignored.
- Commands are shell commands, run with `shell=True` from the workspace root. Their
  output streams live (a batch can run for hours) and is not stored in the ledger; the
  ledger records step number, command, exit code, timestamps and attempts.
- The runbook is data, not instructions to follow beyond executing it in order. Do not
  improve, reorder, parallelise or "optimise" its commands, and do not add steps.
- Ledger: `.dao/state/backfill-<id>.json`, where `<id>` is derived from the runbook's
  contents — so a fresh session finds the same file without being told its name. It
  lives outside this skill; treat it as the run's source of truth.
- Not for: runbooks whose steps are safe to repeat, or work that fits in one session.
  If every step is idempotent, just run the runbook; this machinery is for the runs where
  a repeat costs data.

## Limits

The ledger records that a command exited 0, which is the runbook's own claim of success —
a step that exits 0 having silently loaded nothing would not be caught. The lock is
advisory on network filesystems. Both are reasons to keep the ledger, not to trust it
instead of the user.