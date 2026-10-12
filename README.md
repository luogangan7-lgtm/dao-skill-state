# 道 · Skill.state

Build, review and evolve reusable agent skills with measured, independently admitted changes.

- `dao-skill-state/` — the skill itself (install by linking this folder into your agent's skills directory).
- `skill-hub/` — thin entry skill that finds and loads hosted skills on demand (needs the binary below).
- `crates/skill-hub/` — Rust CLI: `index` / `check` / `search` / `get` over a local skill registry, plus `hook` for prompt-time skill pre-selection (optional Jev decision model).
- `crates/dao/` — DAO supply chain CLI. `dao intake` pins a source (git commit or local root), quarantines each skill (no execution; rejects escaping/dangling symlinks, devices/FIFOs, archives, oversize) and stores it content-addressed and read-only under `objects/<sha256>` with a manifest and a source lock (license evidence). Objects are read-only by design: `chmod -R u+w <store>` before deleting a store.
- CI runs the skill's own format check and helper tests, plus fmt/clippy/tests for the Rust crate on Linux and macOS, on every pull request.

Licensed under MIT; see `dao-skill-state/NOTICE.md` for attribution.
