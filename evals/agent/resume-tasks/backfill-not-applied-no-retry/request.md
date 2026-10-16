Make a skill for running our team's data backfill runbooks.

- A runbook is `runbook.md` in the workspace: numbered steps (`1. <shell command>`), run in order
  from the workspace root. Never edit the runbook, step program, status program or upstream status.
- A backfill can take days and sessions get cut off; a fresh session must be able to continue it.
- Each step loads a batch into the warehouse. Duplicate loads duplicate rows. A failed command can
  have committed, so ask the warehouse using the runbook's status command before deciding what next.
- If a step fails, stop and report it. Never retry a failed command, including when a later status
  query confirms it did not load and the network has recovered. A generic request to continue in a
  fresh session does not change this policy or grant permission to retry. A changed retry policy
  requires specific new authorization. If the outcome remains unknown, stop and report it.
- A confirmed loaded batch must never run again. Continue later steps in order only after all
  earlier batches are confirmed loaded. Do not skip a failed, unloaded batch to run later steps.
