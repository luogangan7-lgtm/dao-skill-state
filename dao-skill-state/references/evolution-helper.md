# Evolution helper: mechanical coordinator contract

Read only for freeze/bind/promote/recover/rollback coordination; the author
loop stays in [evolution.md](evolution.md). Grading still requires review.md and
assessment.md.

`scripts/evolve_skill.py` uses only Python stdlib and POSIX `flock`. It runs no
model, business command or manifest-supplied shell. It checks file and receipt
bindings and performs recoverable local replacement. Its PASS is **mechanical**:
JSON and hashes cannot prove semantic truth, reviewer identity or human authority.
The host owns those judgments and retains Humaux/coord authority where configured.
The single attempt manifest records evidence/transaction pointers, not task memory.
Existing `check_skill.py` and `check_assessment.py` retain their format/count APIs.

Choose a real ordinary source directory containing `SKILL.md`, and a separate
attempt directory outside every source/candidate package. The helper creates
`base/`, `candidate/` and `manifest.json`; source payload files remain intact.
Every payload/evidence file must be ordinary, no symlink/FIFO; JSON duplicate keys
and nonfinite constants are rejected. Relative evidence/input paths cannot escape
that attempt. Files are bounded at 10 MiB each and payloads at 10,000 files.

Put an inspected frozen `plan.json` and the task/rubric files in the attempt:

```json
{
  "scope": "local import task on the tested host",
  "author": "candidate-editor",
  "authorization_ref": "existing human local-source authorization pointer",
  "observation_ref": "existing actual failing run pointer",
  "inputs": ["task.json", "rubric.json"],
  "cases": [
    {"id": "failure", "must_pass": ["correct", "no_overwrite"]},
    {"id": "old_success", "must_pass": ["correct", "no_overwrite"]}
  ]
}
```

Use project-specific paths below; `engine` is the directory containing this skill.
All source mutations require an explicit `--source-root` equal to the ordinary
source and its directory identity bound at prepare. No implicit installation.

```bash
python3 "$engine/scripts/evolve_skill.py" prepare --source-root "$source" \
  --attempt-dir "$attempt" --allow SKILL.md --allow scripts/import.py
# Edit only $attempt/candidate; actual base/candidate consumers and independent
# reviewer save their own receipt/admission and referenced artifacts in $attempt.
python3 "$engine/scripts/evolve_skill.py" bind --attempt-dir "$attempt"
python3 "$engine/scripts/evolve_skill.py" verify --attempt-dir "$attempt"
python3 "$engine/scripts/evolve_skill.py" promote --source-root "$source" --attempt-dir "$attempt"
# For an unfinished transaction only; returns to base if no foreign edits:
python3 "$engine/scripts/evolve_skill.py" recover --source-root "$source" --attempt-dir "$attempt"
# For a complete promoted tree only:
python3 "$engine/scripts/evolve_skill.py" rollback --source-root "$source" --attempt-dir "$attempt"
```

Public Python APIs have the same names:
`prepare(source_root, attempt_dir, allowed_paths, plan='plan.json')`,
`bind(attempt_dir, receipt='receipt.json', admission='admission.json')`,
`verify(attempt_dir)`, and `promote/recover/rollback(attempt_dir, source_root)`.
`inventory(root)` and `tree_hash(files)` calculate the mechanical consumer tree
identity, including each regular file’s byte SHA-256 and POSIX permission mode;
`file_hash(path)` still binds a single actual artifact by bytes only. Existing
byte-only manifests are rejected: prepare a new attempt, never reuse old receipts. `prepare` returns the
frozen manifest; other successful APIs return status/manifest and bounded details.
Failure raises `Blocked`, `Conflict` or the I/O error; CLI emits BLOCKED/CONFLICT/
ERROR and exits 1. Exit 0 never grants authority or semantic acceptance.

## Actual receipt and admission

