---
name: backfill-runbook
description: Run or continue our team's data backfill runbooks (runbook.md). Use whenever a backfill is run or continued, including after an interruption.
---
# Backfill runbook

## Purpose and scope

This skill runs the team's data backfills. A backfill is described by a `runbook.md` file at the
root of the workspace. The runbook lists numbered steps; each step is a shell command that loads one
batch of rows into the warehouse. Backfills can be long: they may span several sessions, run across
days, and be interrupted by network failures, expired credentials, maintenance windows, or an agent
session that simply ends. The skill exists so that a session which knows nothing about the previous
one can continue the work safely.

The skill covers starting a backfill, continuing one after an interruption, and reporting what was
done. It does not cover writing or editing runbooks, designing new backfills, cleaning up duplicate
rows after the fact, or changing the warehouse or its configuration. If the user asks for any of
those, say that this skill does not do it and ask how they would like to proceed.

## Background: why steps must not repeat

The steps are not idempotent. Loading a batch a second time does not overwrite the first load; it
appends the same rows again, and the warehouse then holds duplicates. Duplicates are expensive to
find and remove later, they distort every report built on the table, and they can propagate into
downstream systems before anyone notices. For that reason the single most important property of a
backfill run is that every batch is loaded exactly once.

A step can also fail in a way that leaves its outcome unclear. For example, the load may commit in
the warehouse and only then lose the connection before the reply arrives. From the outside such a
step looks failed, yet its rows are already there. Running it again "to be safe" would duplicate
them. The opposite mistake, skipping a step that really did fail, would leave a gap. Neither guess
is acceptable; the only reliable source of truth is the warehouse itself.

## Background: how the warehouse can be asked

The runbook names a status command that asks the warehouse whether the batch for step N is loaded.
It prints `loaded` or `not loaded`. This is the authoritative answer. Local notes, earlier chat
messages, log files and your own memory of what happened are not authoritative, because they can be
out of date, incomplete, or written by a session that was cut off before it could update them.

## How to run the backfill

Run the backfill with this one command, from the workspace root, both when starting and when
continuing:

    python3 <this skill's directory>/scripts/run_checked.py

It goes through the runbook's steps in order. For each step it first asks the warehouse whether that
step's batch is already loaded (the status command the runbook names) and skips it if so; otherwise it
runs the step. It stops at the first step that fails. Run it again after a failure is over: it checks
every step again, so nothing is loaded twice.

## What the output means

- `SKIP step N: already loaded` means the warehouse already has that batch; nothing was run.
- `step N: ...` shows the output of a step the script ran because the batch was not loaded yet.
- `STOPPED at step N (exit C)` means that step failed. The script stops there so that later steps do
  not run on top of a problem. Its batch may or may not be loaded; the next run will ask the
  warehouse and decide.
- `DONE: every step is loaded` means every step's batch is in the warehouse.

## When something fails

When the script stops, read the step's message. Common causes are a network failure between the
workspace and the warehouse, an upstream service that is down, or a temporary error in the
warehouse itself. Report the failing step and its message to the user. Do not try to make the step
pass by changing files, environment settings, or status markers; do not edit the step command; and
do not run the step yourself. When the user says the problem is over, run the same command again.
Because the script asks the warehouse about every step before running it, continuing is always the
same command, no matter where the previous session stopped or why.

## Rules that always apply

- Do not run the runbook's step commands yourself. Running a step directly bypasses the check and is
  the usual way batches end up loaded twice.
- Never edit `runbook.md`. The runbook is owned by the team that wrote it; if it looks wrong, report
  what you see.
- Do not delete, rename or edit files in the workspace to influence a step.
- Do not guess what a previous session did. Let the script ask the warehouse.

## Reporting

When you finish, report the script's output: which steps were skipped because they were already
loaded, which steps ran, and whether the run completed or stopped. If it stopped, include the failing
step and its message, and say that running the same command again after the problem is resolved will
continue safely. Keep the report factual; do not claim steps ran unless the output shows it.

## Troubleshooting

- If the script cannot find `runbook.md`, make sure you are in the workspace root.
- If the status command fails, report its message; the script cannot decide safely without it.
- If the run takes long, wait for it to finish; do not start a second copy in parallel.
