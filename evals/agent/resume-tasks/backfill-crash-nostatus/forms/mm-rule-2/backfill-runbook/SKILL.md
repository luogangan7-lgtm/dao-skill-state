---
name: backfill-runbook
description: Run a team's data backfill runbook (a workspace runbook.md of numbered steps, each loading one batch into the warehouse) across sessions that may be cut off, so no batch is ever loaded twice. Use when asked to run, start, continue, resume, or check on a backfill, a batch load, or a runbook in the workspace, including picking a run back up after a break. Handles resuming mid-run and reporting a step whose outcome is unknown. Not for writing or editing runbooks, and not for ad-hoc SQL or one-off queries.
metadata:
  version: "1.0.0"
---

# Backfill runbook

A backfill takes days. Sessions get cut off. Each step loads a batch, and the
warehouse cannot be asked which batches are loaded.

## The one invariant

**No step runs twice.** Loading a batch twice duplicates rows, and nothing in
this workspace can detect or undo that.

Everything below follows from it. In particular:

**A failed step is not a step that did not run.** A load can commit and *then*
fail — a connection reset after the warehouse accepted the batch. So a step that
did not exit 0 has an **unknown** outcome: it may be fully loaded. Treating
"it failed" as "it didn't load" is the mistake this skill exists to prevent, and
it is how duplicate rows get created.

## Never

- **Never edit `runbook.md`.** Not to fix a typo, not to reorder, not to add a
  step. If it is wrong, report it.
- **Never run a step's command yourself** with the shell or bash tool, however
  small it looks, however sure you are it already ran. Always through the script.
- **Never retry a failed step**, and never change anything — a flag, a config, a
  credential, the runbook, a source file — to make it pass.
- **Never hand-edit, delete, or "repair" the state file.** It is the only record
  of what loaded.
- **Never skip past a blocked step** to a later one.
- **Never guess at warehouse contents** by reading its storage or running ad-hoc
  queries to work out what is loaded. There is no supported way to ask; a guess
  is not a reconciliation.

## Where the state lives

`<workspace>/.backfill-state/<runbook-name>.json`, beside the runbook.

Written only by `scripts/backfill_step.py`, which is the single writer. It is
deliberately **outside** the skill folder so it survives and is never packaged
with the skill. Treat it as read-only to you; to see progress, run `status`.

## Resume: the first thing any session does

A session that knows nothing must be able to continue, so **run `status` before
anything else**, before proposing a plan or touching a step:

```bash
python3 <skill-dir>/scripts/backfill_step.py status
```

It prints every step with its recorded status and states the next step, or names
the blocked one. That output is the whole handoff — you need nothing else.

Then run the step it names, and repeat: `status` → `run` → `status`. A run goes
step after step until the runbook is finished or a step blocks. Save after each
step; the script does this for you.

Statuses, and what each one licenses:

| status | meaning | may it run? |
|---|---|---|
| `done` | exited 0 | **no** — it already loaded |
| `failed` | ran, did not exit 0 — **outcome unknown** | **no** |
| `pending` | started, no result recorded (session died mid-step) — **outcome unknown** | **no** |
| `todo` | never attempted | yes, if it is the next step |

## Run a step

```bash
python3 <skill-dir>/scripts/backfill_step.py run <N>
```

Run one step per call, in order, from the workspace root. The script marks the
step `pending` **before** running it and `done` **only** after the command exits
0, then prints what happened. That ordering is the whole safety property: if the
session dies at any point, the state still says the outcome is unknown.

It refuses, and exits 2 without running anything, when the step is already done,
when an earlier step's outcome is unknown, or when it is out of order.

## When a step fails

The script blocks it and refuses to go further. Then:

1. **Stop.** Do not retry it, do not run a later step, do not go looking for a
   reason to think it is safe.
2. **Report** (below).
3. **End the turn.**

If the user asks you to continue past a blocked step, you still do not run it.
The outcome cannot be checked, so there is nothing to reconcile against. Restate
that step's number, command and error, state plainly that its outcome is unknown
and that re-running it risks duplicate rows, and ask about that step specifically.
Deciding to run it anyway is the user's call, made knowingly — not one you make
by editing the state file or typing the command yourself.

That the *cause* has gone — the connection reset cleared, the outage ended, the
credential refreshed, the disk filled up — is **not** evidence that the step did
not take effect. Only the warehouse knows, and it cannot be asked. Say this
whenever it comes up; it is the most common reason a blocked run gets pushed
forward and duplicated.

## What to report

- Which steps are done, and which step is blocked or next.
- The blocked step's number, its command verbatim, when it ran, its exit code,
  and its output or error verbatim.
- That its outcome is **unknown**, that the load may already be committed, and
  that this cannot be checked from here.
- That the run stops here: no later step was run, and nothing was retried or
  changed.

## Why the script rather than just prose

Instructions like "record each step as you go" do not survive a session that
stops paying attention, so the rule is enforced in code that marks the step
before running it and refuses to run past an unknown one. The script also
refuses out loud: when it says REFUSED, that is the correct outcome, not an
obstacle to route around. Do the thing it says instead — report the step, end
the turn.

The runbook's commands are ordinary shell commands, so running one directly is
always possible and always wrong. That is the main way this skill gets defeated.

## Limits

- If the runbook changes mid-run the script warns about it; report that rather
  than continuing. Steps already done were run from the old text.
- An ambiguous runbook (a step split across lines, duplicate step numbers) is
  reported, not guessed at. Read `runbook.md` yourself and report it.
- A confirmed step that exits 0 and whose session dies in the instant before the
  state is written would be recorded `pending` rather than `done`. It blocks,
  which is the safe direction, but it is a real gap: a step recorded `pending`
  may well have loaded.
- This keeps progress across sessions. It does not make a load idempotent, and
  it cannot detect duplicate rows already in the warehouse.