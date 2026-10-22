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
for the providers wired in). On pull requests it only checks that every checker fails on an
empty workspace.

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
- A second case on main (run 38795566333, DeepSeek, dao, status task, 4/5): rep 3 made `failed`
  terminal and required `reopen --authorized-by "<name>"`. Two cases in 8 runs made it a rule
  (authoring: "Do not invent gates either"). Replay on that rule (run 38799698815, DeepSeek, dao, n=5
  per task): status task 5/5, 0 refusals of "continue" (3 resumed after a clean stop, 2 finished in
  session 1); no-status task: 5/5 stopped safely at the unknown batch, 0 double loads. There the
  human-resolve stop is what the rule keeps, since the batch cannot be checked. A replay, n=5, no control, not causal.
- Same replay on MiniMax after the merge (run 39198153129, `MiniMax-M3`, main at #67, dao, n=5 per
  task): status task 5/5, 0 refusals, 0 reruns (2 resumed after a clean stop, 3 finished in session 1),
  no human-only marker in any created skill; no-status task: 5/5 stopped safely at the unknown batch,
  0 double loads. The rule held on a second model; still n=5, no control.
- A weaker free model, NVIDIA `nemotron-3-super-120b-a12b` (provider `nvidia`), both tasks; controls
  only in the first run:

  | run | dao text | arms | pass |
  |---|---|---|---|
  | 39205391856 | main at #68 | none / skill-creator / dao, n=3 per task | 1/6 · 1/6 (2 made no skill) · 1/6 |
  | 39226064503 | + pending/check rule inlined in authoring.md | dao, n=5 per task | 3/10 |
  | 39246989882 | main at #68, logs which files the maker reads (#70) | dao, n=3 per task | 2/6 |
  | 39261109764 | + the same rule in the entry SKILL.md | dao, n=5 per task | 1/10 (3 made no skill) |

  dao showed no gain over the controls (1/6 each). The invented-gate rule held on MiniMax and DeepSeek
  but nothing here suggests it reaches this weaker model. Most created skills did not mention a pending
  mark (1 of 10 with the rule in authoring.md, 3 of 7 with it in the entry, one of which passed). In
  39246989882 all 6 makers wrote and tested a Python runner and 5 of 6 used 37–41 of their 40 turns
  (the cap allows one over); 4 of 6 read only the entry. Stating the rule in the entry did not clearly
  change what was written. Pass rates of 1/6–3/10 are within noise at these n; neither rule placement
  is merged.
- Is it the writing or the running? The same model given the hand-written forms (run 39316556840,
  main, n=3 per task): status task none 0/3, fixed-check 2/3, stateful-pending 3/3; no-status task
  fixed-check 0/3, stateful-pending 0/3 (none 2/3, both runs where session 1 loaded all 8 batches and
  so never hit the cut-off). With a status query a correct hand-written skill passed 2/3 and 3/3, so the gap
  there looks like the maker's writing. Without one even a correct skill does not hold: session 1 stops cleanly at
  the pending batch, then session 2 loads it again. Its final replies cite the network status file
  reading "up", apparently taking the cut-off as fixed and the batch as not loaded.
- Tried on that model, none kept (dao arm unless noted; status / no-status pass):

  | run | variant | pass |
  |---|---|---|
  | 39278958103 | dao ships `ledger.py` (begin/done/resolve; exit 4 = pending, check or stop) | 0/5 · 1/5 |
  | 39296706971 | the same plus a workflow block to paste as is | 0/5 · 1/5 |
  | 39333512246 | hand-written form calling the ledger, vs stateful-pending | 2/5 vs 4/5 · 0/5 vs 0/5 |
  | 39337815413 | hand-written stateful-pending plus "the cause being gone says nothing about whether the step took effect" | 2/5 · 1/5 |
  | 39365404803 | dao entry: one path per action, no runner for commands the agent can run | 0/5 · 1/5 |

  Makers copied the ledger into 10 of 17 skills but pasted the workflow block into one. In the 21 no-status
  runs with a hand-written form, no session 2 stopped at the unverifiable batch: 20 loaded it again
  or never hit the cut-off, one marked it loaded with no way to check. None of the variants tried
  moved it on this model; n=3–5 per cell.
- Where the status-task runs went wrong (dao arm, 4 runs, 15 runs that made a skill, from
  the session Bash logs; small n, descriptive only): the agent ran steps by hand and then also the
  runner the maker wrote in 8 (1 passed; failing ones show loads such as 1..5 then 1..5 again right
  after the runner started); by hand only in 5 (3 passed); runner only in 2 (0 passed). A rule
  against the second path (39365404803, row above) was not followed (7 of 10 skills still shipped a
  runner), and the by-hand-only status runs there passed 0/3 (one went on to step 35), so the split
  above is not causal.
