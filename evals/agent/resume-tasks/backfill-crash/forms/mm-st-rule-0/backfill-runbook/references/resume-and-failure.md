# Resume and failure: worked cases

Each case is a fresh session that knows nothing about the one before it. In every case the answer
comes from the same place: ask the warehouse about every step, then run only what is not loaded.

## Case 1 — Fresh session, run already half done

The runbook has 6 steps. Steps 1–3 committed in a session that ended normally; 4–5 never ran.

The fresh session's only inputs are `runbook.md` and the warehouse. It checks all six, gets
`loaded, loaded, loaded, not loaded, not loaded, not loaded`, and starts at step 4. Steps 1–3 are
skipped, not re-checked "to be sure" by re-running them.

This is the ordinary case, and it is the reason the pre-flight covers *all* steps: the fresh session
cannot know 1–3 were done, so it does not assume — it checks.

## Case 2 — Session was cut off mid-step

A session ran step 4. The load committed. Then the connection dropped and the session ended before
writing `backfill-progress.md`.

`backfill-progress.md` says step 3 was the last completed step. If that were trusted, the next
session would run step 4 again and duplicate a batch that is already in the warehouse.

The check settles it: step 4 reports loaded. Step 4 is skipped and the note is corrected. The
warehouse won; the note was stale. This is why the note is derived and why the note is written
*after* the step, never assumed before it.

## Case 3 — The step command failed after committing

```text
$ ./load_batch.sh orders_2019_q3
Loading orders_2019_q3...
connection reset by peer        # exit 1
```

The exit code says failure. It does not say the batch is absent — the reset can land after the
commit. The only thing that settles it is the check.

- **Check says loaded.** The batch is in. Stop the run and report it, and say plainly that the data
  is present and the step must not run again. The user decides what to do about the failed command
  itself.
- **Check says not loaded.** Nothing was written. Stop the run and report it. This step is not
  retried in this session — the user's rule is that a failure ends the run and gets reported.
- **Check itself fails or is unclear.** The step's status is unknown. Stop, report step N as needing
  a check, and run nothing else — an unknown step is not a step to try.

In all three the run ends here. The report names the step, the error, what the warehouse showed, and
what the next session should do. That is the whole deliverable of a failed step.

## Case 4 — The check command cannot tell loaded from not loaded

The runbook names `./check_batch.sh`, but its output is not documented and two invocations return
text this skill cannot confidently map to a status.

Do not pick the interpretation that unblocks the run. Report that the check's output cannot be
interpreted, show the raw output, and ask the user. A wrong "not loaded" here duplicates rows; a
wrong "loaded" skips a batch. Both are worse than a question.

## Case 5 — Loaded steps are not a prefix

Steps 1–3 and 5 are loaded; step 4 is not.

Proceed from step 4. Do not re-run step 5 to restore the order — it is loaded, and re-running it is
the exact harm this skill prevents. Mention the gap in the run's report so the user can look at it.

## Writing the note

Keep it short and factual: which runbook, when the note was updated, which steps are loaded, and any
step that failed with what the warehouse showed about it. It exists so a human can see where a
multi-day run stopped and so the next session knows what was already reported. It is not evidence
that anything happened, and it never justifies skipping a check.

- Wipe it entirely if the warehouse contradicts it.
- Delete it if the backfill is finished.
- Keep it out of any deliverable; it is a scratch record for the workspace.