# Agent evaluations

Does a skill help a real agent do a task, and can a meta-skill create or evolve skills that work?
These evaluations run Claude Code (CLI 2.1.285, headless) in GitHub Actions against public tasks
with programmatic checkers. Nothing runs on a developer machine.

| Suite | Script | Question |
|---|---|---|
| `run` (C) | `run.py` | Does loading a skill change task success? Arms: `none`, `skills` (installed), `hook` (installed + skill-hub hook) |
| `create` (B) | `create.py` | Does a meta-skill make better skills? Creator arms `none`, `skill-creator` (anthropics/skills, pinned), `dao` (this repo's `dao-skill-state`); the created skill is then used on tasks whose prompts never restate the rules |
| `evolve` (D) | `evolve.py` | Can a meta-skill fix a skill from one observed failure without breaking it? The harness replays the fail case, a case that already worked and a held-out case with the original skill and with the candidate |
| `resume` (E, F) | `resume.py` | Fixed steps or a stateful skill? Session 1 is interrupted mid-task; a fresh session 2 must finish without repeating a non-idempotent step. Arm `make:<creator>` lets a meta-skill write the skill first |

Run: Actions → `agent-eval` → Run workflow (`suite`, `tasks`, `arms`, `reps`, `provider`). The
workflow needs an Anthropic-compatible key as a repository secret (`KIMI_API` or `MINIMAX_API`
for the providers wired in). On pull requests it checks that every legacy checker fails on an
empty workspace and runs the Rust formatting, lint, tests and recovery gate calibration.

## Results (2026-10-14)

All numbers are recorded CI runs (evidence E3) **for Claude Code driven by third-party models**
(Kimi K3, MiniMax-M3), not Claude models. Small samples: treat differences of one or two runs as
noise.

### A. Making third-party skills meta-skill ready (`intake-bench.yml`, no model)

112 skills from five pinned public repositories (anthropics/skills, openai/skills,
huggingface/skills, vercel-labs/agent-skills, obra/superpowers) through the `dao` supply chain
(intake → scan → normalize → relations → eval → admit). 80 admitted at first; the benchmark found
scanner false positives (`printenv`/`env` and `crontab` matched inside words), a query-stopword
gap in routing and an unclear same-name rejection. After those fixes (#30, #31, #39): **86/112
admitted**; the rest are rejected with a stated reason.

**A2 — security scan, labeled set** (`labeled_bench_attack_variants_and_benign_look_alikes` in
`crates/dao/src/scan.rs`, runs in CI). Synthetic attack variants mostly not in the other scan tests,
plus benign look-alikes. Before #49 the scanner caught 16/23 attacks (missed command/process
substitution of downloads, `exec(urlopen(…))`, a PowerShell home wipe, "do not mention this step to
the user", and a prose instruction to send a private key to a URL) and flagged 0/15 benign. After:
**26/26 attacks (3 variants added during review), 0/27 benign**; on the 112 real skills the
verdict counts are unchanged (CLEAN 100 / REVIEW 11 / UNRESOLVED 1, 86 admitted). The
attack variants are ours, so this is recall on known shapes, not on unseen attacks.

**A3 — normalization fidelity** (`crates/dao/tools/card_fidelity.py`, a gate in `intake-bench`).
Every routing-card field must be copied from the skill: name/description equal the YAML values
(trimmed), sentences are parts of the description, headings are `## ` lines, invocation entries are
frontmatter lines. On the 112 real skills: **0/111 violations**; the first run's 2 differences were
a trailing newline normalize trims on purpose, now stated in its contract (#50).

**A3 — relations**, hand-labeled on the same run (54 relations): drift 1/1, family 8/8 and
name_conflict 2/2 correct; the one near_duplicate (vercel `writing-guidelines` /
`web-design-guidelines`) is a shared body template for different domains — true as defined, not a
duplicate skill; of 42 references, 3 are false (names openai's `skill-creator` uses only as
examples: `docx`, `brand-guidelines`, `gh-address-comments`) and the other 39 are cross-references
inside skill families (superpowers, figma, hf-cloud), not each verified. Recall is not measured: the
set has no translations and no labeled list of relations that should exist. Hand labels are
evidence E2 (one reviewer, no second rater).

### C. Skill effect (`run`, Kimi, run 38525090200, 8 tasks × 3 reps)

| Arm | Pass | Runs that opened a skill |
|---|---|---|
| none | 24/24 | – |
| skills (installed) | 22/24 | 6/24 |
| hook (installed + skill-hub hook) | 20/24 | 16/24 |

- Without any skill the agent passed everything: these office/TDD/debug tasks are within its reach.
- The hook surfaced the right skill in 24/24 runs; installed skills alone were rarely opened.
- Skills cost turns (about 1× on simple tasks up to 3.4–3.9× on pptx and pdf-form) and **lowered**
  the pass rate here: the pptx skill's own conventions overrode exact user specs (skills 1/3, hook
  0/3) and docx-tracked-change dropped to 2/3 with the hook.

Conclusion: the routing hook is what makes installed skills get used; a skill only pays off when
the task is beyond the model's defaults, and a skill whose conventions conflict with the request
can make results worse.

### B. Skill creation (`create`, 3 tasks)

| Run | Model | reps | Created (valid): none / skill-creator / dao | Mean create turns |
|---|---|---|---|---|
| 38565313467 | MiniMax | 3 | 9 (7) / 9 (6) / **6 (6)** of 9 | 5.3 / 5.9 / 16.3 |
| 38588702838 (after #43, dao arm only) | MiniMax | 3 | – / – / **9 (9)** of 9 | – / – / 18 |
| 38581453943 (partial: Kimi quota ran out) | Kimi | 2 | 6/6 / 5/6 / 4/5 (2 refused, 1 not run; no dao csv-cleaning result) | 3.7 / 4.2 / 10.6 |

"Valid" = passes dao-skill-state's format lint. The rerun after #43 covers only the dao arm.

- Baseline (usage tasks with no skill): **0/3** — the team rules cannot be guessed, so passing
  usage proves the created skill carries them.
- Using the created skills (MiniMax 38565313467): with the hook, usage passed every time (9/9, 9/9, 6/6 per
  creator arm); with the skill only installed, 8/9 (none), 5/9 (skill-creator) and 6/6 (dao) —
  use depends on the agent choosing to open it. The post-fix dao rerun: 8/9 with skills, 8/9 with
  the hook.
- Defect found and fixed: dao sometimes wrote `./new-skill/<name>/SKILL.md` (nesting a folder to
  satisfy its own folder = name rule) and did not say so. #43 fixed the entry rule; rerun 9/9.
- dao is the slowest creator (2–3× the turns), often adding scripts and self-checks.

### D. Self-evolution (`evolve`, tasks with a seeded defect)

| Run | Model | reps | Candidates fixed (none / skill-creator / dao) | Regressions | Held-out passes |
|---|---|---|---|---|---|
| 38559638817 | MiniMax | 3 | 5/6 / 6/6 / 6/6 | 0/18 | 18/18 |
| 38575435595 | Kimi | 2 | 4/4 / 4/4 / 4/4 | 0/12 | 12/12 |

All arms fix the observed failure without breaking prior successes (one `none` candidate on
MiniMax did not fix it). The held-out case fails with the original skill in changelog-perf (and
in csv-dates on Kimi) and passes with every candidate, so the fixes generalise beyond the shown
case.
These two seeded defects are easy enough that no meta-skill is needed, so they do not separate the
arms.

**Over-fix trap (`csv-region`).** The failure shows one EU export whose slashed dates are day
first; the reviewer says nothing about other exports. Switching every slashed date to day first
fixes it but breaks the US exports: `old-ok` (its request is shown to the evolver) and
`heldout-us` (never shown). Clean = fixed, no regression, held-out passes.

| Run (MiniMax) | reps | Clean: none / skill-creator / dao | Mean evolve turns |
|---|---|---|---|
| 38598558955 | 3 | 2/3 / 0/3 / 3/3 | 5 / 10 / 12 |
| 38602257736 | 5 | 5/5 / 2/5 / 5/5 | 7 / 10 / 15 |
| 38608445681 (skill-creator only, candidates kept) | 3 | – / 3/3 / – | – / 9 / – |
| **Total** | 8 / 11 / 8 | **7/8 / 5/11 / 8/8** | |

- skill-creator's candidates broke the unseen US case in 5/11 (and the EU held-out once). The use
  runs' replies suggest swapped columns, a misspelt header or flipped US dates; those candidates
  were not kept, so that cause is a hypothesis. The three kept candidates (all clean) show the
  style: they add rules the evidence did not give (a "first component > 12 means day first"
  heuristic, a rewritten section order), which a weaker reading can turn into regressions.
- dao vs skill-creator 8/8 vs 5/11: two-sided Fisher exact p ≈ 0.02, pooled over three runs, one
  task, MiniMax only. No difference was observed between dao and no meta-skill (7/8; p ≈ 0.15 vs
  skill-creator). The `none` miss was a model glitch (the reply was a raw tool-call string).
- An earlier run (38594726481) used a prompt that only said "write the improved skill into
  ./candidate/"; 3/9 evolvers (none 1, dao 2) put it in `./candidate/<name>/` and scored "no
  candidate". The prompt now names `./candidate/SKILL.md` (#45); that run is excluded above.
- Candidates are now kept in the run artifact (`candidates/`) so a regression can be traced to
  the rule that caused it.

### E. Fixed steps vs state on an interrupted task (`resume`, MiniMax)

An 8-step backfill whose loads must not repeat. Session 1 is cut off; a fresh session 2 continues.
Each form is a hand-written runbook with the same rules; `stateful` adds a progress file,
`stateful-pending` also marks a step pending before running it and, on resume, settles a pending
step against the real system first; `fixed-check` is fixed steps plus "before rerunning a step that
may have run, check the system".

| Task (run) | What is hard | none | fixed | fixed-check | stateful | stateful-pending |
|---|---|---|---|---|---|---|
| backfill-interrupted (38685135055) | outage at step 5; audit log visible | 4/5 | 5/5 | – | 5/5 | – |
| backfill-external (38685410387) | progress only in an external store | 3/5 | 5/5 | – | 5/5 | – |
| backfill-crash (38694187804, 38697589642) | step 5 commits, then "connection reset"; a status query exists | – | 5/5 | 5/5 | **0/5** | 5/5 |
| backfill-crash-nostatus (38697639318) | same crash, no status query; pass = nothing loaded twice | – | 3/3 | 3/3 | **0/3** | 3/3 |

- A progress file that records only finished steps is worse than none: after the crash it says
  step 5 is not done, the agent trusts it and loads step 5 again (8/8 runs).
- The pending marker fixes that; so does a fixed runbook that says to check the system. With a
  status query both finish. Without one, `fixed-check` and `stateful-pending` both say "if the
  runbook names no way to check, do not rerun: stop and report"; `stateful-pending` did so in 3/3,
  `fixed-check` in 2/3. Pass there means nothing was loaded twice, so stopping passes. Of the four
  `fixed` / `fixed-check` runs that finished 6-8, three read the stand-in store's log files
  directly (probing a system the runbook never named) and one skipped step 5 unverified and
  flagged it afterwards (per-run rows in each run's `resume.jsonl`).
- Routing (run 38703260705, arms `hook:<form>`): the prompts do not name the skill and five
  third-party skills (executing-plans, subagent-driven-development, …) are installed beside it.
  The skill-hub hook picked `backfill-runbook` in 12/12 sessions (lexical path; both prompts
  say "backfill", so this is the easy case) and every session 2 loaded it. `stateful-pending` 3/3;
  `fixed-check` 2/3: in the miss, session 1 had gone on to finish 1-8, and session 2 checked
  steps 1-5 only (`status.py 1`…`5` in that row's `s2.bash` in `resume.jsonl`) and loaded 6-8
  again. One run (n=1), but it shows the gap: without a record of where the last session stopped,
  a fixed runbook checks only what it assumes is unfinished.
- Cost is similar (fresh tokens per run 35–60k); `stateful-pending` spends more turns in session 1
  on bookkeeping and fewer in session 2.
- Consequence for skills: choose `stateful` only with the full contract (pending action + settle
  before rerun), see `dao-skill-state/references/state-contract.md`; a partial one is a regression.

### F. Does a meta-skill pick the right form? (`resume`, arm `make:<creator>`, MiniMax)

The creator writes the backfill skill from `request.md`, which describes the situation only;
the two crash tasks differ in one line ("the warehouse can be asked whether a batch is loaded" /
"offers no way to ask"), so by the form standard the right form differs: fixed steps checking every
step, or stateful with a pending marker. Both sessions then run with the created skill. Created
skills are kept in each run's artifact (`created/`); the form was read from them.

| Round (runs) | creator | status task pass | no-status task pass | form chosen |
|---|---|---|---|---|
| 1 (38720176111, 38720180129) | none | 3/3 | 3/3 | mostly a progress file in both tasks; never declared |
| 1 | skill-creator | 2/3 | 2/3 | same |
| 1 | dao | 2/3 | 3/3 | declared and as the rule says 6/6 |
| 2-4 (38729806153, 38731985167, 38731994978, 38738110671, 38738121250) | dao, after each authoring fix | 13/15 | 9/10 | as the rule says except 3 status-task skills that chose stateful anyway (they still checked the warehouse; 2 passed, 1 refused the runbook) |
| 5 (38770795491) | dao, after the open-input rule | 5/5 (2 with work left to resume) | 5/5 | as the rule says 10/10 (5 fixed + check; 5 stateful with a pending/in-flight marker, 4 of them stopped safely at the unknown batch) |

- No dao-made skill loaded a batch twice (0/41 runs). Without a meta-skill one run did (round 1,
  no-status task: a progress file without a pending marker).
- Runs that read the stand-in store directly instead of through `status.py` (status task, round 1):
  none 3/3, skill-creator 3/3, dao 0/3; later dao rounds 0/5, 1/5, 3/5, 1/5.
- Every dao failure was a refusal, never a wrong load: the skill demanded an input format the request
  never gave (a `## Check command` heading, YAML frontmatter, an exit-code convention for the status
  command) and stopped; once the session-2 agent edited `runbook.md` to fit it. The fixes added to
  authoring.md (take inputs as the request describes them; test a created skill on input written that
  way) lowered but did not remove this. Round 5 added one more: when the request leaves an input's
  format open, the skill has the agent read it directly and writes a parser only for a fixed format;
  a helper that finds nothing is not proof of absence. No refusal was observed in round 5 (10/10).
  The open input here is how a runbook names its check command (the step format is fixed by the
  request, so parsing steps is allowed); only the status task exercises it. In 3 of the 5 status-task
  runs session 1 had already finished all 8 batches, leaving nothing to resume. With n=5 and no
  control arm this is not evidence that the rule caused the change, and it does not rule the defect out.
- "One step per session" (a skill that stops after each step) appeared in skills made without a
  meta-skill and by dao; the form standard now says state does not limit a run to one step.
- Rounds ran on successive commits of the branch; n=3-5 per cell, one model, one task family.

Other models (same tasks and arms, n=3 per cell, commit of #63; DeepSeek run 38778472384,
`deepseek-flash`; Kimi run 38778479822, `k3`, stopped by the subscription's monthly limit after 14 of
18 runs, so Kimi has no dao row on the no-status task and n=2 for skill-creator there):

| model | creator | status task pass | no-status task pass | form on the status task (rule: fixed + check) | runs that probed the store (status task) |
|---|---|---|---|---|---|
| DeepSeek | none | 3/3 | 3/3 | progress file 3/3 | 3/3 |
| DeepSeek | skill-creator | 3/3 | 3/3 | progress file 3/3 | 3/3 |
| DeepSeek | dao | 2/3 | 3/3 | as the rule says 2/3 (1 declared stateful) | 2/3 |
| Kimi | none | 3/3 | 3/3 | progress file 3/3 | 0/3 |
| Kimi | skill-creator | 2/3 | 2/2 | progress file 3/3 | 0/3 |
| Kimi | dao | 3/3 | not run | as the rule says 3/3 | 0/3 |

- On the no-status task every created skill on both models kept state with a started/pending marker,
  which is what the rule gives there; no run on either model loaded a batch twice.
- Only dao chose fixed steps where the warehouse can be asked (5 of 6); skills made without it or with
  skill-creator kept a progress file every time; pass rates were no worse on these tasks (11/12 vs
  dao 5/6, n small), though the one silent wrong result below came from a progress-file skill.
- The failures differ in kind. Kimi skill-creator, rep 2: its helper took the check command's
  exit status (always 0) as "loaded" and ignored its output, recorded all 8 batches as loaded and reported the backfill complete without loading any
  (a silent wrong result). DeepSeek dao, rep 2: the skill wrote a stop marker on the failed step that
  "only a human clears" and so refused the user's "continue" in session 2 without asking the warehouse,
  which could have been asked; a gate the skill invented for itself (n=1, not yet a rule).
- Probing the store depends on the model, not on dao: on MiniMax only dao did not probe, on Kimi no
  arm probed, on DeepSeek every arm did (dao 2/3).

### Limits

- Third-party models only; results can differ with Claude models.
- 2–3 repetitions per cell; no significance claims between arms.
- Tasks are public and small by design so anyone can rerun them.
# Rust recovery boundary evaluation

The `resume-rust` suite runs through the existing Rust `dao` CLI. It does not
change the existing frozen legacy Python suites. Dispatch `agent-eval` with
`suite=resume-rust`, `arms=make:dao`, `reps=2` and an empty task selection for all
six cases: applied crash, unknown result, retry allowed, retry forbidden, real
approval missing and user paused. The generated skill must use Rust for any
persistent helper; no generated Python is accepted.

In GitHub CI, build the locked `crates/dao` crate, then run:

```sh
dao eval resume run --repo "$PWD" --out results --reps 2
dao eval resume check --repo "$PWD" --out results --reps 2
```

Each case has fresh maker/session HOME directories, an external Rust mock and
attempt/load/query records. The producer binds HEAD, binary, policy and fixture
source hashes, task selection, budgets and GitHub run identity. Every phase must
finish cleanly, load the required skill, preserve inputs and match the independent
event gate. Missing, stale, truncated or failed evidence is red. The first known
red stops further model spending and saves its evidence; unrun rows stay NOT_RUN.
Acceptance requires the complete 6×2 matrix and independent raw trace review.
Trace inspection is bounded observation, not a security sandbox.

For NVIDIA's free hosted endpoints, use `provider=nvidia`, an explicit model
from the workflow's frozen cohort, and `evidence_cert` containing the owner's
PEM public recipient certificate. Keep the matching private key outside GitHub.
The pinned local protocol adapter preserves the same Claude Code skill host and
Rust matrix. Each model runs separately, with its documented reasoning settings
recorded in `provider.json`; those settings differ, so results are not a controlled
estimate of model-only gains. Ordinary gateway retries and Claude retries are
configured off; the adapter has a known independent 401 auth-refresh attempt. Error
retry counts are unmeasured. NVIDIA raw artifacts, stdout and diagnostics are
CMS-encrypted for internal review. The public artifact contains the ciphertext,
recipient fingerprint and numeric acceptance metadata; encryption failure has no
plaintext fallback. A protocol probe alone cannot pass the behavioral matrix.

The additional `nvidia/nemotron-3.5-super-vl-120b-a12b` candidate now uses
`reasoning_effort=low` and the unchanged 32,000-token output cap. NVIDIA's
[hosted API reference](https://docs.api.nvidia.com/nim/reference/nvidia-nemotron-3_5-super-vl-120b-a12b-infer)
(checked 2026-10-17) documents that mode, tools, SSE and the supported output
range; its [model page](https://build.nvidia.com/nvidia/nemotron-3.5-super-vl-120b-a12b)
lists the free endpoint as available. This is a controlled configuration change
from `none` at `e891789`: DAO contents, task prompts, helpers, fixtures, gates,
host, adapter and phase budgets remain fixed. The earlier timeout and rule
violations remain evidence; reasoning mode is a hypothesis, and no speed or
capability improvement is assumed before the complete matrix passes.

The native protocol probe uses that same `low` setting for Super 3.5 VL,
without a conflicting `enable_thinking=false` override. It reports HTTP status
and exact tool-argument matches only; native probe success does not validate
the Claude adapter or any DAO behavior.

The PR check freezes all `.py` files against the base, runs Rust gate calibration
and exports formatting patches from CI when needed. No local execution is part
of this task's acceptance.