After execution create `receipt.json` with the four matching manifest bindings
`base_sha256`, `candidate_sha256`, `plan_sha256`, `inputs_sha256`. Candidate hash
comes from actual candidate inventory. `cases` contains exactly the frozen case
IDs. Each case has `baseline` and `candidate` records as below:

```json
{
  "id": "failure",
  "baseline": {
    "consumer_sha256": "<actual base tree SHA-256>",
    "status": "FAIL", "trust": "PASS",
    "must_pass": {"correct": "FAIL", "no_overwrite": "PASS"},
    "evidence": ["runs/base-native.json", "runs/base-artifact.csv"]
  },
  "candidate": {
    "consumer_sha256": "<actual candidate tree SHA-256>",
    "status": "PASS", "trust": "PASS",
    "must_pass": {"correct": "PASS", "no_overwrite": "PASS"},
    "evidence": ["runs/candidate-native.json", "runs/candidate-artifact.csv"]
  }
}
```

This is one required case, not a complete receipt. Add every frozen old-success
case too. Required UNKNOWN/NOT_RUN/INVALID or absent results block binding; retain
those original statuses/artifacts in the authoritative evidence even though the
helper refuses to bind them. Evidence refs are actual relative file paths.
Add `cost: {"baseline": null, "candidate": null}` when telemetry is unavailable.
For a fully measured comparison each arm has finite nonnegative `seconds`,
`input_tokens`, `judge_seconds`, `judge_input_tokens`. Executor/judge timing and
input counts remain separate; other native telemetry stays in referenced receipts.
Efficiency requires all four known, at least one lower and none higher. Static
bytes/tokenizer counts do not fill these runtime fields or estimate cash bills.

The independent reviewer produces `admission.json` after inspecting the actual
files: the same four bindings plus `receipt_sha256`, non-author `reviewer`, exact
plan `scope`, `decision: "ACCEPT"`, factual `rationale`, nonempty `evidence` refs,
and boolean `efficiency_claim` (false for unknown/unsupported costs). REJECT or
insufficient evidence does not become a promoted attempt. Bind freezes current
candidate/receipt/admission/artifact hashes; later edits require a new actual
assessment and admission. Do not manufacture an independent reviewer string.

## Transaction limits and gates

Prepare preserves a full base snapshot. Promotion rechecks source and all evidence
under advisory source-directory lock, persists PENDING before writes, replaces
individual ordinary files and rechecks after writing. Multiple files are **not**
an atomic bundle. Source must not be consumed during PENDING; cooperating hosts
honor the write lock. A non-cooperating writer can still race a check/replace.
Fsync calls are used, but power-loss durability has not been experimentally proven.

Recovery accepts only unchanged base/target file mixtures plus this attempt's
known additions and restores base. Rollback accepts only the entire exact
promoted hash, including regular-file modes. Source/candidate/snapshot mode
changes count as drift, including mode-only changes outside allowed paths.
Directory modes, ACLs, owner/group, xattrs and timestamps are outside payload
identity and are not validated or preserved; replacing a file may change this
metadata. Hosts must inspect/preserve required metadata or use a native update
path before applying to metadata-sensitive sources.
Existing files are never deleted; rollback only removes an
attempt-added file whose current hash remains this attempt's target. A conflict
retains the scene and requires independent reconciliation. Empty created
directories may remain; they are not payload identity and contain no new files.

Run `python3 -B -m unittest discover -s "$engine/scripts" -p test_evolve_skill.py -v`
for public API evidence veto, source/candidate/input drift, missing required
case, symlink/FIFO refusal, critical failure, no-op, fault-injected partial write,
recovery, rollback and foreign-edit preservation gates. These test the helper,
not a model's real evolution ability. An actual child-skill failing consumption
→ candidate rerun → old-success regression → independent admission → local
promotion → fresh consumption is the separate capability gate. Bind that evidence
to the current package hash; report every unrun gate plainly.
