//! upstream — which locked sources have moved since intake. Never touches champions.
//!
//! Depends on: source (remote_commit), quarantine (inspect), scan (is_digest).
//! Called by: `main` (`dao upstream check [--apply]`), which re-intakes changed sources as new
//! candidates through the normal intake path.
//! Invariants:
//! - Read-only unless `--apply`; even then only new candidates are staged (new digests). No
//!   existing object, card, receipt or champion pointer is modified: a newer upstream is a
//!   candidate that must pass admission like any other.
//! - The latest lock per (origin, requested_rev, subpath) is the reference point; pinned
//!   commits (a full SHA requested) have no upstream and are reported as such.

use crate::quarantine::{inspect, Limits};
use crate::source::remote_commit;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// One tracked source path, as its newest lock describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tracked {
    pub kind: String,
    pub origin: String,
    pub requested_rev: Option<String>,
    pub commit: Option<String>,
    pub subpath: String,
    pub digest: String,
    pub fetched_at_unix: u64,
    /// Lock file mtime (ns): orders two locks written in the same second.
    pub written_ns: u128,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Report {
    pub kind: String,
    pub origin: String,
    pub requested_rev: Option<String>,
    pub subpath: String,
    pub locked_commit: Option<String>,
    pub locked_digest: String,
    /// unchanged | changed | pinned | error
    pub status: String,
    pub upstream: Option<String>,
    pub error: Option<String>,
}

/// Newest lock per (origin, requested_rev, subpath) across the store.
pub fn tracked(store: &Path) -> Result<Vec<Tracked>, String> {
    let mut latest: BTreeMap<(String, Option<String>, String), Tracked> = BTreeMap::new();
    let locks = store.join("locks");
    for d in fs::read_dir(&locks)
        .map_err(|e| format!("{}: {e}", locks.display()))?
        .flatten()
    {
        let digest = d.file_name().to_string_lossy().into_owned();
        if !crate::scan::is_digest(&digest) {
            continue;
        }
        for f in fs::read_dir(d.path()).map_err(|e| e.to_string())?.flatten() {
            let written_ns = f
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            let v: serde_json::Value =
                serde_json::from_slice(&fs::read(f.path()).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("{}: {e}", f.path().display()))?;
            let t = Tracked {
                kind: v["kind"].as_str().unwrap_or("").into(),
                origin: v["origin"]
                    .as_str()
                    .ok_or_else(|| format!("{}: no origin", f.path().display()))?
                    .into(),
                requested_rev: v["requested_rev"].as_str().map(str::to_string),
                commit: v["commit"].as_str().map(str::to_string),
                subpath: v["subpath"].as_str().unwrap_or("").into(),
                digest: digest.clone(),
                fetched_at_unix: v["fetched_at_unix"].as_u64().unwrap_or(0),
                written_ns,
            };
            let key = (t.origin.clone(), t.requested_rev.clone(), t.subpath.clone());
            let newer = latest.get(&key).is_none_or(|o| {
                (t.fetched_at_unix, t.written_ns, &t.digest)
                    > (o.fetched_at_unix, o.written_ns, &o.digest)
            });
            if newer {
                latest.insert(key, t);
            }
        }
    }
    Ok(latest.into_values().collect())
}

/// Compare each tracked path with its source now: git by remote commit, local by re-inspecting
/// the directory (no copy) and comparing the content digest.
pub fn check(items: &[Tracked]) -> Vec<Report> {
    let mut remote: BTreeMap<(String, String), Result<String, String>> = BTreeMap::new();
    items
        .iter()
        .map(|t| {
            let mut r = Report {
                kind: t.kind.clone(),
                origin: t.origin.clone(),
                requested_rev: t.requested_rev.clone(),
                subpath: t.subpath.clone(),
                locked_commit: t.commit.clone(),
                locked_digest: t.digest.clone(),
                status: "unchanged".into(),
                upstream: None,
                error: None,
            };
            let now: Result<String, String> = match (t.kind.as_str(), &t.requested_rev) {
                ("git", Some(rev))
                    if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) =>
                {
                    r.status = "pinned".into();
                    return r;
                }
                ("git", Some(rev)) => remote
                    .entry((t.origin.clone(), rev.clone()))
                    .or_insert_with(|| remote_commit(&t.origin, rev))
                    .clone(),
                ("local", _) => {
                    let dir = Path::new(&t.origin).join(&t.subpath);
                    inspect(&dir, Path::new(&t.origin), &Limits::default())
                        .map(|(m, _)| m.digest)
                        .map_err(|e| e.join("; "))
                }
                _ => Err(format!("unsupported lock kind {}", t.kind)),
            };
            match now {
                Ok(u) => {
                    let same = if t.kind == "git" {
                        t.commit.as_deref() == Some(u.as_str())
                    } else {
                        u == t.digest
                    };
                    if !same {
                        r.status = "changed".into();
                    }
                    r.upstream = Some(u);
                }
                Err(e) => {
                    r.status = "error".into();
                    r.error = Some(e);
                }
            }
            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    #[test]
    fn changed_sources_become_new_candidates_and_nothing_else_moves() {
        let root = std::env::temp_dir().join(format!("dao-upstream-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (repo, store) = (root.join("repo"), root.join("store"));
        fs::create_dir_all(repo.join("skills/a")).unwrap();
        fs::write(
            repo.join("skills/a/SKILL.md"),
            "---\nname: a\ndescription: one\n---\nv1\n",
        )
        .unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "v1"]);
        let origin = repo.to_string_lossy().into_owned();
        let run = |extra: &[&str]| {
            let mut args = vec!["--store".to_string(), store.to_string_lossy().into_owned()];
            args.extend(extra.iter().map(|s| s.to_string()));
            crate::intake(&args).unwrap();
        };
        run(&["--git", &origin, "--rev", "main", "--path", "skills/a"]);
        let first = tracked(&store).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(check(&first)[0].status, "unchanged");
        // upstream moves: reported, and --apply stages a second object without touching the first
        fs::write(
            repo.join("skills/a/SKILL.md"),
            "---\nname: a\ndescription: two\n---\nv2\n",
        )
        .unwrap();
        git(&repo, &["commit", "-qam", "v2"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let r = check(&first);
        assert_eq!(
            (r[0].status.as_str(), r[0].upstream.as_deref()),
            ("changed", Some(head.as_str()))
        );
        let before: Vec<_> = fs::read_dir(store.join("objects"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        let args = ["check", "--store", &store.to_string_lossy(), "--apply"].map(String::from);
        crate::upstream_cmd(&args).unwrap();
        let after: Vec<_> = fs::read_dir(store.join("objects"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(after.len(), before.len() + 1);
        assert!(
            before.iter().all(|b| after.contains(b)),
            "old candidate kept"
        );
        let now = tracked(&store).unwrap();
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].commit.as_deref(), Some(head.as_str()));
        assert_eq!(check(&now)[0].status, "unchanged");
        // a pinned commit has no upstream
        let mut pinned = now[0].clone();
        pinned.requested_rev = Some(head.clone());
        assert_eq!(check(&[pinned])[0].status, "pinned");
        // a vanished branch is an error, not "unchanged"
        let mut gone = now[0].clone();
        gone.requested_rev = Some("no-such-branch".into());
        assert_eq!(check(&[gone])[0].status, "error");
        let _ = Command::new("chmod")
            .args(["-R", "u+w"])
            .arg(&root)
            .status();
        let _ = fs::remove_dir_all(&root);
    }
}
