//! upstream — which locked sources have moved since intake. Never touches champions.
//!
//! Depends on: source (remote_refs / resolve_ref / validate_origin), quarantine (inspect),
//! scan (is_digest).
//! Called by: `main` (`dao upstream check [--apply] [--allow-local DIR]...`), which re-intakes
//! changed sources as new candidates through the normal intake path.
//! Invariants:
//! - Read-only unless `--apply`; even then only new candidates are staged (new digests). No
//!   existing object, manifest, card, receipt or champion pointer is modified, nor any lock of
//!   another ref; the one exception is a ref moved back to a commit it had before, whose own
//!   lock for that commit is rewritten with a new fetch time. A newer upstream is a candidate
//!   that must pass admission like any other. Refs outside HEAD, branches and tags (e.g.
//!   pull/N/head) are not followed.
//! - Lock files are store data and may be tampered with: only `<16 hex>.json` regular files of
//!   at most 64 KiB are read; a bad lock is an error for that item, never for the whole run;
//!   subpaths must be relative without `..`; local paths (a `local` origin, or a git origin that
//!   is a local path) are only read when the caller allows their root with `--allow-local`.
//! - The newest lock per (origin, requested_rev, subpath) is the reference point; pinned
//!   commits (a full SHA requested) have no upstream and are reported as such.

use crate::quarantine::{inspect, Limits};
use crate::source::{remote_refs, resolve_ref, validate_origin};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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
    /// unchanged | changed | pinned | skipped | error
    pub status: String,
    pub upstream: Option<String>,
    pub error: Option<String>,
}

const LOCK_MAX: u64 = 64 * 1024;

fn lock_name_ok(name: &str) -> bool {
    name.len() == 21 && name.ends_with(".json") && name[..16].bytes().all(|b| b.is_ascii_hexdigit())
}

fn subpath_ok(p: &str) -> bool {
    !Path::new(p).is_absolute() && !p.split('/').any(|c| c == "..")
}

/// Newest lock per (origin, requested_rev, subpath), plus one error report per unreadable lock.
pub fn tracked(store: &Path) -> Result<(Vec<Tracked>, Vec<Report>), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut latest: BTreeMap<(String, Option<String>, String), Tracked> = BTreeMap::new();
    let mut bad = Vec::new();
    let locks = store.join("locks");
    let Ok(dirs) = fs::read_dir(&locks) else {
        return Ok((Vec::new(), Vec::new())); // nothing staged yet
    };
    for d in dirs.flatten() {
        let digest = d.file_name().to_string_lossy().into_owned();
        if !crate::scan::is_digest(&digest) {
            continue;
        }
        let Ok(files) = fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if !lock_name_ok(&name) {
                continue; // .DS_Store, temp files of an interrupted write, …
            }
            let err = |e: String| Report {
                kind: String::new(),
                origin: String::new(),
                requested_rev: None,
                subpath: String::new(),
                locked_commit: None,
                locked_digest: digest.clone(),
                status: "error".into(),
                upstream: None,
                error: Some(format!("lock {name}: {e}")),
            };
            let meta = match fs::symlink_metadata(f.path()) {
                Ok(m) if m.is_file() && m.len() <= LOCK_MAX => m,
                Ok(_) => {
                    bad.push(err("not a regular file of at most 64 KiB".into()));
                    continue;
                }
                Err(e) => {
                    bad.push(err(e.to_string()));
                    continue;
                }
            };
            let v: serde_json::Value = match fs::read(f.path())
                .map_err(|e| e.to_string())
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
            {
                Ok(v) => v,
                Err(e) => {
                    bad.push(err(e));
                    continue;
                }
            };
            let Some(origin) = v["origin"].as_str() else {
                bad.push(err("no origin".into()));
                continue;
            };
            let written_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos())
                // a skewed clock can stamp the file in the future too
                .min(u128::from(now) * 1_000_000_000 + 999_999_999);
            let t = Tracked {
                kind: v["kind"].as_str().unwrap_or("").into(),
                origin: origin.into(),
                requested_rev: v["requested_rev"].as_str().map(str::to_string),
                commit: v["commit"].as_str().map(str::to_string),
                subpath: v["subpath"].as_str().unwrap_or("").into(),
                digest: digest.clone(),
                // a lock "from the future" (clock skew) must not shadow every later intake
                fetched_at_unix: v["fetched_at_unix"].as_u64().unwrap_or(0).min(now),
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
    Ok((latest.into_values().collect(), bad))
}

/// Whether a local path may be read: under one of the caller-allowed roots (canonicalized).
fn local_allowed(path: &str, allow: &[PathBuf]) -> bool {
    fs::canonicalize(path).is_ok_and(|p| allow.iter().any(|a| p.starts_with(a)))
}

