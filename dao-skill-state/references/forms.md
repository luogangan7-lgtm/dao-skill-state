# Skill form: fixed steps or stateful

Read this when creating, evolving or reviewing a skill that may need to resume. Running or keeping state for a long task is [the state contract](state-contract.md).

A skill is **stateful** when it keeps progress somewhere to resume from; otherwise it is **fixed steps**. Decide with two questions, for created and evolved skills alike:

1. **Can a run be cut off and continued by a fresh session** (it spans sessions or days, waits for approvals, or can crash mid-step)? No → fixed steps.
2. **Can that session learn from the authoritative system, cheaply and without probing it, which steps took effect** (every step is idempotent, or the system answers "did step N take effect" for every step)? Yes → fixed steps plus a resume rule: before running any step, check it in the system, all of them, not only the one that failed. No → stateful.

A stateful skill must state three things: **where** state lives (outside the skill directory, or a reference to the user's task system when it owns progress), **how a run resumes** (read the state first), and **how an action with an unknown outcome is reconciled** (mark it pending before running it; on resume check the real system before running it again; if there is no way to check, do not rerun it: stop and report). Never ship part of it: a progress file without the pending rule is worse than no state. State is for surviving a cut-off, not a reason to stop: a run continues step after step until done, blocked or failed, saving after each step.

In a stateful skill, prefer enforcing the pending rule in a small script the skill runs for every step over prose alone: it keeps the state, marks a step pending before running it and done only after it succeeds, and does not run past a step still pending or failed. The script also offers the one way to resolve such a step: check it against the system and record it done if it took effect, or run it again only where the request allows a retry; when it cannot be checked, the step stays pending until the user decides about it. Weaker executors often skip writing state the prose asks for, and they treat a bare refusal as an obstacle. So the refusal says what to do instead (resolve the step that way, or, when it cannot be checked, report which step needs checking and end the turn) and names the shortcuts that would defeat it: running the step's command directly, editing the state by hand instead of resolving, or reading the system's internals in place of a check it offers. It also says that the cause of the failure being gone does not show whether the step took effect.

When evolving an existing skill:
- fixed → stateful: an observed resume failure (a non-idempotent action repeated, an approval or position lost) and question 2 answers no. If the system can answer, add the check rule instead.
- stateful → fixed: question 1 or 2 now answers yes for every step, or the state duplicates a task system that already owns progress (reference it instead).
- a stateful skill with a gap: add the missing part; do not delete state unless the questions say fixed.

`dao normalize` records each skill's form, the lines that make it stateful, and which of the three parts it leaves out (`form`, `state_evidence`, `state_gaps` on the routing card). It is a lexical review hint: a gap is a review note, not a rejection, and an empty gap list is not a pass.
