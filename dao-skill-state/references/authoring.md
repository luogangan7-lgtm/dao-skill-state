# Authoring: one useful capability at a time

## Select the artifact

- Start with one procedural skill. Add routed references only for substantial conditional detail.
- Split a family by independently useful jobs and inputs/outputs, not by every noun or platform name. A platform-specific skill is justified when constraints, tools, deliverables, or acceptance differ. Share stable mechanisms by reference; platform facts need dated sources.
- A method becomes a decision procedure, not a quote collection. Distinguish evidence, author preference, and your inference. Use create/merge/discard against the nearest existing asset; an explicit request for an independently maintained product can justify create despite overlap.
- Use a host-provided creator or initializer when it is available and saves work; it is not a required dependency. Do not initialize an existing skill or copy a large scaffold to get one file.

## Choose the form

Pick **fixed steps** or **stateful** with the two questions in the [state contract's form standard](state-contract.md#form-standard-created-and-third-party-skills-alike): can a run be cut off, and can a fresh session learn from the authoritative system which steps took effect? Duration and interruption alone do not require state: a task spanning days is still fixed steps plus a resume check when the permitted system interface can answer for every step. Otherwise a resumable task needs all three parts of the stateful contract. Say which form you chose and why; do not add state to a short task.

Choose the form before building helpers. When the form-standard questions select fixed-plus-check, write that workflow directly; do not add a competing progress store or state engine. Write the usable `SKILL.md` first, then add only executable support that its workflow actually needs. For necessary helpers reuse verified tools or libraries before implementing generic parsers, hashing or a framework; a language requirement does not itself require a helper or a new runtime. A stateful skill still needs its full pending/reconciliation contract, but only the state and transitions its task needs.

## Write the contract

Specify trigger and non-trigger, required input, decision workflow, output, stopping conditions, tool requirements and the pass/fail check. Explain only what a capable agent would not reliably infer.

Take inputs as the request describes them. Do not demand structure it did not specify (a heading, section, field or file layout); when the request says the input names something ("each runbook names the command"), tell the skill to find it in the input as written, and stop only when it is truly absent. An invented format makes the skill refuse real inputs. So when the request leaves an input's format open (prose a person wrote, a document of unspecified layout), have the agent read it directly; write a parser only for a format the request fixes. A helper that finds nothing has not shown the thing is absent: the agent reads the input itself before stopping.

Do not invent persistent stop markers, human-only resets or exact approval wording. A request to continue an interrupted task is a request to resume under its existing policy, not new authorization. Do not require a second request naming the failed action when existing authorization and retry policy already permit it. Preserve any approval, pause or retry requirement that the task, user, host, project or authoritative system actually imposes; carry these distinctions into generated instructions and helper guards.

Keep a task's stop-on-error rule separate from later resumption: if a command fails and the task requires stopping that session, report and stop even if a status query confirms it took effect. Only on a later request to continue, recheck authorization and required gates, then reconcile the stopped action against the authoritative system: if it took effect, continue from the next unfinished step; if confirmed not to have taken effect, retry only when authorized and the task permits it, otherwise stop and report; if its outcome is unknown, do not repeat a non-idempotent action and report what cannot be checked.

For external-state decisions, name the task's authoritative source and preserve its permitted observation interface and access boundaries in the generated workflow. If how to use that interface is unclear, first research the task inputs, authorized helpers or official interface documentation. Do not infer outcomes by reverse-engineering backend internals or accessing storage outside those boundaries in place of the designated interface. If this permitted research still cannot establish the outcome, report it as unknown and follow the task's stop or escalation policy.

Use a concise `SKILL.md` with `name` and `description` in YAML. Prefer portable lowercase ASCII letters, digits and single hyphens, at most 64 characters, matching the directory. Human-facing names can be localized in `agents/openai.yaml`. Descriptions are non-empty and at most 1024 characters; specificity matters, not an arbitrary minimum length.

Preserve supported optional fields (`license`, `compatibility`, `metadata`, experimental `allowed-tools`) when relevant. `metadata` is a string-to-string map; quote versions. Host-specific fields may need an adapter and a host validator. Never present a host exception as the universal format. Headings are authoring choices, not format requirements.

Keep essentials in the entry, detailed task guidance in directly linked references, executable helpers in `scripts/`, and output ingredients in `assets/`. Add only directories with a current use. A public/maintained package may also need human setup instructions and license notices. Do not dump research archives or evaluation transcripts into the skill.

For tool-coupled behavior name the available tool and actual required options. For visuals preserve the requested generation tool, aspect ratio, safe area and real visual QA; prompt-only and image-producing modes must not be confused. Do not fabricate tools, links, credentials, examples of success, or supported hosts.

## Verify two levels

1. **Generator:** test this skill's routing, boundary decisions and file production using realistic user requests.
2. **Generated skill:** validate its format and use it on representative input (written the way the request describes it, never shaped to fit the skill's own parser or examples); inspect correctness and forbidden side effects, not heading names. Include a boundary case and a prior failure if available. For evidence-dependent outputs, require a claim gate in the generated workflow: inspect **each exact user-facing claim**, including headlines and benefit copy, and map it to a source ID plus the supplied support text. Mark supported/inferred/unverified. Only supported claims enter an evidence-only deliverable; omit or explicitly separate inferred benefits. Check the actual claim table against the actual output before reporting PASS—an agent's self-reported PASS is not evidence.

Before a trial, choose a run directory **outside** the generated skill folder (a sibling is sufficient). At handoff enumerate the package files separately from trial inputs/outputs/logs and verify none of those run records entered the distributable payload. Explicitly requested synthetic examples may live in the package; private or actual run artifacts may not.

Keep a synthetic trial within its own inputs, mock system and run records. Do not inspect unrelated runs or a surrounding evaluator's hidden fixtures, reference answers or checker implementation to steer the generated skill or decide the downstream task's outcome. A test oracle is not authoritative evidence of the real task's state; use the task's permitted interface. Explicitly authorized review of a test implementation is a separate scope.

For a narrow change preserve the original successful case. For a substantial new workflow run an independent forward test in an isolated target when feasible. Give the evaluator the request and raw inputs, not the intended answer. If execution is unavailable mark E2/NOT_RUN and describe the exact remaining gate.

Installation and remote publication are separate actions. Validate the source, inspect the destination, preserve existing policy and rollback, then activate only when authorized. Do not require a Git remote or a commit just to author or test local files. Maintenance versions and source hashes identify candidates even before the user chooses a repository.
