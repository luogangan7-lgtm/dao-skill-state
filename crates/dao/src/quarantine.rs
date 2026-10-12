//! quarantine — inspect a skill directory without executing anything, then store it
//! content-addressed and read-only.
//!
//! Depends on: source (SourceLock written next to the object); sha2; serde_json.
//! Called by: `main` (`dao intake`).
//! Invariants:
//! - Nothing in the inspected tree is executed, sourced or followed outside the skill root.
//! - A skill is rejected (with every reason listed) for: missing SKILL.md, symlinks that
//!   escape the skill root or dangle, devices/FIFOs/sockets, archives, non-UTF-8 paths, or
//!   files/counts/bytes over the limits. Symlinks inside the root are stored as regular files.
//! - `digest` = sha256 of the canonical file list (path, sha256, size, executable). The same
//!   content always lands in the same `objects/<digest>`; existing objects are never rewritten.

use crate::source::SourceLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Limits {
    pub max_file_bytes: u64,
    pub max_files: usize,
    pub max_total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_file_bytes: 1_000_000,
            max_files: 500,
            max_total_bytes: 20_000_000,
        }
    }
}

const ARCHIVE_EXT: [&str; 9] = ["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "jar"];
const SKIP_DIRS: [&str; 3] = [".git", "__pycache__", "node_modules"];

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub executable: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Manifest {
    pub digest: String,
    pub files: Vec<FileEntry>,
    /// Observations that are allowed but must stay visible to later stages
    /// (e.g. `executable:scripts/run.sh`, `symlink-materialized:refs/a.md`).
    pub flags: Vec<String>,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum Outcome {
    Staged {
        digest: String,
        subpath: String,
        files: usize,
        flags: Vec<String>,
    },
    Rejected {
        subpath: String,
        reasons: Vec<String>,
    },
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Collected files: (relative path, absolute path of the bytes to store, executable).
type Collected = Vec<(String, PathBuf, bool)>;

/// Walk state: the skill root, the tree symlinks may resolve into, limits and accumulators.
struct Walk<'a> {
    root: &'a Path,
    boundary: &'a Path,
    lim: &'a Limits,
    out: Collected,
    flags: Vec<String>,
    reasons: Vec<String>,
}

fn walk(w: &mut Walk, dir: &Path, depth: usize) {
    if depth > 16 {
        w.reasons
            .push(format!("nesting too deep at {}", dir.display()));
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else {
        w.reasons
            .push(format!("unreadable directory {}", dir.display()));
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let Some(rel) = p
            .strip_prefix(w.root)
            .ok()
            .and_then(|r| r.to_str())
            .map(str::to_string)
        else {
            w.reasons
                .push(format!("non-UTF-8 path under {}", dir.display()));
            continue;
        };
        let Ok(meta) = fs::symlink_metadata(&p) else {
            w.reasons.push(format!("unreadable: {rel}"));
            continue;
        };
        let ft = meta.file_type();
        if ft.is_dir() {
            if !SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref()) {
                walk(w, &p, depth + 1);
            }
            continue;
        }
        let target = if ft.is_symlink() {
            match fs::canonicalize(&p) {
                Ok(t) if t.starts_with(w.root) && t.is_file() => {
                    w.flags.push(format!("symlink-materialized:{rel}"));
                    t
                }
                // shared files elsewhere in the same source tree (e.g. ../resources/contract.md)
                Ok(t) if t.starts_with(w.boundary) && t.is_file() => {
                    w.flags.push(format!("symlink-shared-materialized:{rel}"));
                    t
                }
                Ok(t) => {
                    w.reasons.push(format!(
                        "symlink escapes the source tree or is not a file: {rel} -> {}",
                        t.display()
                    ));
                    continue;
                }
                Err(_) => {
                    w.reasons.push(format!("dangling symlink: {rel}"));
                    continue;
                }
            }
        } else if ft.is_file() {
            p.clone()
        } else {
            w.reasons
                .push(format!("special file (device/FIFO/socket): {rel}"));
            continue;
        };
        let ext = Path::new(&rel)
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ARCHIVE_EXT.contains(&ext.as_str()) {
            w.reasons.push(format!("archive not allowed: {rel}"));
            continue;
        }
        let size = fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
        if size > w.lim.max_file_bytes {
            w.reasons
                .push(format!("file too large ({size} bytes): {rel}"));
            continue;
        }
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(&target)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        };
        #[cfg(not(unix))]
        let executable = false;
        if executable {
            w.flags.push(format!("executable:{rel}"));
        }
        w.out.push((rel, target, executable));
    }
}

/// Inspect without copying. Symlinks may point anywhere inside `boundary` (the fetched tree
/// or local root); their target bytes are stored. Ok = manifest; Err = every rejection reason.
pub fn inspect(
    skill_dir: &Path,
    boundary: &Path,
    lim: &Limits,
) -> Result<(Manifest, Collected), Vec<String>> {
    let root =
        fs::canonicalize(skill_dir).map_err(|e| vec![format!("{}: {e}", skill_dir.display())])?;
    let boundary =
        fs::canonicalize(boundary).map_err(|e| vec![format!("{}: {e}", boundary.display())])?;
    if !root.starts_with(&boundary) {
        return Err(vec![format!(
            "skill dir {} is outside the source tree",
            root.display()
        )]);
    }
    let mut w = Walk {
        root: &root,
        boundary: &boundary,
        lim,
        out: Vec::new(),
        flags: Vec::new(),
        reasons: Vec::new(),
    };
    walk(&mut w, &root, 0);
    let Walk {
        out: files,
        flags,
        mut reasons,
        ..
    } = w;
    if !files.iter().any(|(rel, _, _)| rel == "SKILL.md") {
        reasons.push("missing SKILL.md at the skill root".into());
    }
    if files.len() > lim.max_files {
        reasons.push(format!("too many files ({})", files.len()));
    }
    let mut entries = Vec::new();
    let mut total = 0u64;
    for (rel, abs, executable) in &files {
        let bytes = fs::read(abs).map_err(|e| vec![format!("{rel}: {e}")])?;
        total += bytes.len() as u64;
        entries.push(FileEntry {
            path: rel.clone(),
            sha256: hex(&Sha256::digest(&bytes)),
            size: bytes.len() as u64,
            executable: *executable,
        });
    }
    if total > lim.max_total_bytes {
        reasons.push(format!("skill too large ({total} bytes)"));
    }
    if !reasons.is_empty() {
        return Err(reasons);
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let canon = serde_json::to_string(&entries).unwrap();
    Ok((
        Manifest {
            digest: hex(&Sha256::digest(canon.as_bytes())),
            files: entries,
            flags,
        },
        files,
    ))
}

#[cfg(unix)]
fn make_read_only(p: &Path, dir: bool) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(
        p,
        fs::Permissions::from_mode(if dir { 0o555 } else { 0o444 }),
    );
}
#[cfg(not(unix))]
fn make_read_only(_: &Path, _: bool) {}

/// Copy an inspected skill into `store/objects/<digest>` (atomic rename) and record its
/// manifest and source lock. Re-staging identical content is a no-op for the object.
pub fn stage(
    skill_dir: &Path,
    boundary: &Path,
    subpath: &str,
    store: &Path,
    lim: &Limits,
    lock: &SourceLock,
) -> Result<Outcome, String> {
    let (manifest, files) = match inspect(skill_dir, boundary, lim) {
        Ok(v) => v,
        Err(reasons) => {
            let line = serde_json::json!({"origin": lock.origin, "commit": lock.commit, "subpath": subpath, "reasons": reasons});
            fs::create_dir_all(store).map_err(|e| e.to_string())?;
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(store.join("rejected.jsonl"))
                .map_err(|e| e.to_string())?;
            writeln!(f, "{line}").map_err(|e| e.to_string())?;
            return Ok(Outcome::Rejected {
                subpath: subpath.into(),
                reasons,
            });
        }
    };
    let objects = store.join("objects");
    let obj = objects.join(&manifest.digest);
    if !obj.exists() {
        let tmp = store
            .join("tmp")
            .join(format!("{}.{}", manifest.digest, std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        for (rel, abs, _) in &files {
            let dst = tmp.join(rel);
            fs::create_dir_all(dst.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::copy(abs, &dst).map_err(|e| format!("{rel}: {e}"))?;
            make_read_only(&dst, false);
        }
        fs::create_dir_all(&objects).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &obj).map_err(|e| format!("publish {}: {e}", obj.display()))?;
        make_read_only(&obj, true);
    }
    let write = |dir: &str, name: String, value: String| -> Result<(), String> {
        let d = store.join(dir);
        fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        fs::write(d.join(name), value + "\n").map_err(|e| e.to_string())
    };
    write(
        "manifests",
        format!("{}.json", manifest.digest),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )?;
    let lock_json = serde_json::to_string_pretty(lock).unwrap();
    let lock_id = hex(&Sha256::digest(
        format!("{}\0{:?}\0{}", lock.origin, lock.commit, subpath).as_bytes(),
    ));
    write(
        &format!("locks/{}", manifest.digest),
        format!("{}.json", &lock_id[..16]),
        lock_json,
    )?;
    Ok(Outcome::Staged {
        digest: manifest.digest,
        subpath: subpath.into(),
        files: files.len(),
        flags: manifest.flags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::LicenseEvidence;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fn unlock(p: &Path) {
                    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
                    if let Ok(rd) = fs::read_dir(p) {
                        for e in rd.flatten() {
                            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                                unlock(&e.path());
                            }
                        }
                    }
                }
                unlock(&self.0);
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn tmp(name: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("dao-q-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn skill(root: &Path) {
        fs::create_dir_all(root.join("references")).unwrap();
        fs::write(
            root.join("SKILL.md"),
            "---\nname: a\ndescription: does a\n---\n# a\n",
        )
        .unwrap();
        fs::write(root.join("references/r.md"), "ref").unwrap();
    }
    fn lock() -> SourceLock {
        SourceLock {
            kind: "local".into(),
            origin: "/x".into(),
            requested_rev: None,
            commit: None,
            subpath: "a".into(),
            fetched_at_unix: 0,
            license: LicenseEvidence {
                file: None,
                sha256: None,
                spdx_guess: None,
                declared: None,
                status: "unknown".into(),
            },
        }
    }

    #[test]
    fn clean_skill_is_staged_read_only_and_idempotent() {
        let t = tmp("clean");
        skill(&t.0.join("a"));
        let store = t.0.join("store");
        let first = stage(
            &t.0.join("a"),
            &t.0,
            "a",
            &store,
            &Limits::default(),
            &lock(),
        )
        .unwrap();
        let second = stage(
            &t.0.join("a"),
            &t.0,
            "a",
            &store,
            &Limits::default(),
            &lock(),
        )
        .unwrap();
        assert_eq!(first, second);
        let Outcome::Staged { digest, files, .. } = first else {
            panic!("{first:?}")
        };
        assert_eq!(files, 2);
        let obj = store.join("objects").join(&digest);
        assert_eq!(
            fs::read_to_string(obj.join("references/r.md")).unwrap(),
            "ref"
        );
        assert!(
            fs::write(obj.join("SKILL.md"), "tamper").is_err(),
            "object must be read-only"
        );
        assert!(store
            .join("manifests")
            .join(format!("{digest}.json"))
            .is_file());
        // one byte changes the digest
        fs::write(t.0.join("a/references/r.md"), "ref!").unwrap();
        let Outcome::Staged { digest: d2, .. } = stage(
            &t.0.join("a"),
            &t.0,
            "a",
            &store,
            &Limits::default(),
            &lock(),
        )
        .unwrap() else {
            panic!()
        };
        assert_ne!(digest, d2);
    }

    #[cfg(unix)]
    #[test]
    fn hostile_trees_are_rejected_with_reasons() {
        let t = tmp("hostile");
        let s = t.0.join("s");
        skill(&s);
        std::os::unix::fs::symlink("/etc/passwd", s.join("escape.md")).unwrap();
        std::os::unix::fs::symlink("missing.md", s.join("dangling.md")).unwrap();
        std::os::unix::fs::symlink("references/r.md", s.join("inside.md")).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(s.join("pipe"))
            .status()
            .unwrap()
            .success());
        fs::write(s.join("payload.zip"), "PK").unwrap();
        fs::write(s.join("big.bin"), vec![0u8; 2_000]).unwrap();
        let lim = Limits {
            max_file_bytes: 1_000,
            ..Limits::default()
        };
        let reasons = inspect(&s, &s, &lim).unwrap_err();
        let has = |k: &str| reasons.iter().any(|r| r.contains(k));
        assert!(
            has("escapes")
                && has("dangling")
                && has("special file")
                && has("archive")
                && has("too large"),
            "{reasons:?}"
        );
        assert!(
            !has("inside.md"),
            "an internal symlink is allowed: {reasons:?}"
        );
        let store = t.0.join("store");
        assert!(matches!(
            stage(&s, &s, "s", &store, &lim, &lock()).unwrap(),
            Outcome::Rejected { .. }
        ));
        assert!(fs::read_to_string(store.join("rejected.jsonl"))
            .unwrap()
            .contains("escapes"));
        assert!(!store.join("objects").exists());
    }

    #[cfg(unix)]
    #[test]
    fn shared_files_inside_the_source_tree_are_materialized() {
        let t = tmp("shared");
        fs::create_dir_all(t.0.join("resources")).unwrap();
        fs::write(t.0.join("resources/contract.md"), "shared").unwrap();
        let s = t.0.join("skills/op");
        skill(&s);
        std::os::unix::fs::symlink(
            "../../../resources/contract.md",
            s.join("references/contract.md"),
        )
        .unwrap();
        // inside the tree: allowed and flagged
        let (m, _) = inspect(&s, &t.0, &Limits::default()).unwrap();
        assert!(m
            .flags
            .contains(&"symlink-shared-materialized:references/contract.md".to_string()));
        // with the skill dir itself as the boundary the same link escapes
        assert!(
            inspect(&s, &s, &Limits::default()).unwrap_err()[0].contains("escapes the source tree")
        );
    }

    #[test]
    fn missing_skill_md_and_limits() {
        let t = tmp("nomd");
        fs::write(t.0.join("README.md"), "x").unwrap();
        assert!(inspect(&t.0, &t.0, &Limits::default())
            .unwrap_err()
            .iter()
            .any(|r| r.contains("missing SKILL.md")));
        let u = tmp("many");
        skill(&u.0);
        for i in 0..5 {
            fs::write(u.0.join(format!("f{i}.md")), "x").unwrap();
        }
        let lim = Limits {
            max_files: 3,
            ..Limits::default()
        };
        assert!(inspect(&u.0, &u.0, &lim)
            .unwrap_err()
            .iter()
            .any(|r| r.contains("too many files")));
    }

    #[cfg(unix)]
    #[test]
    fn executable_and_internal_symlink_are_flagged() {
        use std::os::unix::fs::PermissionsExt;
        let t = tmp("flags");
        skill(&t.0);
        fs::create_dir_all(t.0.join("scripts")).unwrap();
        fs::write(t.0.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(
            t.0.join("scripts/run.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("references/r.md", t.0.join("alias.md")).unwrap();
        let (m, _) = inspect(&t.0, &t.0, &Limits::default()).unwrap();
        assert!(m.flags.contains(&"executable:scripts/run.sh".to_string()));
        assert!(m
            .flags
            .contains(&"symlink-materialized:alias.md".to_string()));
        assert!(m.files.iter().any(|f| f.path == "alias.md" && f.size == 3));
    }
}
