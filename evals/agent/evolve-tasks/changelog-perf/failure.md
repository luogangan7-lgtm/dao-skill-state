The skill was used for this request:

> Write the changelog for release 3.1.0, dated 2026-09-30, from the commits in commits.txt, following our
> team's changelog conventions.

commits.txt:
```
feat: add audit log export (#501)
perf(api): batch database writes (#503)
fix: correct currency rounding (#504)
```

The agent produced CHANGELOG-3.1.0.md:
```
# v3.1.0 — 2026-09-30
## Added
- Add audit log export (#501)
## Fixed
- Batch database writes (#503)
- Correct currency rounding (#504)
```

Release manager's review: "Performance work is not a bug fix. In our conventions perf commits go under
Changed, together with refactors. Everything else in this file is right."
