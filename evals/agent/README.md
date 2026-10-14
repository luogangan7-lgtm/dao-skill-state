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

**A2 — security scan, labeled set** (`labeled_bench_attack_variants_and_benign_look_alikes` in
`crates/dao/src/scan.rs`, runs in CI). Synthetic attack variants mostly not in the other scan tests,
plus benign look-alikes. Before #49 the scanner caught 16/23 attacks (missed command/process
substitution of downloads, `exec(urlopen(…))`, a PowerShell home wipe, "do not mention this step to
the user", and a prose instruction to send a private key to a URL) and flagged 0/15 benign. After:
**26/26 attacks, 0/27 benign**; the 112 real skills got identical verdicts (86 admitted). The
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

### Limits

- Third-party models only; results can differ with Claude models.
- 2–3 repetitions per cell; no significance claims between arms.
- Tasks are public and small by design so anyone can rerun them.
