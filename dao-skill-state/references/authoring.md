# Authoring: one useful capability at a time

## Select the artifact

- Start with one procedural skill. Add routed references only for substantial conditional detail.
- Split a family by independently useful jobs and inputs/outputs, not by every noun or platform name. A platform-specific skill is justified when constraints, tools, deliverables, or acceptance differ. Share stable mechanisms by reference; platform facts need dated sources.
- A method becomes a decision procedure, not a quote collection. Distinguish evidence, author preference, and your inference. Use create/merge/discard against the nearest existing asset; an explicit request for an independently maintained product can justify create despite overlap.
- Use a host-provided creator or initializer when it is available and saves work; it is not a required dependency. Do not initialize an existing skill or copy a large scaffold to get one file.

## Choose the form

A skill is either **fixed steps** (the default: one decision workflow that finishes within a session) or **stateful** (long, resumable work where a restart would lose decisions, work position, approvals or receipts). Pick it with the two questions in the [state contract's form standard](state-contract.md#form-standard-created-and-third-party-skills-alike) (can a run be cut off? can the next session learn from the system what took effect?); a stateful skill needs all three parts listed there. Say which form you chose and why; do not add state to a short task.

## Write the contract

Specify trigger and non-trigger, required input, decision workflow, output, stopping conditions, tool requirements and the pass/fail check. Explain only what a capable agent would not reliably infer.

Take inputs as the request describes them. Do not demand structure it did not specify (a heading, section, field or file layout); when the request says the input names something ("each runbook names the command"), tell the skill to find it in the input as written, and stop only when it is truly absent. An invented format makes the skill refuse real inputs.

Use a concise `SKILL.md` with `name` and `description` in YAML. Prefer portable lowercase ASCII letters, digits and single hyphens, at most 64 characters, matching the directory. Human-facing names can be localized in `agents/openai.yaml`. Descriptions are non-empty and at most 1024 characters; specificity matters, not an arbitrary minimum length.

Preserve supported optional fields (`license`, `compatibility`, `metadata`, experimental `allowed-tools`) when relevant. `metadata` is a string-to-string map; quote versions. Host-specific fields may need an adapter and a host validator. Never present a host exception as the universal format. Headings are authoring choices, not format requirements.

Keep essentials in the entry, detailed task guidance in directly linked references, executable helpers in `scripts/`, and output ingredients in `assets/`. Add only directories with a current use. A public/maintained package may also need human setup instructions and license notices. Do not dump research archives or evaluation transcripts into the skill.

For tool-coupled behavior name the available tool and actual required options. For visuals preserve the requested generation tool, aspect ratio, safe area and real visual QA; prompt-only and image-producing modes must not be confused. Do not fabricate tools, links, credentials, examples of success, or supported hosts.

## Verify two levels

1. **Generator:** test this skill's routing, boundary decisions and file production using realistic user requests.
2. **Generated skill:** validate its format and use it on representative input; inspect correctness and forbidden side effects, not heading names. Include a boundary case and a prior failure if available. For evidence-dependent outputs, require a claim gate in the generated workflow: inspect **each exact user-facing claim**, including headlines and benefit copy, and map it to a source ID plus the supplied support text. Mark supported/inferred/unverified. Only supported claims enter an evidence-only deliverable; omit or explicitly separate inferred benefits. Check the actual claim table against the actual output before reporting PASS—an agent's self-reported PASS is not evidence.

Before a trial, choose a run directory **outside** the generated skill folder (a sibling is sufficient). At handoff enumerate the package files separately from trial inputs/outputs/logs and verify none of those run records entered the distributable payload. Explicitly requested synthetic examples may live in the package; private or actual run artifacts may not.

For a narrow change preserve the original successful case. For a substantial new workflow run an independent forward test in an isolated target when feasible. Give the evaluator the request and raw inputs, not the intended answer. If execution is unavailable mark E2/NOT_RUN and describe the exact remaining gate.

Installation and remote publication are separate actions. Validate the source, inspect the destination, preserve existing policy and rollback, then activate only when authorized. Do not require a Git remote or a commit just to author or test local files. Maintenance versions and source hashes identify candidates even before the user chooses a repository.
