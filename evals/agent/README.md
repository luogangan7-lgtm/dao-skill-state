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

- Without any skill the agent passed **24/24**: these office/TDD/debug tasks are within its reach.
- Installed skills were almost never opened unprompted (0/3 in the first probe). With the skill-hub
  hook the right skill was surfaced and opened **24/24**.
- Skills cost 2–4× the turns with no pass gain on these tasks; the pptx skill's own conventions
  overrode exact user specs in some runs.

Conclusion: the routing hook is what makes installed skills get used; whether a skill improves
the result depends on the task being beyond the model's defaults.

### B. Skill creation (`create`, 3 tasks)

| Run | Model | reps | Created: none / skill-creator / dao | Mean create turns |
|---|---|---|---|---|
| 38565313467 | MiniMax | 3 | 9/9 / 9/9 / **6/9** | 5.3 / 5.9 / 16.3 |
| 38588702838 (after #43) | MiniMax | 3 | – / – / **9/9** | – / – / 18 |
| 38581453943 | Kimi | 2 | 6/6 / 5/5 / 4/4 (3 runs refused by quota) | 3.7 / 4.2 / 10.6 |

- Baseline (usage tasks with no skill): **0/3** — the team rules cannot be guessed, so passing
  usage proves the created skill carries them.
- Every created skill was usable: with the hook, usage passed in nearly all runs. With the skill
  only installed, use depends on the agent choosing to open it (skill-creator's api-errors skill
  0/3 on MiniMax without the hook, 3/3 with it).
- Defect found and fixed: dao sometimes wrote `./new-skill/<name>/SKILL.md` (nesting a folder to
  satisfy its own folder = name rule) and did not say so. #43 fixed the entry rule; rerun 9/9.
- dao is the slowest creator (2–3× the turns), often adding scripts and self-checks.

### D. Self-evolution (`evolve`, 2 tasks with a seeded defect)

| Run | Model | reps | Candidates fixed (none / skill-creator / dao) | Regressions | Held-out passes |
|---|---|---|---|---|---|
| 38559638817 | MiniMax | 3 | 5/6 / 6/6 / 6/6 | 0/18 | 18/18 |
| 38575435595 | Kimi | 2 | 4/4 / 4/4 / 4/4 | 0/12 | 12/12 |

All arms fix the observed failure without breaking prior successes. The seeded defects are easy
enough that no meta-skill is needed, so D does not yet separate the arms; harder tasks
(multi-file skills, conflicting evidence, a tempting over-fix) are the next step.

### Limits

- Third-party models only; results can differ with Claude models.
- 2–3 repetitions per cell; no significance claims between arms.
- Tasks are public and small by design so anyone can rerun them.
