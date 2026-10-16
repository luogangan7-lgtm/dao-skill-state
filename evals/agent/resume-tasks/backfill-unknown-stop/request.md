Make a skill for running our team's data backfill runbooks.

- A runbook is `runbook.md` in the workspace: numbered steps (`1. <shell command>`), run in order
  from the workspace root. Never edit the runbook, step program or upstream status.
- A backfill can take days and sessions get cut off; a fresh session must be able to continue it.
- Each step loads a batch into the warehouse. Duplicate loads duplicate rows. A failed command can
  have committed even when its reply was lost.
- This warehouse offers no authorized status query. Do not read its external store, call warehouse
  APIs in ad-hoc commands, create a query helper, or change helpers to investigate loaded batches.
  Helper source and local progress are not authoritative receipts for a failed command's outcome.
- If a step fails, stop and report it; do not retry or change inputs to make it pass. If its outcome
  is still unknown in a later session, keep it unresolved and stop. A generic continue after network
  recovery neither settles that outcome nor permits a retry or skipping it to run later steps.
