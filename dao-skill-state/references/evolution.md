# Evolve through actual consumption

Use for an authorized skill evolution request: turn observed failure or a new
requirement into a durable candidate, execute it, then admit or discard it.
For an ordinary known-defect edit retain the short entrypoint repair route.
Choose the role before loading further references:

- **Candidate author:** with an already frozen task, failure, old successes and
  boundaries, read this loop and those inputs only. No default review/assessment
  or helper-schema load. Implement and actually consume the candidate; do not
  manufacture your own independent admission. If inputs or criteria are missing,
  the acceptance owner freezes them before comparison.
- **Actual grader/admission owner:** read [review.md](review.md) and
  [assessment.md](assessment.md) completely before grading or designing acceptance.
- **Freeze/bind/promote coordinator:** read [evolution-helper.md](evolution-helper.md)
  for the complete mechanical APIs, schema and transaction contract. A role doing
  grading too must also follow the grading route.

Trust remains a veto over permissions, sensitive data, untrusted inputs/actions,
provenance/dependencies and environment fitness. Required FAIL or UNKNOWN blocks
admission. Use the user's existing authority and coordination. Do not load
unrelated methods or the entire knowledge library.

## Agent loop

1. **Observe.** Capture the failing request, actual artifact/native return and
   skill hash. Locate the nearest rule and callers. A correct ignored rule needs
   retrieval/execution repair; another warning does not solve that failure.
   Query existing decisions/rejections and the designated authority before work.
2. **Freeze and propose.** Separate fixed task/rubric inputs from the editable
   skill. Freeze the same failing task plus old successes, relevant boundaries
   and affected modes. Specify must-pass criteria, scope and allowed file paths
   before seeing candidate grades. Make the smallest complete candidate in a
   package-external attempt directory; no speculative new platform or memory.
   A resume failure (repeated action, lost approval or position) changes the
   skill's form only as the [form standard](forms.md) says.
3. **Consume.** Run both frozen base and candidate on the same inputs and matched
   environment. Re-read the actual candidate files; record the consumer hash,
   native return, artifact and criterion results for each required case. Keep
   failures. A command exit 0, code lint or claimed hash is not task success.
   Do not expose held-out answers to the candidate author or change an oracle.
4. **Admit independently.** A reviewer who did not edit the candidate inspects
   actual outputs and side effects, with explicit scope and evidence. Every
   required candidate criterion and Trust item must PASS; an old PASS cannot
   regress. Missing evidence is UNKNOWN and blocks admission. A base FAIL becoming
   candidate PASS is bounded capability gain; report it separately from costs.
   A no-op or no demonstrated gain is discarded, including measured cost growth
   with no quality gain. Unknown cost permits a measured capability gain under
   limited scope, but cannot support an efficiency claim. No cross-case average
   can cancel a critical failure. No mandatory paper review, repeated judges,
   population inference or fixed number of model calls is needed for a narrow fix.
5. **Update locally.** Before replacement obtain the host's authorized write lock
   and name the verified real source and snapshot. Installed symlinks are not
   write targets. Verify current source still equals the frozen base and all
   receipts still bind the candidate and inputs. Only complete, admitted local
   updates may be freshly consumed; no pending tree is an accepted version.
6. **Reopen or recover.** Preserve new failures, conflicts and receipts; update
   the existing authoritative task/decision rather than adding a helper ledger.
   If a new consumption fails, reopen and patch with new actual evidence. Use
   rollback only if the complete current tree still matches this attempt's
   promoted tree. Foreign edits stop rollback/recovery; never override them.
7. **Fresh check and persist.** Re-read the promoted source and run the actual
   task again. Store the durable reason, bounded outcome/evidence and recovery
   pointer in the user's designated system, then update your task element.
   Broader activation remains subject to existing independent/project gates.

## Evidence scope

The helper’s recorded local tests support only its mechanical behavior. Actual
child-skill failing consumption → candidate rerun → old-success regression →
independent admission → local promotion → fresh consumption is the capability
gate. Bind it to current file identity; old evidence does not close a new failure.
Keep native returns and failed attempts outside the package. If format lint fails
only because a staging folder name differs, validate one same-payload named export
and preserve its identity; do not keep retrying or rename an installed source.
