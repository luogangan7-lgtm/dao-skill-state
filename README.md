# 道 · Skill.state

Build, review and evolve reusable agent skills with measured, independently admitted changes.

- `dao-skill-state/` — the skill itself (install by linking this folder into your agent's skills directory).
- `skill-hub/` — thin entry skill that finds and loads hosted skills on demand (needs the binary below).
- `crates/skill-hub/` — Rust CLI: `index` / `check` / `search` / `get` over a local skill registry.
- CI runs the skill's own format check and helper tests, plus fmt/clippy/tests for the Rust crate on Linux and macOS, on every pull request.

Licensed under MIT; see `dao-skill-state/NOTICE.md` for attribution.
