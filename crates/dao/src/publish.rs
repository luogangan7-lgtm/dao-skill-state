//! publish — install champion objects into a host skill directory, versioned and reversible.
//!
//! Depends on: quarantine (Manifest, verify_object), admission (champion pointers, durable_write,
//! valid_name).
//! Called by: `main` (`dao publish`).
//! Invariants:
//! - Dry run unless `--apply`. Only `<target>/<name>/` directories listed in the publication
//!   lock are ever replaced or removed. A directory with a champion's name that the lock does
//!   not list, or a listed one edited by hand since it was published, is a conflict. Any conflict
//!   refuses the whole apply, and nothing is written.
//! - Every copy is checked against the object's manifest before it is swapped in. The lock
//!   (`<target>/.dao-publication.json`) is written last and durably, with the whole version
//!   history. A crash between swaps re-plans cleanly: a directory that already equals its
//!   target object counts as unchanged.
//! - `--rollback` publishes the previous version's exact set from the immutable store objects,
//!   as a new version, so rolling back can itself be rolled back.

use crate::quarantine::{digest_of, verify_object, Manifest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const LOCK: &str = ".dao-publication.json";
const SCHEMA: &str = "dao-publication/1";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Version {
    pub version: u64,
    /// name -> digest, the complete published set at this version
    pub published: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Lock {
    pub schema: String,
    pub history: Vec<Version>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Action {
    pub name: String,
    /// add | update | remove | unchanged | conflict
    pub action: String,
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub fn read_lock(target: &Path) -> Result<Lock, String> {
    match fs::read(target.join(LOCK)) {
        Ok(b) => {
            let l: Lock = serde_json::from_slice(&b)
                .map_err(|e| format!("{}: {e}", target.join(LOCK).display()))?;
            if l.schema != SCHEMA {
                return Err(format!("{LOCK}: unknown schema {}", l.schema));
            }
            Ok(l)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock {
            schema: SCHEMA.into(),
            history: vec![],
        }),
        Err(e) => Err(format!("{}: {e}", target.join(LOCK).display())),
    }
}

fn current(lock: &Lock) -> BTreeMap<String, String> {
    lock.history
        .last()
        .map(|v| v.published.clone())
        .unwrap_or_default()
}

/// What `--apply` would publish: the current set with every champion's digest laid over it
/// (`rollback` instead: the previous version's set exactly).
pub fn wanted(
    store: &Path,
    lock: &Lock,
    rollback: bool,
) -> Result<BTreeMap<String, String>, String> {
    if rollback {
        let n = lock.history.len();
        return (n >= 2)
            .then(|| lock.history[n - 2].published.clone())
            .ok_or_else(|| "no previous publication to roll back to".to_string());
    }
    let mut want = current(lock);
    let dir = store.join("champions");
    let rd = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(want),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for e in rd {
        let p = e.map_err(|e| e.to_string())?.path();
        let Some(name) = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
        else {
            continue;
        };
        if let Some(c) = crate::admission::champion(store, name)? {
            if c.name != name {
                return Err(format!("champions/{name}.json names {:?}", c.name));
            }
            want.insert(c.name, c.digest);
        }
    }
    Ok(want)
}

fn manifest(store: &Path, digest: &str) -> Result<Manifest, String> {
    if !crate::scan::is_digest(digest) {
        return Err(format!("not a digest: {digest}"));
    }
    let m: Manifest = fs::read(store.join("manifests").join(format!("{digest}.json")))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        .map_err(|e| format!("manifest {}: {e}", &digest[..12]))?;
    // the manifest must hash to the digest, or a rewritten manifest could vouch for any bytes
    let mut sorted = m.files.clone();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    if m.digest != digest || sorted != m.files || digest_of(&sorted) != digest {
        return Err(format!(
            "manifest {} does not hash to its digest",
            &digest[..12]
        ));
    }
    Ok(m)
}

/// `Some(true)` the directory equals the object, `Some(false)` it differs, `None` it is absent.
/// A symlink or non-directory counts as different.
fn holds(dir: &Path, m: &Manifest) -> Option<bool> {
    let meta = fs::symlink_metadata(dir).ok()?;
    Some(meta.is_dir() && verify_object(dir, m).is_ok())
}

pub fn plan(
    store: &Path,
    target: &Path,
    lock: &Lock,
    want: &BTreeMap<String, String>,
) -> Vec<Action> {
    let cur = current(lock);
    let mut out = Vec::new();
    let act = |name: &str, action: &str, digest: Option<&String>, reason: Option<String>| Action {
        name: name.into(),
        action: action.into(),
        digest: digest.cloned(),
        reason,
    };
    // two names that differ only in case are one directory on case-insensitive filesystems
    let mut folded: BTreeMap<String, usize> = BTreeMap::new();
    for n in want.keys() {
        *folded.entry(n.to_lowercase()).or_default() += 1;
    }
    for (name, digest) in want {
        if folded[&name.to_lowercase()] > 1 {
            let why = "another published name differs from it only in case".to_string();
            out.push(act(name, "conflict", Some(digest), Some(why)));
            continue;
        }
        if let Err(e) = crate::admission::valid_name(name) {
            out.push(act(name, "conflict", Some(digest), Some(e)));
            continue;
        }
        let m = match manifest(store, digest)
            .and_then(|m| verify_object(&store.join("objects").join(digest), &m).map(|_| m))
        {
            Ok(m) => m,
            Err(e) => {
                out.push(act(name, "conflict", Some(digest), Some(e)));
                continue;
            }
        };
        let dir = target.join(name);
        let was = cur.get(name).and_then(|d| manifest(store, d).ok());
        out.push(match holds(&dir, &m) {
            None => act(name, "add", Some(digest), None),
            Some(true) => act(name, "unchanged", Some(digest), None),
            Some(false) if was.as_ref().is_some_and(|w| holds(&dir, w) == Some(true)) => {
                act(name, "update", Some(digest), None)
            }
            Some(false) => act(
                name,
                "conflict",
                Some(digest),
                Some(format!(
                    "{} exists and is not the copy dao published (unmanaged or edited by hand)",
                    dir.display()
                )),
            ),
        });
    }
    for (name, digest) in cur.iter().filter(|(n, _)| !want.contains_key(*n)) {
        let dir = target.join(name);
        let ok = crate::admission::valid_name(name).is_ok()
            && manifest(store, digest).is_ok_and(|m| holds(&dir, &m) != Some(false));
        out.push(if ok {
            act(name, "remove", Some(digest), None)
        } else {
            act(
                name,
                "conflict",
                Some(digest),
                Some(format!(
                    "{} was edited by hand; not removing it",
                    dir.display()
                )),
            )
        });
    }
    out
}

#[cfg(unix)]
fn set_mode(p: &Path, x: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(if x { 0o755 } else { 0o644 }))
}
#[cfg(not(unix))]
fn set_mode(_: &Path, _: bool) -> std::io::Result<()> {
    Ok(())
}

/// Copy an object into `stage`, file by file from its manifest, and check the copy.
fn copy_object(store: &Path, m: &Manifest, stage: &Path) -> Result<(), String> {
    let obj = store.join("objects").join(&m.digest);
    for f in &m.files {
        let rel = Path::new(&f.path);
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(format!("manifest path {} is not plain relative", f.path));
        }
        let to = stage.join(rel);
        fs::create_dir_all(to.parent().unwrap())
            .and_then(|_| fs::copy(obj.join(rel), &to))
            .and_then(|_| set_mode(&to, f.executable))
            .map_err(|e| format!("{}: {e}", to.display()))?;
    }
    verify_object(stage, m)
}

fn scratch(target: &Path, kind: &str, name: &str) -> PathBuf {
    target.join(format!(".dao-{kind}-{name}-{}", std::process::id()))
}

/// Carry out `actions` (none may be a conflict) and record the new version.
pub fn apply(
    store: &Path,
    target: &Path,
    mut lock: Lock,
    want: &BTreeMap<String, String>,
    actions: &[Action],
) -> Result<Lock, String> {
    if let Some(c) = actions.iter().find(|a| a.action == "conflict") {
        return Err(format!(
            "{}: {}; nothing was published",
            c.name,
            c.reason.as_deref().unwrap_or("conflict")
        ));
    }
    fs::create_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;
    // scratch left by an interrupted run (single writer: the caller holds the target lock)
    for e in fs::read_dir(target).map_err(|e| format!("{}: {e}", target.display()))? {
        let p = e.map_err(|e| e.to_string())?.path();
        let n = p.file_name().unwrap_or_default().to_string_lossy();
        if n.starts_with(".dao-stage-") || n.starts_with(".dao-old-") {
            fs::remove_dir_all(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    for a in actions {
        let dir = target.join(&a.name);
        let old = scratch(target, "old", &a.name);
        match a.action.as_str() {
            "add" | "update" => {
                let m = manifest(store, a.digest.as_deref().unwrap())?;
                let stage = scratch(target, "stage", &a.name);
                let _ = fs::remove_dir_all(&stage);
                if let Err(e) = copy_object(store, &m, &stage) {
                    let _ = fs::remove_dir_all(&stage);
                    return Err(e);
                }
                if a.action == "update" {
                    fs::rename(&dir, &old).map_err(|e| format!("{}: {e}", dir.display()))?;
                }
                fs::rename(&stage, &dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                let _ = fs::remove_dir_all(&old);
            }
            "remove" if fs::symlink_metadata(&dir).is_ok() => {
                fs::rename(&dir, &old).map_err(|e| format!("{}: {e}", dir.display()))?;
                let _ = fs::remove_dir_all(&old);
            }
            _ => {}
        }
    }
    if current(&lock) != *want {
        let version = lock.history.last().map_or(1, |v| v.version + 1);
        lock.history.push(Version {
            version,
            published: want.clone(),
        });
        let body = serde_json::to_vec_pretty(&lock).unwrap();
        crate::admission::durable_write(&target.join(LOCK), &body)?;
    }
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store with two staged objects (two versions of skill "s") built through `stage`.
    fn store_with_two_versions(root: &Path) -> (PathBuf, String, String) {
        let store = root.join("store");
        let mut digests = vec![];
        for (i, body) in ["one", "two"].iter().enumerate() {
            let src = root.join(format!("src{i}/s"));
            fs::create_dir_all(src.join("scripts")).unwrap();
            fs::write(
                src.join("SKILL.md"),
                format!("---\nname: s\ndescription: {body}\n---\n# s\n"),
            )
            .unwrap();
            fs::write(src.join("scripts/run.sh"), "echo hi\n").unwrap();
            set_mode(&src.join("scripts/run.sh"), true).unwrap();
            let lock = crate::source::SourceLock {
                kind: "local".into(),
                origin: src.to_string_lossy().into_owned(),
                requested_rev: None,
                commit: None,
                subpath: ".".into(),
                fetched_at_unix: 0,
                license: crate::source::LicenseEvidence {
                    file: None,
                    sha256: None,
                    spdx_guess: None,
                    declared: None,
                    status: "unknown".into(),
                },
            };
            let lim = crate::quarantine::Limits::default();
            let out = crate::quarantine::stage(&src, &src, ".", &store, &lim, &lock).unwrap();
            match out {
                crate::quarantine::Outcome::Staged { digest, .. } => digests.push(digest),
                o => panic!("{o:?}"),
            }
        }
        (store, digests[0].clone(), digests[1].clone())
    }

    fn champion(store: &Path, d: &str) {
        let p = crate::admission::Pointer {
            name: "s".into(),
            digest: d.into(),
            history: vec![],
        };
        fs::create_dir_all(store.join("champions")).unwrap();
        fs::write(
            store.join("champions/s.json"),
            serde_json::to_vec(&p).unwrap(),
        )
        .unwrap();
    }

    fn run(store: &Path, target: &Path, rollback: bool) -> Result<Vec<Action>, String> {
        let lock = read_lock(target)?;
        let want = wanted(store, &lock, rollback)?;
        let acts = plan(store, target, &lock, &want);
        apply(store, target, lock, &want, &acts)?;
        Ok(acts)
    }

    fn kinds(a: &[Action]) -> Vec<String> {
        a.iter()
            .map(|a| format!("{}:{}", a.name, a.action))
            .collect()
    }

    #[test]
    fn publishes_updates_refuses_hand_edits_and_rolls_back() {
        let root = std::env::temp_dir().join(format!("dao-publish-{}", std::process::id()));
        let _ = crate::quarantine::make_writable(&root);
        let _ = fs::remove_dir_all(&root);
        let (store, d1, d2) = store_with_two_versions(&root);
        let target = root.join("target");
        // an unrelated skill the user installed by hand is never touched
        fs::create_dir_all(target.join("mine")).unwrap();
        fs::write(target.join("mine/SKILL.md"), "x").unwrap();

        champion(&store, &d1);
        let lock = read_lock(&target).unwrap();
        let want = wanted(&store, &lock, false).unwrap();
        assert_eq!(kinds(&plan(&store, &target, &lock, &want)), ["s:add"]);
        assert!(!target.join("s").exists(), "planning writes nothing");
        assert_eq!(kinds(&run(&store, &target, false).unwrap()), ["s:add"]);
        let m1 = manifest(&store, &d1).unwrap();
        assert_eq!(holds(&target.join("s"), &m1), Some(true), "exec bit kept");
        assert_eq!(
            kinds(&run(&store, &target, false).unwrap()),
            ["s:unchanged"]
        );
        assert_eq!(
            read_lock(&target).unwrap().history.len(),
            1,
            "no-op adds no version"
        );

        champion(&store, &d2);
        assert_eq!(kinds(&run(&store, &target, false).unwrap()), ["s:update"]);
        assert_eq!(read_lock(&target).unwrap().history.len(), 2);

        assert_eq!(kinds(&run(&store, &target, true).unwrap()), ["s:update"]);
        assert_eq!(
            holds(&target.join("s"), &m1),
            Some(true),
            "rolled back to v1"
        );
        assert_eq!(read_lock(&target).unwrap().history.len(), 3);

        // a hand edit is a conflict and the whole apply is refused
        fs::write(target.join("s/SKILL.md"), "edited").unwrap();
        let e = run(&store, &target, false).unwrap_err();
        assert!(e.contains("edited by hand"), "{e}");
        assert_eq!(
            fs::read_to_string(target.join("s/SKILL.md")).unwrap(),
            "edited"
        );
        // a same-named directory dao never published is not taken over
        let t2 = root.join("t2");
        fs::create_dir_all(t2.join("s")).unwrap();
        assert!(run(&store, &t2, false).is_err());
        assert_eq!(
            fs::read_to_string(target.join("mine/SKILL.md")).unwrap(),
            "x"
        );
        // rollback after a first publish has nothing to go back to
        let t3 = root.join("t3");
        run(&store, &t3, false).unwrap();
        assert!(run(&store, &t3, true).is_err());

        let _ = crate::quarantine::make_writable(&root);
        let _ = fs::remove_dir_all(&root);
        assert!(!root.exists());
    }

    #[test]
    fn manifests_must_hash_to_their_digest_and_names_must_not_fold_together() {
        let root = std::env::temp_dir().join(format!("dao-publish-fold-{}", std::process::id()));
        let _ = crate::quarantine::make_writable(&root);
        let _ = fs::remove_dir_all(&root);
        let (store, d1, d2) = store_with_two_versions(&root);
        // a manifest rewritten to vouch for other bytes is refused
        let mp = store.join("manifests").join(format!("{d1}.json"));
        let _ = crate::quarantine::make_writable(&mp);
        let mut m: Manifest = serde_json::from_slice(&fs::read(&mp).unwrap()).unwrap();
        m.files[0].size += 1;
        fs::write(&mp, serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(manifest(&store, &d1).unwrap_err().contains("does not hash"));
        // Skill and skill would share one directory on a case-insensitive filesystem
        let want = BTreeMap::from([("Skill".to_string(), d2.clone()), ("skill".to_string(), d2)]);
        let lock = read_lock(&root.join("t")).unwrap();
        let acts = plan(&store, &root.join("t"), &lock, &want);
        assert!(acts.iter().all(|a| a.action == "conflict"), "{acts:?}");
        let _ = crate::quarantine::make_writable(&root);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rollback_removes_what_the_newer_version_added() {
        let root = std::env::temp_dir().join(format!("dao-publish-rm-{}", std::process::id()));
        let _ = crate::quarantine::make_writable(&root);
        let _ = fs::remove_dir_all(&root);
        let (store, d1, _) = store_with_two_versions(&root);
        let target = root.join("target");
        // v1: empty set written by hand-free means: publish nothing first, then add s
        fs::create_dir_all(&target).unwrap();
        let empty = Lock {
            schema: SCHEMA.into(),
            history: vec![Version {
                version: 1,
                published: BTreeMap::new(),
            }],
        };
        fs::write(target.join(LOCK), serde_json::to_vec(&empty).unwrap()).unwrap();
        champion(&store, &d1);
        assert_eq!(kinds(&run(&store, &target, false).unwrap()), ["s:add"]);
        // a hand-edited directory is not removed by a rollback
        fs::write(target.join("s/SKILL.md"), "edited").unwrap();
        assert!(run(&store, &target, true)
            .unwrap_err()
            .contains("edited by hand"));
        assert_eq!(
            fs::read_to_string(target.join("s/SKILL.md")).unwrap(),
            "edited"
        );
        let m1 = manifest(&store, &d1).unwrap();
        fs::remove_dir_all(target.join("s")).unwrap();
        copy_object(&store, &m1, &target.join("s")).unwrap();
        // leftovers of an interrupted apply are swept
        fs::create_dir_all(target.join(".dao-stage-s-1/x")).unwrap();
        assert_eq!(kinds(&run(&store, &target, true).unwrap()), ["s:remove"]);
        assert!(!target.join("s").exists());
        assert!(!target.join(".dao-stage-s-1").exists());
        let _ = crate::quarantine::make_writable(&root);
        let _ = fs::remove_dir_all(&root);
    }
}
