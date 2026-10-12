//! source — pin where a skill came from (repository, exact commit, subpath, license evidence).
//!
//! Depends on: the system `git` binary (fetch only; no hooks, no ext:: transports); sha2.
//! Called by: `main` (`dao intake`), quarantine (writes the lock next to the staged object).
//! Invariants:
//! - A fetched tree is identified by the commit git reports after checkout, never by the
//!   requested ref alone; a requested full SHA must equal the checked-out commit.
//! - Remote URLs are limited to https:// and local paths; `ext::`, `fd::` and leading `-` are
//!   rejected so a source string can never run a command.
//! - Git runs with hooks disabled and no init template, so nothing in the fetched repo or the
//!   user's git templates executes during intake.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct LicenseEvidence {
    /// License file found, relative to the repository root (or skill dir for local sources).
    pub file: Option<String>,
    pub sha256: Option<String>,
    /// Best-effort SPDX guess from the file text; never treated as legal advice.
    pub spdx_guess: Option<String>,
    /// `license:` from the skill's frontmatter, verbatim.
    pub declared: Option<String>,
    /// green (permissive, recognised) | restricted (copyleft / non-commercial) | unknown
    pub status: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SourceLock {
    /// `git` or `local`
    pub kind: String,
    pub origin: String,
    pub requested_rev: Option<String>,
    pub commit: Option<String>,
    /// Skill directory relative to the fetched tree root.
    pub subpath: String,
    pub fetched_at_unix: u64,
    pub license: LicenseEvidence,
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn validate_origin(origin: &str) -> Result<(), String> {
    let lower = origin.to_ascii_lowercase();
    if origin.starts_with('-')
        || lower.starts_with("ext::")
        || lower.starts_with("fd::")
        || lower.contains("::")
    {
        return Err(format!(
            "refusing source with a transport helper or option syntax: {origin}"
        ));
    }
    if let Some(rest) = lower.strip_prefix("https://") {
        if rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .contains('@')
        {
            return Err(
                "credentials in the URL would be written to locks; use a credential helper".into(),
            );
        }
        return Ok(());
    }
    if Path::new(origin).is_absolute() {
        return Ok(());
    }
    Err(format!(
        "source must be an https:// URL or an absolute local path: {origin}"
    ))
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "protocol.ext.allow=never",
        ])
        .args(args)
        .current_dir(dir)
        // never act on a repository named by the caller's environment (hooks, wrappers)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_NAMESPACE")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Fetch `rev` of `origin` into the empty directory `dest`; returns the checked-out commit.
pub fn fetch_git(origin: &str, rev: &str, dest: &Path) -> Result<String, String> {
    validate_origin(origin)?;
    if rev.starts_with('-') || rev.is_empty() {
        return Err(format!("invalid revision: {rev}"));
    }
    fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    git(dest, &["-c", "init.templateDir=", "init", "-q"])?;
    git(dest, &["fetch", "-q", "--depth", "1", "--", origin, rev])?;
    git(dest, &["checkout", "-q", "--detach", "FETCH_HEAD"])?;
    let commit = git(dest, &["rev-parse", "HEAD"])?;
    if rev.len() == 40
        && rev.chars().all(|c| c.is_ascii_hexdigit())
        && !commit.eq_ignore_ascii_case(rev)
    {
        return Err(format!("requested commit {rev} but checked out {commit}"));
    }
    Ok(commit)
}

/// Read a small metadata file (license, frontmatter) before quarantine has vetted the tree:
/// regular file only (a symlink must resolve to a regular file inside `boundary` and outside
/// `.git`), at most `cap` bytes. Anything else reads as absent.
pub fn read_vetted(path: &Path, boundary: &Path, cap: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let boundary = fs::canonicalize(boundary).ok()?;
    let real = fs::canonicalize(path).ok()?;
    if !real.starts_with(&boundary) || real.starts_with(boundary.join(".git")) {
        return None;
    }
    if !fs::symlink_metadata(&real).ok()?.file_type().is_file() {
        return None;
    }
    let mut buf = Vec::new();
    fs::File::open(&real)
        .ok()?
        .take(cap)
        .read_to_end(&mut buf)
        .ok()?;
    Some(buf)
}

const LICENSE_FILES: [&str; 6] = [
    "LICENSE",
    "LICENSE.md",
    "LICENSE.txt",
    "LICENCE",
    "COPYING",
    "LICENSE-MIT",
];

fn guess_spdx(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    let rules: [(&[&str], &str); 9] = [
        (&["gnu affero general public license"], "AGPL-3.0"),
        (&["gnu lesser general public license"], "LGPL"),
        (&["gnu general public license"], "GPL"),
        (&["noncommercial"], "CC-BY-NC"),
        (&["apache license", "version 2.0"], "Apache-2.0"),
        (&["mit license"], "MIT"),
        (&["permission is hereby granted, free of charge"], "MIT"),
        (&["isc license"], "ISC"),
        (
            &["redistribution and use in source and binary forms"],
            "BSD",
        ),
    ];
    rules
        .iter()
        .find(|(needles, _)| needles.iter().all(|n| t.contains(n)))
        .map(|(_, id)| *id)
}

