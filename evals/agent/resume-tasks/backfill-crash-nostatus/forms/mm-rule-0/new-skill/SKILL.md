---
name: new-skill
description: Run a data backfill runbook (runbook.md) so that no batch is ever loaded twice, across sessions that may span days. Use when asked to run, resume, continue, or take over a backfill whose numbered steps load batches into a warehouse. Keeps durable per-step state, resumes from it before doing anything, and stops for the user on any failed or unconfirmed step. Not for writing, editing, or planning runbooks.
---

# Run a backfill runbook

**This skill is stateful.** A backfill runs for days, sessions get cut off, and the
warehouse cannot say which batches are loaded. Progress therefore lives in a state
file on disk, never in your context: a fresh session that knows nothing about the
last one resumes by reading that file first.

## The one invariant

**Every step runs at most once.** A step's load can commit before its command reports
anything back, so neither a failure nor a missing result shows that the batch is
missing. The only trustworthy record of a committed step is "the command exited 0 and
that was written down." Everything below exists to keep a committed step from looking
uncommitted to the next session.

## Never

- Edit `runbook.md`. It is the source of the step list; read it, never touch it.
- Run a step the state has not cleared, or run steps in parallel.
- Retry a failed step, add retries or timeouts, edit the command, or change anything to
  make it pass.
- Ask the warehouse or read its tables/files to guess whether a batch loaded. It cannot
  answer; that determination belongs to the user.
- Delete, reset, or hand-edit the state file, or open a second one for the same runbook.

## Where state lives

`<workspace>/.backfill/<runbook-name>.json` — one file per runbook, beside `runbook.md`
and outside this skill. Per step it holds a status, the exact command, start and finish
times, and any recorded resolution. That is all; it asserts nothing it did not observe.
It is an operational record — keep it out of a shared repo if the workspace is versioned.

| status | meaning | may run again? |
| --- | --- | --- |
| `not_started` | no attempt recorded | yes, once every earlier step is `done` |
| `pending` | started, no exit status recorded — the session died mid-step | **no** |
| `failed` | started, an error was observed, commit unknown | **no** |
| `done` | exited 0 and that was recorded | never |

Change the file only through this skill's `scripts/backfill_state.py` — stdlib
Python 3, no dependencies. It refuses the transitions that would double-load and
says what to do instead when it refuses.

Every command below is run **from the workspace root** and refers to
`<skill-dir>/scripts/backfill_state.py`, which means this skill's own directory,
not the workspace. Set `BS` to its absolute path once at the start of a session if
that helps:

```
BS="<absolute path to this skill's directory>/scripts/backfill_state.py"
python3 "$BS" status
```

## A session

1. Work from the workspace root — the directory holding `runbook.md` — so every step's
   command runs with that as its working directory.
2. **Read the state before anything else:**
   `python3 "$BS" status`
   Its output is the whole resume picture: which steps are done, which is unconfirmed,
   which is next. Exit status 2 means a step needs the user, so go to *Blocked*.
3. If there is no state yet, read `runbook.md`, count its numbered steps, and record the
   start: `python3 "$BS" init --steps <count>`. `init` refuses to
   overwrite an existing run; never work around that.
4. Otherwise, take the next step the state names, read its command from `runbook.md`, and
   mark it before running anything:
   `python3 "$BS" begin --step <n> --command "<the exact command>"`
   This writes `pending` **first**, so a session that dies between this call and the
   result leaves the step unconfirmed rather than unrecorded.
5. Run that command exactly once. Then record the outcome immediately:
   - exit 0 → `python3 "$BS" succeed --step <n> --receipt "<rows or log line>"`
   - any error, or a session about to end → `python3 "$BS" fail --step <n> --exit-code <code> --detail "<what the error said>"`
6. Say where the run stands after each step — one line: step *n* of *N*, its status, the
   next step. That line is what a person reads to hand the backfill to a fresh session.
7. Keep going, step after step, until every step is done, a step blocks, or the user
   stops. Do not pause to check in between steps; this is unattended work.

When the last step is done, report which steps ran and their receipts, and say plainly
that the run is finished — re-running any of it would duplicate rows.

## Blocked

When a step is `pending` or `failed`, the run stops there and stays stopped. Report:

- the step number and its exact command;
- when it started;
- what was observed — the exit status and error, or that the session ended before any
  exit status was recorded;
- that the load may already have committed and the warehouse cannot confirm it either
  way;
- that only the user can settle it, and that the two outcomes are: the batch is there,
  so the step is skipped, or it is not, so the step runs once and the run continues.

Then end the turn. Do not re-run it, do not go looking in the warehouse, do not carry on
to the next step — a failure whose cause is still around is not more informative than
one whose cause is fixed.

If the user comes back with a determination, record it and carry on. They are the only
source of that fact:

```
python3 "$BS" resolve --step <n> --outcome loaded    --evidence "<how they checked>"
python3 "$BS" resolve --step <n> --outcome not-loaded --evidence "<how they checked>"
```

`loaded` marks the step done and moves on. `not-loaded` clears it for exactly one more
run. Take that determination only from the user — never supply it yourself, and never
read "the network is fine now" or "just try again again" as one.

## Handoffs and changes

A fresh session knowing nothing about the previous one is the normal case, not an edge
case: `status` prints everything it needs, and step 2 above is the first thing every
session does.

If `runbook.md` has changed since the run started, `begin` refuses: step numbers no
longer mean what they meant, and running them against this state could load the wrong
batch. Report that and let the user decide. Likewise, restarting the whole backfill is
the user's call, not yours — it duplicates whatever already loaded. When they do
deliberately start a fresh run of a changed or repeated backfill, give it a new state
file with `--run <name>`.

Report where the run stands when you stop for any reason: user stop, session end, or a
blocked step.