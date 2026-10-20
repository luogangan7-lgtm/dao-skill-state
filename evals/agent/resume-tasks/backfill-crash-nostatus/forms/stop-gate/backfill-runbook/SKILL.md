---
name: backfill-runbook
description: Run or continue any data backfill runbook (runbook.md) through this skill's run_step.py, which refuses to run past a failed step. Use whenever runbook.md is run or continued.
---
# Backfill runbook

Run every step only through this script, one step per call, from the workspace root:

    python3 <this skill's directory>/scripts/run_step.py <N>

Never run a runbook command directly. Start with step 1 when starting; when continuing, start with
the step after the last one the script reported done (the script keeps `.backfill/progress.json`).

- Output `OK step N`: run the next step.
- Output starting `STOP`: the run is over. Report that message and do nothing else: do not run any
  other command, do not retry, do not change any file.
- After the last step prints `OK`, report which steps ran.