fn status_for(spdx: Option<&str>) -> &'static str {
    match spdx {
        Some("MIT" | "Apache-2.0" | "ISC" | "BSD") => "green",
        Some(_) => "restricted",
        None => "unknown",
    }
}

/// Look for a license file in the skill directory first, then up to the tree root.
pub fn detect_license(
    tree_root: &Path,
    skill_dir: &Path,
    declared: Option<String>,
) -> LicenseEvidence {
    let mut dir = Some(skill_dir.to_path_buf());
    while let Some(d) = dir {
        for name in LICENSE_FILES {
            let p = d.join(name);
            if let Some(bytes) = read_vetted(&p, tree_root, 256 * 1024) {
                let spdx = guess_spdx(&String::from_utf8_lossy(&bytes));
                return LicenseEvidence {
                    file: p
                        .strip_prefix(tree_root)
                        .ok()
                        .map(|r| r.to_string_lossy().into_owned()),
                    sha256: Some(hex(&Sha256::digest(&bytes))),
                    spdx_guess: spdx.map(str::to_string),
                    declared,
                    status: status_for(spdx).into(),
                };
            }
        }
        if d == tree_root {
            break;
        }
        dir = d
            .parent()
            .map(PathBuf::from)
            .filter(|p| p.starts_with(tree_root));
    }
    LicenseEvidence {
        file: None,
        sha256: None,
        spdx_guess: None,
        declared,
        status: "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dao-source-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn origins_that_could_run_commands_are_refused() {
        for bad in [
            "ext::sh -c touch% /tmp/pwned",
            "fd::17",
            "-uploadpack=x",
            "git@github.com:a/b.git",
            "relative/path",
            "http://x",
        ] {
            assert!(validate_origin(bad).is_err(), "{bad}");
        }
        assert!(validate_origin("https://github.com/a/b.git").is_ok());
        assert!(validate_origin("/abs/repo").is_ok());
        assert!(validate_origin("https://user:tok@github.com/a/b").is_err());
        assert!(validate_origin("https://github.com/a/b@v1").is_ok());
    }

    #[test]
    fn fetch_pins_the_commit() {
        let src = tmp("repo");
        let run = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&src)
                .status()
                .unwrap()
                .success())
        };
        run(&["init", "-q"]);
        fs::write(src.join("SKILL.md"), "---\nname: a\ndescription: b\n---\n").unwrap();
        run(&["-c", "user.email=t@t", "-c", "user.name=t", "add", "."]);
        run(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "x",
        ]);
        let head = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&src)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let dest = tmp("dest");
        let commit = fetch_git(src.to_str().unwrap(), head.trim(), &dest.join("t")).unwrap();
        assert_eq!(commit, head.trim());
        assert!(dest.join("t/SKILL.md").is_file());
        assert!(fetch_git(src.to_str().unwrap(), "-x", &dest.join("u")).is_err());
        let _ = (fs::remove_dir_all(src), fs::remove_dir_all(dest));
    }

    #[test]
    fn license_is_found_nearest_first_and_classified() {
        let root = tmp("lic");
        fs::create_dir_all(root.join("skills/a")).unwrap();
        fs::write(
            root.join("LICENSE"),
            "MIT License\nPermission is hereby granted, free of charge",
        )
        .unwrap();
        let l = detect_license(&root, &root.join("skills/a"), Some("MIT".into()));
        assert_eq!(
            (
                l.file.as_deref(),
                l.spdx_guess.as_deref(),
                l.status.as_str()
            ),
            (Some("LICENSE"), Some("MIT"), "green")
        );
        fs::write(
            root.join("skills/a/LICENSE.txt"),
            "GNU GENERAL PUBLIC LICENSE Version 3",
        )
        .unwrap();
        let l = detect_license(&root, &root.join("skills/a"), None);
        assert_eq!(
            (l.file.as_deref(), l.status.as_str()),
            (Some("skills/a/LICENSE.txt"), "restricted")
        );
        // a license symlink out of the tree, or to a FIFO/device, reads as absent
        let out = tmp("outside");
        fs::write(out.join("secret"), "MIT License").unwrap();
        fs::remove_file(root.join("skills/a/LICENSE.txt")).unwrap();
        fs::remove_file(root.join("LICENSE")).unwrap();
        std::os::unix::fs::symlink(out.join("secret"), root.join("LICENSE")).unwrap();
        assert_eq!(
            detect_license(&root, &root.join("skills/a"), None).file,
            None
        );
        fs::remove_file(root.join("LICENSE")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", root.join("LICENSE")).unwrap();
        assert_eq!(
            detect_license(&root, &root.join("skills/a"), None).file,
            None
        );
        let _ = fs::remove_dir_all(out);
        let none = tmp("nolic");
        assert_eq!(detect_license(&none, &none, None).status, "unknown");
        let _ = (fs::remove_dir_all(root), fs::remove_dir_all(none));
    }
}
