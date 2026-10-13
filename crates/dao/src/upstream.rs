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
//!   that must pass admission like any other. A local source whose license evidence changed
//!   (same content) is `changed` too: re-applying it rewrites that source's own lock.
//! - Lock files are store data and may be tampered with: only `<16 hex>.json` regular files of
//!   at most 64 KiB are read; a bad lock is an error for that item, never for the whole run;
//!   subpaths must be relative without `..`; local paths (a `local` origin, or a git origin that
//!   is a local path) are only read when the caller allows their root with `--allow-local`.
//! - The newest lock per (origin, requested_rev, subpath) is the reference point; a timestamp in
//!   the future (clock skew) counts as unknown (0), so it never outranks a real one. Pinned
//!   commits (a full SHA requested, matching the locked commit) have no upstream.

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
    /// License evidence recorded at intake (file, sha256): a local source can change it alone.
    pub license: (Option<String>, Option<String>),
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
    let mut latest: BTreeMap<(String, String, Option<String>, String), Tracked> = BTreeMap::new();
    let mut bad = Vec::new();
    let locks = store.join("locks");
    let dirs = match fs::read_dir(&locks) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        // unreadable is not "nothing tracked": that would silently skip every check
        Err(e) => return Err(format!("{}: {e}", locks.display())),
    };
    for d in dirs {
        let d = d.map_err(|e| format!("{}: {e}", locks.display()))?;
        let digest = d.file_name().to_string_lossy().into_owned();
        if !crate::scan::is_digest(&digest) {
            continue;
        }
        let files = fs::read_dir(d.path())
            .and_then(|fs| fs.collect::<Result<Vec<_>, _>>())
            .map_err(|e| format!("{}: {e}", d.path().display()))?;
        for f in files {
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
            // a time in the future (skewed or restored clock) is unknown, not "newest": clamping
            // it to now would let it overtake every later lock again on the next run
            let written_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .filter(|&ns| ns < u128::from(now + 1) * 1_000_000_000)
                .unwrap_or(0);
            let t = Tracked {
                kind: v["kind"].as_str().unwrap_or("").into(),
                origin: origin.into(),
                requested_rev: v["requested_rev"].as_str().map(str::to_string),
                commit: v["commit"].as_str().map(str::to_string),
                subpath: v["subpath"].as_str().unwrap_or("").into(),
                digest: digest.clone(),
                fetched_at_unix: v["fetched_at_unix"]
                    .as_u64()
                    .filter(|&t| t <= now)
                    .unwrap_or(0),
                written_ns,
                license: (
                    v["license"]["file"].as_str().map(str::to_string),
                    v["license"]["sha256"].as_str().map(str::to_string),
                ),
            };
            // kind too: a git and a local intake of the same path are different sources
            let key = (
                t.kind.clone(),
                t.origin.clone(),
                t.requested_rev.clone(),
                t.subpath.clone(),
            );
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

/// Whether a local path may be read: `Some(Ok(canonical path))` under one of the
/// caller-allowed roots; `Some(Err)` if it cannot be resolved but its nearest existing parent is
/// allowed (vanished or unmounted: an error, not a permission skip); `None` if not allowed.
/// Read through the returned path only, so a symlink retargeted later is not followed.
pub fn local_allowed(path: &str, allow: &[PathBuf]) -> Option<Result<PathBuf, String>> {
    let under = |p: &Path| allow.iter().any(|a| p.starts_with(a));
    match fs::canonicalize(path) {
        Ok(p) => under(&p).then_some(Ok(p)),
        Err(e) => Path::new(path)
            .ancestors()
            .skip(1)
            .find_map(|a| fs::canonicalize(a).ok())
            .filter(|a| under(a))
            .map(|_| Err(format!("{path}: {e}"))),
    }
}

/// Compare each tracked path with its source now: git by the remote commit of the requested
/// ref (one `ls-remote` per origin), local by re-inspecting the directory (no copy).
pub fn check(items: &[Tracked], allow_local: &[PathBuf]) -> Vec<Report> {
    // the requested revs of each origin are queried with it (refs outside heads/tags included)
    let mut revs: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for t in items {
        if let ("git", Some(rev)) = (t.kind.as_str(), &t.requested_rev) {
            revs.entry(&t.origin).or_default().push(rev);
        }
    }
    let mut refs = BTreeMap::new(); // origin -> its ls-remote result
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
            let local = is_local_path.then(|| local_allowed(&t.origin, allow_local));
            let mut license_now = None;
            let now: Result<String, String> = if !subpath_ok(&t.subpath) {
                Err("lock subpath is absolute or contains '..'".into())
            } else if let Some(None) = local {
                r.status = "skipped".into();
                r.error = Some("local origin not allowed (pass --allow-local <root>)".into());
                return r;
            } else if let Some(Some(Err(e))) = local {
                Err(e)
            } else {
                match (t.kind.as_str(), &t.requested_rev) {
                    ("git", Some(rev)) if crate::source::is_full_commit(rev) => {
                        // a lock whose commit is not the pinned one is inconsistent, not pinned
                        if t.commit
                            .as_deref()
                            .is_some_and(|c| c.eq_ignore_ascii_case(rev))
                        {
                            r.status = "pinned".into();
                            return r;
                        }
                        Err(format!("pinned {rev} but the lock records {:?}", t.commit))
                    }
                    ("git", Some(rev)) => refs
                        .entry(&t.origin)
                        .or_insert_with(|| {
                            // a local origin is read through the path that passed --allow-local
                            let at = match &local {
                                Some(Some(Ok(p))) => p.to_string_lossy().into_owned(),
                                _ => t.origin.clone(),
                            };
                            remote_refs(&at, &revs[t.origin.as_str()])
                        })
                        .as_ref()
                        .map_err(Clone::clone)
                        .and_then(|rs| {
                            resolve_ref(rs, rev).ok_or(format!("{}: no ref named {rev}", t.origin))
                        }),
                    ("git", None) => Err("git lock without a requested ref".into()),
                    ("local", _) => {
                        let root = match &local {
                            Some(Some(Ok(p))) => p.as_path(),
                            _ => Path::new(&t.origin),
                        };
                        validate_origin(&t.origin).and_then(|_| {
                            let dir = root.join(&t.subpath);
                            let l = crate::source::detect_license(root, &dir, None);
                            license_now = Some((l.file, l.sha256));
                            inspect(&dir, root, &Limits::default())
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
                        // a repo-root LICENSE can change without the skill directory changing
                        u == t.digest && license_now.as_ref() == Some(&t.license)
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

    #[test]
    fn review_round_edge_cases() {
        let root = std::env::temp_dir().join(format!("dao-upstream-edge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (repo, store) = (root.join("repo"), root.join("store"));
        fs::create_dir_all(repo.join("skills/a")).unwrap();
        fs::write(
            repo.join("skills/a/SKILL.md"),
            "---\nname: a\ndescription: one\n---\nv1\n",
        )
        .unwrap();
        fs::write(repo.join("LICENSE"), "MIT License").unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "v1"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["tag", "-a", "v1", "-m", "v1"]);
        git(&repo, &["update-ref", "refs/pull/1/head", "HEAD"]);
        let origin = repo.to_string_lossy().into_owned();
        let allow = vec![fs::canonicalize(&root).unwrap()];
        let run = |extra: &[&str]| {
            let mut args = vec!["--store".to_string(), store.to_string_lossy().into_owned()];
            args.extend(extra.iter().map(|s| s.to_string()));
            crate::intake(&args).unwrap();
        };
        // annotated tags under every spelling, and a ref outside heads/tags, match intake
        for rev in ["v1", "tags/v1", "refs/tags/v1", "refs/pull/1/head"] {
            run(&["--git", &origin, "--rev", rev, "--path", "skills/a"]);
        }
        run(&["--local", &origin, "--path", "skills/a"]);
        let (items, bad) = tracked(&store).unwrap();
        assert_eq!((items.len(), bad.len()), (5, 0));
        let r = check(&items, &allow);
        assert!(r.iter().all(|x| x.status == "unchanged"), "{r:?}");
        // a repo-root LICENSE change alone moves the local source
        fs::write(repo.join("LICENSE"), "GNU GENERAL PUBLIC LICENSE").unwrap();
        let local: Vec<_> = items
            .iter()
            .filter(|t| t.kind == "local")
            .cloned()
            .collect();
        assert_eq!(check(&local, &allow)[0].status, "changed");
        // a pinned lock whose commit is another one is an error
        let mut pinned = items.iter().find(|t| t.kind == "git").unwrap().clone();
        pinned.requested_rev = Some(head.clone());
        pinned.commit = Some("0".repeat(40));
        assert_eq!(check(&[pinned], &allow)[0].status, "error");
        // an allowed local origin that vanished is an error; an unallowed one is skipped
        let mut gone = local[0].clone();
        gone.origin = root.join("moved").to_string_lossy().into_owned();
        assert_eq!(check(&[gone.clone()], &allow)[0].status, "error");
        assert_eq!(check(&[gone], &[])[0].status, "skipped");
        // a lock dated in the future does not outrank a real newer one
        let lock = walk_locks(&store)
            .into_iter()
            .find(|(_, b)| String::from_utf8_lossy(b).contains("\"kind\": \"local\""))
            .unwrap()
            .0;
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&lock).unwrap()).unwrap();
        v["fetched_at_unix"] = (u64::MAX / 2).into();
        let future = lock.with_file_name("fedcba9876543210.json");
        fs::write(&future, v.to_string()).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(86_400);
        fs::File::options()
            .write(true)
            .open(&future)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let mut real = v.clone();
        real["fetched_at_unix"] = 1.into();
        fs::write(&lock, real.to_string()).unwrap();
        let (items, _) = tracked(&store).unwrap();
        let t = items.iter().find(|t| t.kind == "local").unwrap();
        assert_eq!((t.fetched_at_unix, t.written_ns > 0), (1, true));
        fs::remove_file(&future).unwrap();
        // a failed --apply (the changed ref no longer has the path) fails the run
        git(&repo, &["rm", "-rq", "skills"]);
        git(&repo, &["commit", "-qm", "gone"]);
        git(&repo, &["update-ref", "refs/pull/1/head", "HEAD"]);
        let args = [
            "check",
            "--store",
            &store.to_string_lossy(),
            "--apply",
            "--allow-local",
            &root.to_string_lossy(),
        ]
        .map(String::from);
        assert_eq!(
            crate::upstream_cmd(&args).unwrap(),
            std::process::ExitCode::from(2)
        );
        // an unreadable locks/ is an error, not an empty store
        fs::remove_dir_all(store.join("locks")).unwrap();
        fs::write(store.join("locks"), "x").unwrap();
        assert!(tracked(&store).is_err());
        // store objects are read-only; make them writable so the temp tree is really removed
        let _ = Command::new("chmod")
            .args(["-R", "u+w"])
            .arg(&root)
            .status();
        let _ = fs::remove_dir_all(&root);
        assert!(!root.exists());
    }
}
