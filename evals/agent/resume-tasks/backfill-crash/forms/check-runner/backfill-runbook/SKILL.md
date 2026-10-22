---
name: backfill-runbook
description: Run or continue our team's data backfill runbooks (runbook.md). Use whenever a backfill is run or continued, including after an interruption.
---
# Backfill runbook

Run the backfill with this one command, from the workspace root, both when starting and when continuing:

    python3 <this skill's directory>/scripts/run_checked.py

It goes through the runbook's steps in order. For each step it first asks the warehouse whether that
step's batch is already loaded (the status command the runbook names) and skips it if so; otherwise it
runs the step. It stops at the first step that fails. Run it again after a failure is over: it checks
every step again, so nothing is loaded twice.

Do not run the runbook's step commands yourself. Never edit `runbook.md`. Report the script's output.
