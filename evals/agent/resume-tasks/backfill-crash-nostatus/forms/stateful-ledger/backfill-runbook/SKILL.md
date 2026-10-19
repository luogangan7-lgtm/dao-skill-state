---
name: backfill-runbook
description: Run our team's data backfill runbooks (runbook.md). Use when asked to run or continue a backfill.
---
# Backfill runbook

Read `runbook.md`: numbered steps, each a shell command, run from the workspace root. Steps are not
idempotent. Never edit `runbook.md`. L is `python3 <this skill's dir>/scripts/ledger.py .backfill/ledger.json`.

For each step N, in order:
1. Run `L begin N`.
   - exit 3: N is done; go to the next step.
   - exit 4: N was cut off earlier. Ask the system whether it took effect if the runbook names a way; if it names none, stop and report the step; then run `L resolve N done` or `L resolve N not-done` and
     run `L begin N` again. Never run N again unchecked.
   - exit 0: run step N.
2. If N succeeded, run `L done N`. If it failed, stop and report N and its message; do not retry.

When done, report which steps ran.
