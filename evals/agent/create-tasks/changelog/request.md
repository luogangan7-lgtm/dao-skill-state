Make a skill for our team's release changelog conventions, so any agent writing a changelog follows them:

- File name: CHANGELOG-<version>.md (version without a leading "v").
- First line: `# v<version> — <YYYY-MM-DD>` (an em dash between version and date).
- Then sections, in this order, each only if it has entries: `## Added`, `## Changed`, `## Fixed`.
- Commit types map to sections: feat → Added; refactor and perf → Changed; fix → Fixed.
  docs, test, chore and ci commits are left out entirely.
- One bullet per commit: `- <subject with the first letter capitalised> (#<PR number>)`, keeping the
  commit's order within each section. No scopes, no commit hashes, no trailing period.
