---
name: backfill-runbook
description: Run our team's data backfill runbooks (runbook.md). Use when asked to run or continue a backfill.
---
# Backfill runbook

1. Read `runbook.md`: numbered steps, each a shell command, run from the workspace root.
2. Progress lives in `.backfill/progress.json` as `{"done": [<step numbers>], "pending": <step or null>}`;
   create it if it is missing. When continuing, read it first and run only the steps not in `done`.
3. Before running a step set `pending` to it and save; after it succeeds add it to `done`, set
   `pending` to null and save, then go on. On the first failure stop and report the failing step and
   its message, leaving `pending` set; do not retry it and do not change anything to make it pass.
4. When continuing with a `pending` step, its outcome is unknown: check the real system for that
   step first (the runbook names how) and run it only if it did not take effect.
5. Steps are not idempotent: never run a step that has already run. Never edit `runbook.md`.
6. When done, report which steps ran.
