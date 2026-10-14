# Agent evaluations

Does a skill help a real agent do a task, and can a meta-skill create or evolve skills that work?
These evaluations run Claude Code (CLI 2.1.285, headless) in GitHub Actions against public tasks
with programmatic checkers. Nothing runs on a developer machine.

| Suite | Script | Question |
|---|---|---|
| `run` (C) | `run.py` | Does loading a skill change task success? Arms: `none`, `skills` (installed), `hook` (installed + skill-hub hook) |
| `create` (B) | `create.py` | Does a meta-skill make better skills? Creator arms `none`, `skill-creator` (anthropics/skills, pinned), `dao` (this repo's `dao-skill-state`); the created skill is then used on tasks whose prompts never restate the rules |
| `evolve` (D) | `evolve.py` | Can a meta-skill fix a skill from one observed failure without breaking it? The harness replays the fail case, a case that already worked and a held-out case with the original skill and with the candidate |

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

### D. Self-evolution (`evolve`, 2 tasks with a seeded defect)

| Run | Model | reps | Candidates fixed (none / skill-creator / dao) | Regressions | Held-out passes |
|---|---|---|---|---|---|
| 38559638817 | MiniMax | 3 | 5/6 / 6/6 / 6/6 | 0/18 | 18/18 |
| 38575435595 | Kimi | 2 | 4/4 / 4/4 / 4/4 | 0/12 | 12/12 |

All arms fix the observed failure without breaking prior successes (one `none` candidate on
MiniMax did not fix it). The held-out case fails with the original skill in changelog-perf (and
in csv-dates on Kimi) and passes with every candidate, so the fixes generalise beyond the shown
case. The seeded defects are easy
enough that no meta-skill is needed, so D does not yet separate the arms; harder tasks
(multi-file skills, conflicting evidence, a tempting over-fix) are the next step.

### Limits

- Third-party models only; results can differ with Claude models.
- 2–3 repetitions per cell; no significance claims between arms.
- Tasks are public and small by design so anyone can rerun them.
