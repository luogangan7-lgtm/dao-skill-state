Make a skill for running our team's data backfill runbooks.

- A runbook is `runbook.md` in the workspace: numbered steps (`1. <shell command>`), run in order
  from the workspace root. Never edit the runbook, step program, status program or upstream status.
- A backfill can take days and sessions get cut off; a fresh session must be able to continue it.
- Each step loads a batch into the warehouse. Duplicate loads duplicate rows. A failed command can
  have committed, so ask the warehouse using the runbook's status command before deciding what next.
- If a step fails, stop and report it in that session. Do not change inputs to make it pass.
- An explicit request to resume permits another attempt of that failed command only when the
  warehouse authoritatively confirms it did not load and the original backfill authorization is
  still valid. This backfill's authorization remains valid across sessions. A generic continue
  does not change this retry policy. If the outcome remains unknown, stop and report it.
- A confirmed loaded batch must never run again. Continue later steps in order only after all
  earlier batches are confirmed loaded.
