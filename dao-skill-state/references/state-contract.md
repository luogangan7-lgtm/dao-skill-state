# Optional long-task state contract

Read this only for resumable, multi-step work. Short tasks remain stateless. This file is an authoring/review contract, **not an implementation of a SKILL.state runtime**.

## Form standard (created and third-party skills alike)

A skill is **stateful** when it keeps progress somewhere to resume from; otherwise it is **fixed steps**. Decide with two questions, for created and evolved skills alike:

1. **Can a run be cut off and continued by a fresh session** (it spans sessions or days, waits for approvals, or can crash mid-step)? No → fixed steps.
2. **Can that session learn from the authoritative system, cheaply and without probing it, which steps took effect** (every step is idempotent, or the system answers "did step N take effect" for every step)? Yes → fixed steps plus a resume rule: before running any step, check it in the system, all of them, not only the one that failed. Prefer putting that rule in a small runner the skill runs from start to finish, starting and continuing alike: for each step it asks the system first, skips a step that took effect, runs one that did not, and stops at the first failure. It never refuses, so continuing after a failure is the same command again. It takes no arguments (it reads the steps and the check from the task's own files), and SKILL.md gives that one command before anything else: weaker executors that skip a written check rule tend to follow one short command, not a long procedure or a command with options. No → stateful.

A stateful skill must state three things: **where** state lives (outside the skill directory, or a reference to the user's task system when it owns progress), **how a run resumes** (read the state first), and **how an action with an unknown outcome is reconciled** (mark it pending before running it; on resume check the real system before running it again; if there is no way to check, do not rerun it: stop and report). Never ship part of it: a progress file without the pending rule is worse than no state. State is for surviving a cut-off, not a reason to stop: a run continues step after step until done, blocked or failed, saving after each step.

In a stateful skill, prefer enforcing the pending rule in a small script the skill runs for every step over prose alone: it keeps the state, marks a step pending before running it and done only after it succeeds, and does not run past a step still pending or failed. The script also offers the one way to resolve such a step: check it against the system and record it done if it took effect, or run it again only where the request allows a retry; when it cannot be checked, the step stays pending until the user decides about it. Weaker executors often skip writing state the prose asks for, and they treat a bare refusal as an obstacle. So the refusal says what to do instead (resolve the step that way, or, when it cannot be checked, report which step needs checking and end the turn) and names the shortcuts that would defeat it: running the step's command directly, editing the state by hand instead of resolving, or reading the system's internals in place of a check it offers. It also says that the cause of the failure being gone does not show whether the step took effect.

When evolving an existing skill:
- fixed → stateful: an observed resume failure (a non-idempotent action repeated, an approval or position lost) and question 2 answers no. If the system can answer, add the check rule instead.
- stateful → fixed: question 1 or 2 now answers yes for every step, or the state duplicates a task system that already owns progress (reference it instead).
- a stateful skill with a gap: add the missing part; do not delete state unless the questions say fixed.

`dao normalize` records each skill's form, the lines that make it stateful, and which of the three parts it leaves out (`form`, `state_evidence`, `state_gaps` on the routing card). It is a lexical review hint: a gap is a review note, not a rejection, and an empty gap list is not a pass.

Measured (evals/agent/README.md section E; one 8-step task with non-idempotent loads, MiniMax only, n=3–5 per arm, E3): progress visible in the workspace or queryable → fixed and stateful resumed equally (5/5); a crash after a committed step → a done-only progress file reloaded the step in 8/8 runs, the full contract and fixed-plus-check passed 5/5; no status query → the full contract stopped safely 3/3, fixed forms finished only by reading the store's files or by skipping the unknown step; one fixed-plus-check run reran steps an earlier session had finished, because it checked only the step it assumed unfinished. Writing skills from a situation-only request (README section F), this meta-skill chose the form these questions give in 38 of 41 created skills (the other 3 chose stateful where fixed would do, and still checked the system) and none loaded a batch twice.

## Decide whether it helps

- Use state when a restart would lose decisions, work position, approvals, or receipts.
- Reuse the user's authoritative task system. If it already owns progress, reference it instead of writing a competing `state.json`. A local checkpoint may be a derived, dated cache, never a second truth.
- Open-ended research/design needs selective evidence rereads and possibly evolving schemas. Do not pretend a small fixed summary is always sufficient.
- “Fixed instructions + current state + latest observation” only becomes runtime context isolation if the host actually controls each model request. Without that control label the mode **checkpoint-assisted**; never tell the model that saying “forget history” has removed it.

## This skill's own long runs

The same contract applies to this meta-skill. An evolution loop, a skill family or a review that spans sessions keeps its state in the user's task system when one exists. Otherwise keep one checkpoint per run at project `.dao/state/<run>.json` (goal, phase and next action, decisions, approvals, artifact and evidence paths with hashes, updated-at); never inside a skill directory, and keep `.dao/` out of anything published (ignore it in version control). Re-read it before resuming and re-observe the real files before an irreversible step; it is checkpoint-assisted, not a runtime.

## Define only necessary fields

Choose typed fields from: task/owner, schema and policy versions, revision, phase/next action, bounded decisions and unknowns, artifact/evidence references, pending action and receipt, source checked-at/valid-until, and a measured size budget. Do not append all transcripts, screenshots, source texts, or every prior event.

Specify the authoritative location, initialization, allowed transitions, completion gate, and how authoritative evidence can be reread. A reference must be resolvable; a stale timestamp is not proof of freshness. Check current external state before an irreversible step.

## Validate before accepting a patch

1. Parse/type-check allowed fields, versions and size. Reject unexpected keys or invalid types.
2. Validate business meaning against evidence and invariants: totals, permissions, allowed state transitions, version freshness and action receipts. Valid JSON is not a true fact. Use deterministic code for deterministic rules.
3. Preserve unspecified keys: merge current with incoming; **explicit null deletes only contract-deletable keys**. Reject deletion of required safety/identity fields. Nested merge semantics must be declared; never silently replace a whole shared document.
4. Before shared writes use the host's ownership/lock/version mechanism. Re-read after a conflict and merge only the authorized delta. Without compare-and-swap or equivalent protection do not claim concurrent-writer safety; use a single writer or stop.
5. Commit only validated state. On failure retain the last verified checkpoint, mark the attempt failed and re-observe; do not replace it with invented “correct” facts.

## Actions, resume and evidence

Before an external action persist a pending record with stable action identity and authorization reference. After execution verify the external receipt before marking done. On restart inspect pending actions and the real system; do not resend a non-idempotent action merely because a local receipt is absent. Missing idempotency/reconciliation means human review, not an exactly-once claim.

Persist the selected checkpoint and evidence references at meaningful boundaries. Resume from the authoritative version, recheck source freshness and tool availability, reconcile any pending action first, then continue from the next unfinished action. State adds no step limit: a run goes on until completion, a gate or approval, an error, a failed recheck, or the user stops it. Keep audit evidence separately under the user's retention policy; reducing prompt history is **not authorization to delete files**.

## Acceptance before runtime adoption

Exercise normal continuation, restart, malformed and semantically wrong patches, explicit-null deletion, stale policy/evidence, duplicate/pending action and concurrent conflict. Mark unsupported cases NOT_TESTED. Compare output quality and actual context/cost/latency with the previous method on the same tasks; preserve failed runs and changed protocols.

Do not claim universal savings, O(1) context, production reliability, or exactly-once from a state file or small experiment. Upgrade from checkpoint assistance only after the host implements and verifies bounded per-request context, sufficient state, recovery and the relevant concurrency/external-action gates.
