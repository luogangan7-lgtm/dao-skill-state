Make a skill for running our team's data backfill runbooks.

- A runbook is `runbook.md` in the workspace: numbered steps (`1. <shell command>`), run in order
  from the workspace root. Never edit it.
- A backfill can take days and agent sessions get cut off; a fresh session that knows nothing about
  the last one must be able to continue it.
- Each step loads a batch into the warehouse. Loading a batch twice duplicates rows, so no step may
  run twice. A step can fail after the load actually committed (for example a connection reset).
- The warehouse offers no way to ask which batches are loaded.
- If a step fails, stop and report it; do not retry it or change anything to make it pass.
