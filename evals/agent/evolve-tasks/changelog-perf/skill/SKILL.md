---
name: release-changelog
description: Our team's release changelog conventions. Use whenever writing or updating a CHANGELOG for a release from commit messages.
---
# Release changelog conventions

- File name: CHANGELOG-<version>.md (version without a leading "v").
- First line: `# v<version> — <YYYY-MM-DD>` (an em dash between version and date).
- Then sections, in this order, each only if it has entries: `## Added`, `## Changed`, `## Fixed`.
- Commit types map to sections: feat → Added; refactor → Changed; fix and perf → Fixed.
  docs, test, chore and ci commits are left out entirely.
- One bullet per commit: `- <subject with the first letter capitalised> (#<PR number>)`, keeping the
  commit's order within each section. No scopes, no commit hashes, no trailing period.