- A stronger NVIDIA model, `nemotron-3.5-super-vl-120b-a12b` with `reasoning_effort: high` (run
  39346350449, main at #74, n=3 per task), status / no-status pass: make:none 1/3 · 3/3, make:dao
  2/3 · 3/3, stateful-pending 3/3 · 3/3. Every dao no-status run stopped cleanly at batch 5 and
  loaded nothing twice. make:none also passed 3/3 by the checker, but only one of those stopped at
  batch 5; the other two continued to batch 8 after the failed step 5. The stop that no skill moved
  on Nemotron-3-Super is reached here with dao, consistent with the gap following model capability
  (one stronger model, reasoning effort also differs; n=3, controls in the same run).
- More on weak models, no-status task unless noted (pass · runs that stopped at batch 5 and loaded
  nothing twice; small n, descriptive only):

  | run | model | skill | pass · stopped |
  |---|---|---|---|
  | 39378666791 | Super | dao, with check_skill erroring on a shell-running script and on progress without "pending" | status 2/5, no-status 0/5 |
  | 39384563904 | Super | written by MiniMax+dao (run 39198153129), 2 skills | 3/6 · 3 |
  | 39384563904 | Super | hand-written stateful-pending | 0/3 · 0 |
  | 39388744697 | Super | hand-written, step 4 an unconditional stop | 2/5 · 1 |
  | 39394182506 | Lightning | stateful-pending / unconditional stop / plus "the cause being gone is not evidence" | 0/5 · 0/5 · 1/5, none stopped |
  | 39394182506 | Lightning | written by MiniMax+dao (one skill) | 2/5 · 0 |

  With the checker gate, 5 of Super's 8 skills mentioned pending (1 of 10 with the rule only in
  authoring.md, run 39226064503) and 4 of 8 shipped a runner, yet no-status stayed 0/5. Skills a
  stronger model wrote with dao passed more often on the two weak executors (5 of 13 valid runs, 3
  of them stops at batch 5; a replicate on Super was cut short when the endpoint began refusing that
  model) than the short hand-written forms (4 of 44 no-status runs across these and earlier runs).
  Which part of those skills matters is not isolated; an unconditional stop alone did not reproduce
  it on Lightning. Lightning is `nemotron-3.5-lightning-30b-a3b` without thinking (#79).
- Ablating that skill (`mm-dao-a`) on Lightning, no-status task, 5 runs per arm (run 39405633274;
  arms on branch `exp/nvidia-ablate`). Each arm drops one part: the stop restated in the
  description, the terminal `failed` status, the two "state file is the only authority" sentences,
  or three sections (943 → 686 words).

  | arm | pass | session 1 loaded exactly 1-5 |
  |---|---|---|
  | unchanged | 0/5 | 1/5 |
  | no stop in description | 1/5 | 0/5 |
  | no `failed` status | 0/5 | 3/5 |
  | no authority sentences | 0/5 | 1/5 |
  | shorter | 2/5 | 3/5 |

  The unchanged skill passed 0/5 here and 2/5 in run 39394182506, so the arm differences (0 to 2 of
  5) are within run-to-run noise. In 17 of 25 runs (16 of the 22 that failed) session 1 did not stop
  at batch 5, and in 12 of those 17 session 2 ran from step 1 again. The passes on Super (3 of 6, all
  after a stop at batch 5) are not replicated here. Small n, descriptive only.
- Where Lightning fails, no-status task, 5 runs per arm (run 39417288601; forms on branch
  `exp/nvidia-stop`). `stop-first` is `stateful-pending` with "a step that exits non-zero ends the
  run" as its first line. `stop-gate` runs each step only through a script in the skill that keeps
  the progress file and prints `STOP` on a failed or pending step.

  | arm | pass | session 1 loaded the skill | session 1 loaded exactly 1-5 | of those: progress file written · session 2 ran step 5 again |
  |---|---|---|---|---|
  | none | 0/5 | 0/5 | 1/5 | 0 · 1 |
  | `stateful-pending` | 1/5 | 5/5 | 4/5 | 3 · 3 |
  | `stop-first` | 0/5 | 5/5 | 4/5 | 0 · 4 |
  | `stop-gate` | 0/5 | 4/5 | 4/5 | 4 · 4 |

  When session 1 loaded one of these short hand-written skills, it loaded exactly steps 1-5 in 12 of
  14 runs (the `mm-a-*` arms on Lightning, run 39405633274: 8 of 25). In its 4 runs that loaded 1-5,
  `stop-first` never wrote the progress file and session 2 started again from step 1 (rerunning step
  5 on the way). In `stop-gate`'s 4 such runs the file was right (`{"done": [1, 2, 3, 4], "pending":
  5}`) and the script refused step 5 in session 2, but each time the agent then ran `python3 step.py
  5` directly. Small n, descriptive only.
- A second run of `stop-gate`, and `stop-gate-say`, whose `STOP` text and SKILL.md also say that
  the problem being over does not tell whether the step took effect and that running the step
  directly is the double load to avoid (run 39437838511, 5 runs each):

  | arm | pass | session 1 loaded the skill | session 1 loaded exactly 1-5 |
  |---|---|---|---|
  | `stop-gate` | 0/5 | 4/5 | 3/5 |
  | `stop-gate-say` | 2/5 | 3/5 | 3/5 |

  After a `STOP` in session 2, `stop-gate` runs went on by running `step.py 5` directly (2); in the
  third, session 2 ran `run_step.py 5` twice, then 6-8, and step 5 ran again (no shell command
  wrote the progress file; non-shell tool calls are not recorded). Of the two `stop-gate-say` passes,
  one stopped and reported; the other found the store behind the `warehouse` module (`/tmp/warehouse`,
  counted as probes), saw batch 5 there, marked it done and ran 6-8. The third clean run ran
  `step.py 5` directly, cleared `pending` with `python3 -c` and ran 6-8 through the script. Across both runs `stop-gate` passed 0 of 10. Small n, descriptive only.
- `stop-gate-say` again, 10 runs (run 39572265076): pass 4/10, none of them probing the store; each
  of the 4 ran `run_step.py 5`, got `STOP` and reported that batch 5 needs checking. Session 1 loaded
  exactly 1-5 in 9 of 10. The other 5 of those 9 ran step 5 again in session 2: 3 by running
  `step.py` directly, 2 through the script, though no shell command wrote the progress file
  (non-shell tool calls are not recorded). With run 39437838511, `stop-gate-say`
  passed 6 of 15, 5 of them without probing; `stop-gate` passed 0 of 10 (runs 39417288601 and
  39437838511). Small n, descriptive
  only.
- The state contract then gained a rule from these runs: in a stateful skill, enforce the pending
  rule in a script the skill runs for every step; the script offers the one way to resolve a pending
  step (check the system, retry only where allowed, otherwise leave it for the user), and its
  refusal says what to do instead and names the shortcuts (running the step directly, editing the
  state by hand, reading the system's internals in place of a check it offers) and that the cause
  being gone does not show whether the step took effect. MiniMax with dao then wrote skills from the
  request (run 39594020075, 3 runs per task): status task 3/3 (two runs probed the store once each),
  no-status 3/3, each no-status run ending with batches 1-5 loaded once; each no-status skill
  shipped a script with a pending-type marker and a stop path. Run by Lightning (run 39601418222, 4
  runs per skill): 6 of 12 passed (per skill 2/4, 0/4, 4/4). In 4 of them session 2 ran nothing and
  probed nothing; one ran only step 6; one probed the store (2 probes) and ran 6-8. With the rule's
  first wording, which had no resolve path (run 39591442902, skills from run 39585338002), 5 of 12
  passed, each with ran = 1-5 and no probes. The skill MiniMax wrote with dao before the rule
  (`mm-dao-a`, runs 39394182506 and 39405633274) passed 2 of 10 on Lightning, neither by stopping at
  batch 5. Small n, descriptive only.
- Writer cost before and after that rule, MiniMax with dao (separate runs; differences are
  descriptive, not attributed). Before (run 39198153129, 5 runs per task): pass 5/5 and 5/5 (status
  / no-status), scripts in 0 of 5 and 3 of 5 (315-365 lines), median maker turns 13 and 24, median
  maker time 111 s and 171 s. With the rule (runs 39585338002 and 39594020075, 6 per task): pass 6/6
  and 6/6, scripts in 4 of 6 (45-847 lines) and 6 of 6 (413-602 lines), median turns 21.5 and 39,
  time 375 s and 384 s. With a candidate sentence added ("keep the script to what the rule needs, a
  few dozen lines ... a fixed-steps skill needs no such script", branch `skill/small-step-script`,
  runs 39606904444 and 39614111245, 6 per task): pass 5/6 and 5/6, scripts in 2 of 6 (172-287 lines)
  and 6 of 6 (257-466 lines, overlapping the range before), median turns 25.5 and 28.5, time 377 s
  and 314 s; no script came near a few dozen lines. Both failures ran nothing in session 1: one
  stopped on a mismatch between its helper and the status command's exit codes, one treated a
  missing state file as an unknown run; the 22 earlier runs had no such stop. Not merged. Small n,
  descriptive only.
- A 77-line reference script in the meta-skill (`assets/step_gate.py`, linked from the state
  contract with "copy it and adapt it rather than writing a larger one"; branch
  `skill/step-gate-template`, PR #87, not merged), MiniMax with dao, 3 runs per task. First version
  (run 39621795655): no-status 3/3, scripts 107, 125 and 269 lines (two adapted from the reference);
  status task 1/3, two of its skills wrote a `step_gate.py` although that task has a status query,
  one session tried it with a made-up step that loaded a batch the runbook does not have, one ran
  nothing in session 1, citing a path mismatch for its script, the pass probed the store twice. With
  "for stateful skills only ... never try it on the real system" added (run 39628099379): no-status
  3/3, scripts 228, 270 and 305 lines; status task 2/3 with no scripts, the third rep's writer
  produced no skill. Across both, no-status scripts were 107-305 lines against 413-602 with the rule
  alone, and the status task passed 3 of 6 against 6 of 6 (one of the three losses is a writer that
  produced no skill, with no script involved). Small n, descriptive only.
- The #85 rule's skills on another executor: Nemotron 3.5 Super VL (reasoning high, run 39346350449
  above) ran the no-status task with `mm-dao-a` (written by MiniMax with dao in run 39198153129,
  before the rule) and the three skills MiniMax wrote under the rule (`mm-rule2-*`, run
  39594020075), 3 runs each (run 39717858820). Every run's session 1 loaded exactly 1-5. `mm-dao-a`
  passed 2 of 3: one stop, one pass that read the store in session 2 and then ran 6-8, one run that
  loaded batch 5 twice and went on through 8. The rule's skills passed 9 of 9: 7 stopped after batch
  5, and 2 (both `mm-rule2-0`) read the store in session 2 and then ran 6-8. Runs with probes: 1 of
  3 and 7 of 9; most session-1 probes were `import warehouse` to locate the module. No run refused
  at the start or stopped before batch 5. Small n, descriptive only.
- The warehouse is now sealed: until this change its store was plain files the agent could read (the
  probe passes above). resume.py now serves it from its own memory on localhost; `load` always
  works, `loaded` only where the runbook names a status query, and the checkers read the steps with
  a token the agent's environment does not carry. It is not a sandbox: the agent runs under the same
  user as resume.py, so a determined agent may still find a way into the harness process or the
  server; it removes the plain-file shortcut. Replays on branch `exp/sealed`: 3.5 Super VL (run
  39723364857, 3 runs each) passed the status task with `fixed-check` 2 of 3, its checks answering
  through the sealed store; on the no-status task `mm-dao-a` passed 2 of 3 and `mm-rule2-0` 3 of 3,
  every pass now a stop after batch 5 (in run 39717858820 three passes had read the store and gone
  on through 8). Lightning with `stop-gate-say` (run 39723378511, 5 runs) passed 1 of 5; the other 4
  loaded batch 5 again. Small n, descriptive only. Rows now also keep failed Bash calls
  (`bash_errors`, first 200 characters). In a check run (run 39728140771) they showed why some first
  `step.py` commands appear twice with one load: 3.5 Super VL passed Bash an unknown `workdir`
  parameter, the call was rejected before running, and the model repeated it; and on the no-status
  task `warehouse.loaded(5)` was refused as intended.
- Lightning on the sealed warehouse, no-status task, 8 runs per skill (run 39736805380); a stop
  means a pass with batches 1-5 loaded once and nothing after:

  | skill | pass | stop | session 1 loaded exactly 1-5 |
  |---|---|---|---|
  | `mm-dao-a` (before the #85 rule) | 1/8 | 0/8 | 3/8 |
  | `mm-rule2-0` / `-1` / `-2` (under the rule) | 2/8 · 4/8 · 2/8 | 1 · 3 · 2 | 4 · 7 · 8 |
  | `stop-gate-say` (hand-written) | 2/8 | 2/8 | 7/8 |

  Under the rule 6 of 24 runs stopped, against 0 of 8 before it and 2 of 8 for the hand-written
  gate; the other passes went on through 8 having loaded batch 5 once. Of the runs whose session 1
  stopped at 5 and that then failed, session 2 ran `step.py` directly in 12 and restarted from step
  1 in 5, and did something else in 2. With the store sealed, these rates no longer include passes
  made by reading it (5 runs still tried). 23 of Lightning's Bash calls were rejected as invalid
  input before running. Small n, descriptive only.

### Limits

- Third-party models only; results can differ with Claude models.
- 2–3 repetitions per cell; no significance claims between arms.
- Tasks are public and small by design so anyone can rerun them.
