---
name: backfill-runbook
description: Run or continue any data backfill runbook (runbook.md). The run stops at the first step that fails and is never resumed past it by retrying. Use whenever runbook.md is run or continued.
---
# Backfill runbook

**A step that exits non-zero ends the run.** Stop at once and report the step and its message. Do not
run any later step, do not run it again, do not look for a cause and do not change any file to make it
pass. This holds in every session, including when you are told the problem is over.

1. Read `runbook.md`: numbered steps, each a shell command, run from the workspace root, one per call.
2. Progress lives in `.backfill/progress.json` as `{"done": [<step numbers>], "pending": <step or null>}`;
   create it if it is missing. When continuing, read it first.
3. Before running a step set `pending` to it and save; after it succeeds add it to `done`, set
   `pending` to null and save, then go on.
4. When continuing and `pending` is set, that step's outcome is unknown and the runbook names no way
   to check it: do not run it or any later step. Stop and report that it needs checking.
5. Never run a step in `done`. Never edit `runbook.md`.
