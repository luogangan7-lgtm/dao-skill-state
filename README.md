# 道 · Skill.state

Build, review and evolve reusable agent skills with measured, independently admitted changes.

- `dao-skill-state/` — the skill itself (install by linking this folder into your agent's skills directory).
- `skill-hub/` — thin entry skill that finds and loads hosted skills on demand (needs the binary below).
- `crates/skill-hub/` — Rust CLI: `index` / `check` / `search` / `get` over a local skill registry, plus `hook` for prompt-time skill pre-selection (optional Jev decision model).
- `crates/dao/` — DAO supply chain CLI. `dao intake` pins a source (git commit or local root), quarantines each skill (no execution; rejects escaping/dangling symlinks, devices/FIFOs, archives, oversize) and stores it content-addressed and read-only under `objects/<sha256>` with a content manifest and a per-source lock (commit, license evidence, symlink/hardlink/exec flags). Objects are read-only by design: `chmod -R u+w <store>` before deleting a store.
- `dao scan --store DIR (DIGEST...|--all)` — local deterministic rules over a staged object (re-hashes it first). Writes `scans/<digest>.json` with BLOCK / UNRESOLVED / REVIEW_REQUIRED / CLEAN, bound to `rules_sha256`. Best-effort: CLEAN is not proof of safety (held-out red-team: 5/20 hostile skills scored CLEAN).
- `dao check [SKILL_DIR]` — Rust port of `dao-skill-state/scripts/check_skill.py` (same JSON, messages and exit codes 0/1/2, PyYAML 1.1 semantics). Gate: `tests/fixtures/check_skill.json` (517 frozen Python results) in `cargo test`; live differential: `python3 -I crates/dao/tools/check_skill_diff.py --dao <bin> --checker dao-skill-state/scripts/check_skill.py --work <tmp> --generated --corpus <dir-of-skills>`.
- `dao normalize --store DIR (DIGEST...|--all)` / `dao relations --store DIR` — verbatim routing cards (`cards/<digest>.json`: name, description, use-when / not-for sentences, invocation lines, headings, language from the source path's locale or the script) and evidence-carrying relations (`relations.json`: same_content, same_entry, translation, drift, name_conflict, near_duplicate, family, references). Nothing is generated, merged or renamed.
- skill-hub dense recall (optional): `skill-hub index … --embed` writes `<registry>.emb` (bge-m3 via a local Ollama, loopback only). `search` and `hook` then fuse BM25 with dense recall by RRF and keep a hit whose BM25 clears the floor or whose cosine ≥ 0.6 (`SKILL_HUB_DENSE_FLOOR`); no sidecar, a mismatched sidecar, another model, or Ollama down/slow (`SKILL_HUB_EMBED_TIMEOUT_MS`, default 2000) → BM25 only.
- CI runs the skill's own format check and helper tests, plus fmt/clippy/tests for the Rust crate on Linux and macOS, on every pull request.

Licensed under MIT; see `dao-skill-state/NOTICE.md` for attribution.
