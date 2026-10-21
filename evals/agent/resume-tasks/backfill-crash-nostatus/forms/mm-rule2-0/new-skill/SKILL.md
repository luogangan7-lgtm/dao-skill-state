---
name: new-skill
description: Run a numbered data backfill runbook one step at a time with durable state that survives session cut-offs. Use when asked to run, start, resume, continue, or check on a backfill, a batch load, or a runbook.md in the workspace. Records each step as running before executing it and never runs one twice, because loading a batch twice duplicates rows and the warehouse cannot report which batches are loaded. Triggers on backfill, runbook, resume the backfill, which step are we on. Not for editing a runbook, re-running a failed load without the user's answer, or general shell scripting.
---

# Running a backfill runbook

## The invariant

Every step loads a batch. **Loading a batch twice duplicates rows, so no step may run
twice** — not after a failure, not after a cut-off, not in a fresh session.

The warehouse has no way to report which batches are loaded. So a step's *effect* can
never be observed, only recorded. And a step can fail **after** its load committed — a
connection reset after commit is the ordinary case, not the exotic one.

That combination decides everything below: when an outcome is unknown, the only safe
move is to stop and ask. Nothing else.

## Every session starts here

State lives in the workspace, not in this skill: `.backfill-state/<runbook>.json`,
written durably. **A new session knows nothing** — never rely on your own memory of
what ran, and never trust a summary in the conversation. The state file is the only
record of what took effect.

Run from the workspace root that holds the runbook:

```bash
python3 <skill>/scripts/runbook.py status    # what took effect
```

| Situation | Do this |
|---|---|
| No state file yet | `init` — reads the runbook, records the steps, runs nothing |
| State exists, some steps done | `next`, then `run` |
| State exists, a step is blocked | stop — see "When a step is blocked" |
| Every step done | report the backfill complete |

`run` executes exactly one step: it marks the step `running` and flushes that to disk
**before** the command starts, runs the command, and marks `done` only if it exits 0.
If the session dies mid-step, the record already says `running`, and that is what stops
the next session from repeating it. Keep calling `run` — one step per call, saving
after each — until the runbook is done or blocked.

Give a long-running step a long tool timeout. If the call is cut off anyway, do not
re-run it: the state is already `running` and only the user can clear it.

## Never edit runbook.md

Read it, execute its steps in order, leave it alone. `scripts/runbook.py` only reads it
and records its hash at `init`. If the runbook changes mid-run, step numbers no longer
name the same commands and the script refuses to continue — report that rather than
guessing which side is right.

If the parser reports no numbered steps, that is a hint, not a verdict: **open
`runbook.md` and read it yourself** before telling the user it is malformed.

## When a step is blocked

A step left `running` (a session died mid-step) or `failed` (non-zero exit) has an
**unknown effect**. The warehouse cannot tell you whether the batch landed, and the
cause of the failure going away tells you nothing about it — a reset connection may
have committed before it broke, and the next attempt may look identical whether the
batch is there or not.

Stop the run. Report:

- the step number and its exact command
- exit code or signal, when it started, and the tail of `.backfill-state/<runbook>.log`
- that the batch is **unknown**, because the warehouse has no status query
- that it needs the user's decision, not your retry

Then wait. The user checks the warehouse directly — that is their side of the boundary,
not yours — and tells you the outcome. Record it only once they have:

```bash
python3 <skill>/scripts/runbook.py resolve <n> --did-load      # confirmed in the warehouse
python3 <skill>/scripts/runbook.py resolve <n> --did-not-load  # confirmed absent; step may run once more
```

`--did-not-load` is the only path that lets a step run a second time, it exists only
because the user just said so, and `attempts` keeps the earlier failure visible to
every later session. If they have not answered, resolve nothing and change nothing.

## Shortcuts that defeat this

Each of these looks reasonable and produces duplicate rows or a silently incomplete
backfill:

- Running a step's command directly, or wrapping it in a `retry`, `until`, or `||`-guarded
  shell loop. Everything goes through `run`.
- Hand-editing the state JSON to clear a blocked step, or deleting it to start over.
- Reading the warehouse's storage files, query logs, or table internals to infer what
  loaded. There is no check to substitute for the user's answer.
- Skipping the blocked step and continuing with the next one. The backfill would end
  with a batch nobody knows about.
- Re-running because the failure looks transient, the network is fine now, or "it
  probably didn't get that far."
- Treating a successful step as a licence to skip ahead, or assuming a fresh session
  starts from zero.

## What this guarantees, and what it does not

Steps **never run twice** from this state: a step runs only when recorded `todo`, and
`running` is written before the command exists. That is at-most-once execution, not
exactly-once delivery — when an outcome is unknowable, the run pauses for a human
instead of guessing. The script is the enforcement point because prose alone gets
skipped when a run gets long.

## Files

- `scripts/runbook.py` — `init` · `status` · `next` · `run` · `resolve <n>`; keeps the
  state, records each step before running it, refuses to pass a blocked step, and takes
  an exclusive lock so two sessions cannot run the same runbook at once.
- `.backfill-state/` (written in the workspace, never here) — `runbook.json` state,
  `runbook.log` output.