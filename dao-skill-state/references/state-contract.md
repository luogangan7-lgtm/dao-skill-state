# Optional long-task state contract

Read this only for resumable, multi-step work. Short tasks remain stateless. This file is an authoring/review contract, **not an implementation of a SKILL.state runtime**.

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
