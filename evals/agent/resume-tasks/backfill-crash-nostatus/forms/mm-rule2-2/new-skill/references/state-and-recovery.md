# State and recovery

Detail behind [SKILL.md](../SKILL.md). Read this when inspecting or repairing a backfill's state,
or when a refusal message needs interpreting.

## Files the helper owns

Everything lives under `<workspace>/.backfill/`, beside the `runbook.md` it describes:

| Path | What it is |
|---|---|
| `.backfill/state.json` | The authoritative record of every step's outcome. Written atomically and fsynced before and after each command. |
| `.backfill/logs/step-NN.log` | That step's combined stdout and stderr, appended, with the command that produced it. |
| `.backfill/lock` | An advisory `flock` held only while a step is running, so two sessions cannot drive one runbook. Released automatically when a session dies; it never goes stale. |

`.backfill/` is derived from the runbook and belongs in version control's ignore list. It is not
part of the runbook, is never edited, and must survive the session that wrote it.

## State file

```json
{
  "schema": "backfill-runbook-state/1",
  "workspace": "/abs/path/to/workspace",
  "runbook": { "path": "runbook.md", "sha256": "…", "step_count": 6 },
  "created_at": "2026-10-14T09:02:11Z",
  "updated_at": "2026-10-19T22:41:03Z",
  "steps": {
    "1": { "status": "done", "started_at": "…", "finished_at": "…",
           "exit_code": 0, "log": ".backfill/logs/step-01.log" },
    "4": { "status": "pending", "command": "load-batch --batch 4 --table sales",
           "runbook_line": 12, "started_at": "2026-10-19T22:40:58Z",
           "outcome": "unknown: the command was started and the result was never recorded" },
    "5": { "status": "failed", "command": "…", "started_at": "…", "finished_at": "…",
           "exit_code": 2, "log": ".backfill/logs/step-05.log" }
  },
  "resolutions": [
    { "step": 3, "previous_status": "pending", "decided_at": "…",
      "note": "Dana checked staging.sales_2026_03 on 2026-10-21, rows present" }
  ]
}
```

`runbook.sha256` is recorded at first use. A runbook edited mid-run means step numbers may no
longer name the same commands, and continuing would load the wrong batch — so the helper refuses
and the run is reported, not reconciled. Fixing the hash by hand would destroy the one check that
catches this.

## Step transitions

| From | Event | To |
|---|---|---|
| not started | `run`, command not yet started | `pending` — written and fsynced **before** the command starts |
| `pending` | command exits 0 | `done` |
| `pending` | command exits non-zero | `failed` |
| `pending` | session dies, is killed, or is interrupted mid-command | stays `pending` — outcome unknown |
| `pending` or `failed` | user confirms out of band that it loaded | `done`, plus a `resolutions` entry naming who decided |
| `done` | — | nothing. No step is ever attempted twice. |

`pending` is a statement that a step was *attempted*, not that it *succeeded*. It is the only safe
place to leave a step whose result was never seen.

## Reading each refusal

| Refusal | What it means | What to do |
|---|---|---|
| `Step N was marked pending … outcome unknown` | A previous session started step N and never recorded a result. | Do not run it. Report step N as needing an out-of-band check and end the turn. |
| `Step N already failed … exit code X` | The command failed; the run stopped there. | Report it. Do not retry, do not change anything to make it pass. |
| `Step N is not the next one to run. Step M is unfinished` | Steps may already be done from earlier sessions — including ones this session never saw. | Run the step the helper names. Never assume the requested number is the right one. |
| `Step N is already done` | It ran in an earlier session. | Continue with the next unfinished step. |
| `runbook.md has changed since this backfill started` | The runbook was edited mid-run. | Report the sha mismatch and show which steps are done. Do not continue. |
| `Another backfill session is running this runbook` | Someone else's session holds the lock. | Run `status` and report; do not force the lock. |
| `state.json … could not be read` / wrong schema / wrong workspace | The one record of what ran is damaged, or a different workspace's state was found. | Report the path and show the file. Never delete or rewrite it — a state file that looks wrong may still be the only evidence of a committed load. |
| `runbook.md has no numbered steps of the form 1. <shell command>` | The runbook is not in the expected form. | Read `runbook.md` and report its actual form. Do not invent steps or guess which lines are commands. |

## Recovery

The only supported recovery is `resolve`, and only from a human finding:

```bash
python3 scripts/backfill_state.py resolve 4 --took-effect --note "who checked, what they saw"
python3 scripts/backfill_state.py run
```

There is deliberately no `--retry`, no `--force`, no `--skip`. Those are the three shortcuts that
turn a stopped run into a duplicated table, and each would leave the state file claiming something
nobody verified.

If `state.json` is lost entirely, no step's outcome is knowable from inside the session — every
remaining step is of unknown status. Report that and let the user reconcile the warehouse by hand
before anything runs again. Do not infer progress from the runbook, from logs, or from what
"looks like it already ran".

## What this setup does and does not guarantee

With the helper driving every step, each step is attempted at most once per runbook, and an
unknown outcome is never resolved silently. It does not make a load atomic or verifiable from the
client side: when a step's outcome is unknown it stays unknown until a human checks, which is why
the run stops there instead of guessing. Exactly-once *loading* is only as good as the warehouse
side of that check.