/// Compare each tracked path with its source now: git by the remote commit of the requested
/// ref (one `ls-remote` per origin), local by re-inspecting the directory (no copy).
pub fn check(items: &[Tracked], allow_local: &[PathBuf]) -> Vec<Report> {
    let mut refs: BTreeMap<String, Result<Vec<(String, String)>, String>> = BTreeMap::new();
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
            let is_local_path = Path::new(&t.origin).is_absolute();
            let now: Result<String, String> = if !subpath_ok(&t.subpath) {
                Err("lock subpath is absolute or contains '..'".into())
            } else if is_local_path && !local_allowed(&t.origin, allow_local) {
                r.status = "skipped".into();
                r.error = Some("local origin not allowed (pass --allow-local <root>)".into());
                return r;
            } else {
                match (t.kind.as_str(), &t.requested_rev) {
                    ("git", Some(rev)) if crate::source::is_full_commit(rev) => {
                        r.status = "pinned".into();
                        return r;
                    }
                    ("git", Some(rev)) => refs
                        .entry(t.origin.clone())
                        .or_insert_with(|| remote_refs(&t.origin))
                        .clone()
                        .and_then(|rs| {
                            resolve_ref(&rs, rev).ok_or(format!("{}: no ref named {rev}", t.origin))
                        }),
                    ("git", None) => Err("git lock without a requested ref".into()),
                    ("local", _) => {
                        let root = Path::new(&t.origin);
                        validate_origin(&t.origin).and_then(|_| {
                            inspect(&root.join(&t.subpath), root, &Limits::default())
                                .map(|(m, _)| m.digest)
                                .map_err(|e| e.join("; "))
                        })
                    }
                    (k, _) => Err(format!("unsupported lock kind {k:?}")),
                }
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

    fn walk_locks(store: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for d in fs::read_dir(store.join("locks")).unwrap().flatten() {
            for f in fs::read_dir(d.path()).unwrap().flatten() {
                out.push((f.path(), fs::read(f.path()).unwrap()));
            }
        }
        out
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
        let allow = vec![fs::canonicalize(&root).unwrap()];
        let run = |extra: &[&str]| {
            let mut args = vec!["--store".to_string(), store.to_string_lossy().into_owned()];
            args.extend(extra.iter().map(|s| s.to_string()));
            crate::intake(&args).unwrap();
        };
        run(&["--git", &origin, "--rev", "main", "--path", "skills/a"]);
        // a second ref at the same commit is its own lock (it used to overwrite main's)
        git(&repo, &["tag", "rel"]);
        run(&["--git", &origin, "--rev", "rel", "--path", "skills/a"]);
        let (first, bad) = tracked(&store).unwrap();
        assert_eq!((first.len(), bad.len()), (2, 0));
        assert!(check(&first, &allow)
            .iter()
            .all(|r| r.status == "unchanged"));
        // without --allow-local a local-path origin taken from a lock is never read
        assert!(check(&first, &[]).iter().all(|r| r.status == "skipped"));
        // main moves (rel stays): reported, and --apply stages one new object, touching nothing
        fs::write(
            repo.join("skills/a/SKILL.md"),
            "---\nname: a\ndescription: two\n---\nv2\n",
        )
        .unwrap();
        git(&repo, &["commit", "-qam", "v2"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let r = check(&first, &allow);
        let changed: Vec<_> = r.iter().filter(|x| x.status == "changed").collect();
        assert_eq!(changed.len(), 1, "{r:?}");
        assert_eq!(changed[0].upstream.as_deref(), Some(head.as_str()));
        let objects = |s: &Path| {
            fs::read_dir(s.join("objects"))
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect::<Vec<_>>()
        };
        let (before, locks_before) = (objects(&store), walk_locks(&store));
        let args = [
            "check",
            "--store",
            &store.to_string_lossy(),
            "--apply",
            "--allow-local",
            &root.to_string_lossy(),
        ]
        .map(String::from);
        crate::upstream_cmd(&args).unwrap();
        let after = objects(&store);
        assert_eq!(after.len(), before.len() + 1);
        assert!(
            before.iter().all(|b| after.contains(b)),
            "old candidate kept"
        );
        for (p, bytes) in &locks_before {
            assert_eq!(&fs::read(p).unwrap(), bytes, "{} changed", p.display());
        }
        let (now, _) = tracked(&store).unwrap();
        assert_eq!(now.len(), 2, "main and rel both tracked");
        assert!(check(&now, &allow).iter().all(|r| r.status == "unchanged"));
        let main = now
            .iter()
            .find(|t| t.requested_rev.as_deref() == Some("main"))
            .unwrap();
        assert_eq!(main.commit.as_deref(), Some(head.as_str()));
        // a pinned commit has no upstream; a vanished ref is an error, not "unchanged"
        let mut pinned = main.clone();
        pinned.requested_rev = Some(head.clone());
        assert_eq!(check(&[pinned], &allow)[0].status, "pinned");
        let mut gone = main.clone();
        gone.requested_rev = Some("no-such-branch".into());
        assert_eq!(check(&[gone], &allow)[0].status, "error");
        let mut esc = main.clone();
        esc.subpath = "../outside".into();
        assert_eq!(check(&[esc], &allow)[0].status, "error");
        // junk and a corrupt lock in a lock dir are per-item problems, not a failed run
        let d = fs::read_dir(store.join("locks"))
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path();
        fs::write(d.join(".DS_Store"), "x").unwrap();
        fs::write(d.join("0123456789abcdef.json"), "{bad").unwrap();
        let (items, bad) = tracked(&store).unwrap();
        assert_eq!((items.len(), bad.len()), (2, 1));
        // git resolves a short name tag-first; so does upstream
        let refs = vec![
            ("c1".to_string(), "refs/tags/v1".to_string()),
            ("c2".to_string(), "refs/heads/v1".to_string()),
        ];
        assert_eq!(
            crate::source::resolve_ref(&refs, "v1").as_deref(),
            Some("c1")
        );
        let _ = Command::new("chmod")
            .args(["-R", "u+w"])
            .arg(&root)
            .status();
        let _ = fs::remove_dir_all(&root);
    }
}
