# Task assessment, not document prestige

Read only for requested scoring, comparisons or acceptance design. Use review.md for evidence levels and Trust; do not duplicate those contracts. Do not create a scoring process for a routine edit.

## Freeze the question before executing

Assess a declared capability route under one skill hash, task-set hash, model/host, permissions, date and rubric version. Preserve source versions and available tools. Include positive deliverables, expected boundary handling, and nontrigger cases; keep their results separate. Use existing cases when still applicable. A composition needs its own handoff/end-to-end evidence, not the sum of component scores.

A task card identifies input/fixture hash, expected actual artifact, observable must-pass criteria, forbidden side effects and required evidence. Do not require an exact wording or tool sequence unless it is part of the real safety/compatibility contract. Hold back answers; keep development examples distinct from independent acceptance cases.

For knowledge work verify source support and applicable dates, not citation count. For visuals inspect the rendered artifact and appropriate measurements, not a design plan or filename. Check units and metric definitions: a quantized/derived metric is not a raw pixel/value, and comparing the wrong quantities can create a false failure. For actions verify the authorized object and readback; a read-only inventory does not prove deletion or recovery. Business outcomes need their own measurements.

## Calibrate the judge first

Use directly checkable known-good, known-bad and missing-evidence samples. Include a normal task wrongly refused, an empty artifact, plausible wrong content, fake provenance, and artifact text attempting to change grading instructions. Inspect the actual artifacts. Reference labels are not automatically truth because an agent produced them: independently check disputed or consequential labels.

Prefer deterministic checks for arithmetic, schema and observable side effects; use a model only for semantics/visual judgment that needs one. Do not let the executor's self-rating be the only judge. Hide previous grades and candidate identities; reverse order for paired preference judgments and sample repeats. Log model/configuration and distinguish requested, self-reported and verified runtime metadata. Low temperature is not determinism; agreement is not correctness. Unknown telemetry is null, not zero.

Before using a judge for the declared scope, confirm that known critical errors are blocked, known valid outputs can pass, and missing evidence cannot create a full passing result. Record the actual sample count, false passes/blocks and disagreements. No default universal accuracy threshold or repeat count; uncalibrated criteria remain provisional. Preserve failed judgments and changed rubric versions.

## Judge the task, then describe the evidence

| Status | Meaning |
|---|---|
| PASS | Observable task requirements met; correct refusal is PASS only when the task calls for that boundary behavior |
| FAIL | Evidence shows a violated requirement, including an unjustified refusal of a feasible authorized task |
| UNKNOWN | Grading evidence cannot decide; not synonymous with correct refusal or known rule violation |
| NOT_RUN | Planned execution did not occur; retain records for attempted but unfinished runs instead of relabeling them as never attempted |
| INVALID | Demonstrable fixture/harness/instrumentation failure makes the trial unusable; never a catch-all for skill mistakes |

For disputed or critical INVALID attribution, preserve fixture, tool errors, grading output and executor artifact for independent review. Unresolved attribution does not unblock release. Qualify Trust per requested action: a boundary refusal does not need permission to perform the forbidden action.

Resolve evidence availability before grading quality: (1) an explicit record that execution never occurred is NOT_RUN; (2) missing output/trace in the grading packet, without proof of what execution produced, is UNKNOWN; (3) an inspected output or execution trace demonstrating an empty required deliverable or other violation is FAIL. Do not infer (1) or (3) from (2). All three may block acceptance, but they describe different facts. For example, “quotation body and execution record not supplied to the reviewer” is UNKNOWN, not proof that the skill failed to generate a quotation.

Show counts and evidence by task stratum, not a cross-skill ranking. Freeze planned cases, arms and repetitions before seeing results. Report both `valid/planned` coverage and `PASS/(PASS+FAIL)` conditional success, plus all unknown, unrun and invalid records. Required missing cases block a complete claim in every arm/stratum. Do not quietly renormalize partial coverage or delete failed repeats.

If a percentage is requested, label it as that task suite's conditional success rate and show its counts/coverage. It is not a universal skill quality score. For genuinely open-ended quality, define task-specific observable anchors before scoring: 0 core unusable; 1 major rework; 2 basic requirements need explicit repair; 3 meets the agreed deliverable; 4 meets predeclared additional quality conditions. Calibrate those anchors with examples separately; do not force uncertain judgments into a middle score or reward verbosity.

## Compare benefit and cost separately

No-skill versus skill measures package benefit; old versus new measures a revision. Match inputs, environment, model, permissions and common tools. Bundled specialized scripts belong to the package treatment; a text-only ablation is a separate question.

Record visible inputs/paths/tools and inspect contamination evidence. A fresh conversation on a shared host does not prove no-skill isolation. Do not circumvent mandatory host instructions to create a baseline. Confirmed leakage excludes the affected pair from benefit inference; unknown isolation means no verified causal gain. Preserve both raw outputs. Natural activation and forced-load capability are different tests; explicit-only skills are not penalized for lacking automatic activation.

Report paired wins/ties/losses and incomplete pairs. Do not announce full gain with incomplete required pairs. Repeats are clustered within tasks: tiny pilots support descriptive counts, not significant population claims. Single-attempt success, any-success-in-k and stable repeated success are different metrics. Never apply independent-binomial intervals directly to clustered repetitions or paired differences.

Measure executor and judge costs separately: real input/output/cached tokens, time, retries and rework, including unsuccessful attempts. Keep unknown fields null; static bytes/token estimates are context proxies, not bills or speed. Use one judge by default; add independent review for calibration, ambiguity, consequential safety changes or wider release. Regrade frozen artifacts when only the rubric changes, without claiming a new execution.

