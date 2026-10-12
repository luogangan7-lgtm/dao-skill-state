//! dao — the DAO skill supply chain CLI.
//!
//! Role: entry point; routes subcommands to modules. Stage 1 (this crate's first card):
//! `dao intake` = source pinning + quarantine into a content-addressed store.
//! Depends on: source, quarantine. Called by: users, CI, later DAO stages (scan, normalize).
//! Invariants: exit 0 when the command ran (rejected skills are a normal outcome, listed in
//! the JSON summary and `rejected.jsonl`); exit 2 on usage or environment errors.

mod quarantine;
mod source;

use quarantine::{stage, Limits, Outcome};
use source::{detect_license, fetch_git, read_vetted, SourceLock};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  dao intake --store DIR (--git HTTPS_URL|ABS_PATH --rev REV | --local DIR) [--path SUBPATH] [--all]\n\
         --all stages every directory under SUBPATH that contains a SKILL.md (not descending into skills).\n\
         exit: 0 ran (see JSON summary; rejections are listed, not errors), 2 usage/environment error"
    );
    ExitCode::from(2)
}

/// Directories under `base` that contain SKILL.md; a skill's own subdirectories are not searched.
fn find_skills(base: &Path, skip: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if base == skip {
        return; // the store itself may live inside a --local tree
    }
    if base.join("SKILL.md").is_file() {
        out.push(base.to_path_buf());
        return;
    }
    if depth == 0 {
        return;
    }
    let Ok(rd) = fs::read_dir(base) else { return };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| !matches!(e.file_name().to_str(), Some(".git" | "node_modules")))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    for d in dirs {
        find_skills(&d, skip, depth - 1, out);
    }
}

fn declared_license(skill_dir: &Path, tree: &Path) -> Option<String> {
    let bytes = read_vetted(&skill_dir.join("SKILL.md"), tree, 64 * 1024)?;
    let text = String::from_utf8_lossy(&bytes);
    let fm = text
        .trim_start_matches('\u{feff}')
        .strip_prefix("---")?
        .split("\n---")
        .next()?
        .to_string();
    fm.lines()
        .find_map(|l| l.strip_prefix("license:"))
        .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|v| !v.is_empty())
}

fn intake(args: &[String]) -> Result<ExitCode, String> {
    let (mut store, mut git, mut rev, mut local, mut path, mut all) =
        (None, None, None, None, String::new(), false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--store" => store = Some(PathBuf::from(val()?)),
            "--git" => git = Some(val()?),
            "--rev" => rev = Some(val()?),
            "--local" => local = Some(PathBuf::from(val()?)),
            "--path" => path = val()?,
            "--all" => all = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let store = store.ok_or("intake needs --store")?;
    if path.split('/').any(|c| c == "..") || Path::new(&path).is_absolute() {
        return Err("--path must be relative to the source root and must not contain '..'".into());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    fs::create_dir_all(&store).map_err(|e| format!("{}: {e}", store.display()))?;
    let store = fs::canonicalize(&store).map_err(|e| e.to_string())?;
    let fetch_dir = store
        .join("tmp")
        .join(format!("fetch-{}", std::process::id()));
    let (kind, origin, tree, commit) = match (git, local) {
        (Some(g), None) => {
            let rev = rev.clone().ok_or("--git needs --rev")?;
            let _ = fs::remove_dir_all(&fetch_dir);
            let commit = fetch_git(&g, &rev, &fetch_dir).inspect_err(|_| {
                let _ = fs::remove_dir_all(&fetch_dir);
            })?;
            ("git", g, fetch_dir.clone(), Some(commit))
        }
        (None, Some(l)) => {
            let abs = fs::canonicalize(&l).map_err(|e| format!("{}: {e}", l.display()))?;
            ("local", abs.to_string_lossy().into_owned(), abs, None)
        }
        _ => return Err("give exactly one of --git or --local".into()),
    };
    let result = stage_all(&tree, &path, all, &store, kind, &origin, &rev, &commit, now);
    if kind == "git" {
        let _ = fs::remove_dir_all(&fetch_dir);
    }
    let outcomes = result?;
    let staged = outcomes
        .iter()
        .filter(|o| matches!(o, Outcome::Staged { .. }))
        .count();
    let summary = serde_json::json!({
        "origin": origin, "commit": commit, "path": path,
        "found": outcomes.len(), "staged": staged, "rejected": outcomes.len() - staged,
        "outcomes": outcomes,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    Ok(ExitCode::SUCCESS)
}

#[allow(clippy::too_many_arguments)]
fn stage_all(
    tree: &Path,
    path: &str,
    all: bool,
    store: &Path,
    kind: &str,
    origin: &str,
    rev: &Option<String>,
    commit: &Option<String>,
    now: u64,
) -> Result<Vec<Outcome>, String> {
    let base = tree.join(path);
    let mut skills = Vec::new();
    if all {
        find_skills(&base, store, 8, &mut skills);
    } else {
        skills.push(base.clone());
    }
    let lim = Limits::default();
    let mut outcomes = Vec::new();
    for dir in &skills {
        let sub = dir
            .strip_prefix(tree)
            .unwrap_or(dir)
            .to_string_lossy()
            .into_owned();
        let lock = SourceLock {
            kind: kind.into(),
            origin: origin.into(),
            requested_rev: rev.clone(),
            commit: commit.clone(),
            subpath: sub.clone(),
            fetched_at_unix: now,
            license: detect_license(tree, dir, declared_license(dir, tree)),
        };
        outcomes.push(stage(dir, tree, &sub, store, &lim, &lock)?);
    }
    Ok(outcomes)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("intake") => intake(&args[1..]),
        _ => return usage(),
    };
    result.unwrap_or_else(|e| {
        eprintln!("error: {e}");
        ExitCode::from(2)
    })
}
