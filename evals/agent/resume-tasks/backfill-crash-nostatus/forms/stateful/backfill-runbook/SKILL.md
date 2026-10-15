---
name: backfill-runbook
description: Run our team's data backfill runbooks (runbook.md). Use when asked to run or continue a backfill.
---
# Backfill runbook

1. Read `runbook.md`: numbered steps, each a shell command, run from the workspace root.
2. Progress lives in `.backfill/progress.json` as `{"done": [<step numbers>]}`; create it if it is
   missing. When continuing, read it first and run only the steps not in `done`.
3. Run the steps in order, one at a time. After a step succeeds, add its number to `done` and save
   the file before the next step. On the first failure stop and report the failing step and its
   message; do not retry it and do not change anything to make it pass.
4. Steps are not idempotent: never run a step that has already run. Never edit `runbook.md`.
5. When done, report which steps ran.