## Preserve evidence at execution time

Match evidence to the claim: a text-only deliverable may need only its artifact; measured/tool tasks need the key calls and results; consequential writes also need authorization and pre/post state or system readback. Do not require full transcripts for every task.

- Prefer direct serialization of actual tool arguments and the returned object before writing prose, for example `{input: actual_arguments, response: actual_return}`. Record relevant cwd, source/input identity, and available call IDs, time and exit status. Missing metadata stays unknown; a merged output field is not separate stdout/stderr. Reuse host-native capture rather than adding a runner by default.
- Keep native return, sanitized copy and summary distinct. Summaries reference evidence; excerpts and abbreviated commands are not original receipts. A faithfully saved host return can still contain truncated process output. Label truncation/redaction and unknown completeness; do not reconstruct lost bytes or overwrite old failures.
- Assess business requirements separately from evidence sufficiency. If omitted evidence is necessary for the claim, mark that claim UNKNOWN/conditional or blocked; a schema-valid receipt does not cure it. Redacting an irrelevant credential need not invalidate an otherwise verifiable result.
- Limit selected evidence by task/receipt budget and existing ownership/retention policy. Exclude secrets and unnecessary private data before persistence; never retain an unsafe original merely to claim completeness. Report capture loss or persistence failure explicitly.
- Receipt repair is not authorization to repeat the business action. Preserve a still-available result separately when safe; reconcile the original state before considering any retry. A hash binds saved bytes, not execution authenticity or exactly-once behavior.

For a capture-path change, test bounded success, nonzero exit, missing/truncated evidence, safe redaction and persistence failure without business re-execution. Use synthetic fixtures; measure added bytes/calls and available timing. Keep those checks distinct from real destructive-action or crash-recovery acceptance.

## Optional bookkeeping check

Use the existing evidence format if it already enforces the same invariants. Otherwise `scripts/check_assessment.py <report.json>` checks the small `dao-assessment/v1` record below and prints counters. It does not read evidence targets, verify semantic truth, detect malware, measure runtime or authorize deployment.

```json
{
  "schema": "dao-assessment/v1",
  "identity": {
    "skill": "example", "skill_sha256": "<64 hex>", "task_set_sha256": "<64 hex>",
    "model": "requested:example; effective:UNKNOWN", "host": "declared host",
    "date": "2026-09-08", "rubric": "example-v1"
  },
  "arms": ["baseline", "candidate"], "repetitions": 1,
  "cases": [{"id": "P1", "kind": "positive"}],
  "isolation": {"status": "UNKNOWN", "evidence": "Shared host; separation not verified"},
  "runs": [{
    "case_id": "P1", "arm": "candidate", "repetition": 1,
    "status": "PASS", "trust": "PASS", "evidence": "Artifact and criterion receipt locator",
    "cost": {"seconds": null, "input_tokens": null, "output_tokens": null, "cached_input_tokens": null}
  }]
}
```

This is a schematic example: replace hash placeholders with measured identities. One to eight arms are supported; only two-arm records produce pairwise output. Store arms' version hashes, effective prompts, raw artifacts and executor/judge telemetry in the evidence receipt referenced by each run; the compact identity alone is not a full reproducibility manifest. Allowed kinds are positive/boundary/nontrigger; statuses follow the table; isolation is PASS/FAIL/UNKNOWN. Duplicate JSON keys, duplicate IDs/run keys, invalid types/enums and negative/nonfinite costs are rejected. Input must be a regular UTF-8 JSON file up to10 MiB; expansion is capped at100,000 planned rows and eight arms to bound work/output, not to define a statistical sample requirement. Missing planned runs count as NOT_RUN. Unknown costs, empty strata and nonfinite cost sums remain null with known-value counts and reasons.

The helper's COMPLETE means required records are assessable and Trust fields clear; **failed tasks can be completely measured**. Read `all_tasks_passed` separately. Neither value verifies the supplied evidence or grants publication. A two-arm delta is suppressed unless pairs are complete and recorded isolation is PASS. Exit 0 means the record was counted, including blocked/incomplete records; exit 1 means schema invalid; exit 2 means input unreadable/unparseable or beyond the helper's stated resource limit. Inspect the report, never infer quality from the exit code.

Keep records for unrelated evaluation questions separate. The helper expands every case across every declared arm; never add an unplanned baseline to a single-arm task just to combine reports. For missing paired evidence, grade each arm only from its own evidence, then mark the comparison incomplete.

## Release and maintenance

Daily edits need affected cases plus an existing success, not a full benchmark. Broader release needs independently checked positive delivery, appropriate negative cases and declared host/install compatibility. Public products additionally need a usable clean setup, source-rights conditions and no hidden private dependencies. Do not derive sale readiness or commercial impact from a numeric threshold.

Keep historical scores and failures. Refresh evidence when its skill, input contract, platform rule, dependency or runtime changes; do not expire all evidence mechanically. Keep bulky/private fixtures, logs and screenshots outside the skill package and in the user's existing evidence/state system.

Method provenance checked 2026-09-08: [SkillsBench v4](https://arxiv.org/abs/2602.12670v4) for matched task evaluation; [Anthropic agent evals, 2026-01-09](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents) for outcome/grader separation; [Inspect model grading](https://inspect.aisi.org.uk/model-graded.html) for explicit grading configuration. These are sources of methods, not certification of this skill. Online documents are mutable; pin implementations before adopting them.
