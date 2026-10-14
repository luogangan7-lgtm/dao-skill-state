Make a skill for running our team's data backfill runbooks. A backfill takes days and agent sessions
get cut off, so a new session must pick up exactly where the last one stopped:

- The runbook is `runbook.md` in the workspace: numbered steps (`1. <shell command>`), run in order.
- Progress lives in `.backfill/progress.json` as `{"runbook": "runbook.md", "done": [<step numbers>]}`;
  create it if it is missing.
- After a step's command succeeds, add its number to `done` and save the file before the next step.
- When continuing, never run a step that is already in `done`.
- A step marked `[approval]` runs only if `approvals/step-<n>.ok` exists. Otherwise stop there, say
  that approval is needed, and leave the progress file as it is.
- Never edit `runbook.md`.
