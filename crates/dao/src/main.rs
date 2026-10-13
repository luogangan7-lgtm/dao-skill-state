//! dao — the DAO skill supply chain CLI.
//!
//! Role: entry point; routes subcommands to modules. Stage 1 (this crate's first card):
//! `dao intake` = source pinning + quarantine into a content-addressed store.
//! Depends on: source, quarantine. Called by: users, CI, later DAO stages (scan, normalize).
//! Invariants: exit 0 when the command ran (rejected skills are a normal outcome, listed in
//! the JSON summary and `rejected.jsonl`); exit 2 on usage or environment errors.

mod check;
mod eval;
mod normalize;
mod quarantine;
mod relations;
mod scan;
mod source;
mod upstream;

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
  dao scan --store DIR (DIGEST... | --all)\n\
         writes scans/<digest>.json (BLOCK | UNRESOLVED | REVIEW_REQUIRED | CLEAN) and prints a summary.\n\
  dao normalize --store DIR (DIGEST...|--all)   writes cards/<digest>.json (verbatim routing card)\n\
  dao relations --store DIR   reads cards/, writes relations.json (duplicates, translations, drift, conflicts, families)\n\
  dao upstream check --store DIR [--apply] [--allow-local DIR]...   sources moved since intake; --apply stages them as new candidates\n\
  dao eval gen --store DIR   writes evals/set.json (routing cases copied from cards + relations)\n\
  dao eval run --store DIR --registry REG --searcher SKILL_HUB_BIN [--top-k 3 --floor 6 --rel 0.5]\n\
  dao check [SKILL_DIR]   (Rust port of check_skill.py: same JSON and exit codes 0/1/2)\n\
         exit: 0 ran (see JSON summary; rejections are listed, not errors), 2 usage/environment error"
    );
    ExitCode::from(2)
}

/// Directories under `base` that contain SKILL.md; a skill's own subdirectories are not searched.
/// `too_deep` collects directories left unsearched at the depth limit, so truncation is visible.
fn find_skills(base: &Path, depth: usize, out: &mut Vec<PathBuf>, too_deep: &mut Vec<PathBuf>) {
    if base.join("SKILL.md").is_file() {
        out.push(base.to_path_buf());
        return;
    }
    if depth == 0 {
        too_deep.push(base.to_path_buf());
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
        find_skills(&d, depth - 1, out, too_deep);
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
    if local.is_some() && rev.is_some() {
        return Err("--rev applies only to --git".into());
    }
    if let Some(g) = &git {
        source::validate_origin(g)?; // before touching the filesystem
    }
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
            if store.starts_with(&abs) {
                return Err("--store must not be inside the --local source tree".into());
            }
            ("local", abs.to_string_lossy().into_owned(), abs, None)
        }
        _ => return Err("give exactly one of --git or --local".into()),
    };
    let result = stage_all(&tree, &path, all, &store, kind, &origin, &rev, &commit, now);
    if kind == "git" {
        let _ = fs::remove_dir_all(&fetch_dir);
    }
    let (outcomes, too_deep) = result?;
    let staged = outcomes
        .iter()
        .filter(|o| matches!(o, Outcome::Staged { .. }))
        .count();
    let summary = serde_json::json!({
        "origin": origin, "commit": commit, "path": path,
        "found": outcomes.len(), "staged": staged, "rejected": outcomes.len() - staged,
        "outcomes": outcomes,
        "not_searched_depth_limit": too_deep,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    Ok(ExitCode::SUCCESS)
}

/// `dao upstream check --store DIR [--apply]`: report moved sources; with --apply stage each
/// changed path again as a new candidate (git: the new commit; local: the current tree).
fn upstream_cmd(args: &[String]) -> Result<ExitCode, String> {
    let (mut store, mut apply, mut allow) = (None, false, Vec::new());
    let mut it = args.iter();
    if it.next().map(String::as_str) != Some("check") {
        return Err(
            "usage: dao upstream check --store DIR [--apply] [--allow-local DIR]...".into(),
        );
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--store" => store = Some(PathBuf::from(it.next().ok_or("--store needs a value")?)),
            "--apply" => apply = true,
            "--allow-local" => {
                let d = it.next().ok_or("--allow-local needs a directory")?;
                allow.push(fs::canonicalize(d).map_err(|e| format!("{d}: {e}"))?);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let store = fs::canonicalize(store.ok_or("--store is required")?).map_err(|e| e.to_string())?;
    if allow.contains(&store) {
        return Err("--allow-local must not be the store itself".into());
    }
    let (items, mut reports) = upstream::tracked(&store)?;
    reports.extend(upstream::check(&items, &allow));
    let mut staged = Vec::new();
    let mut apply_failed = 0usize;
    if apply {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // one fetch per (origin, upstream commit); every changed path under it is staged
        let mut groups: std::collections::BTreeMap<
            (String, String, Option<String>, String),
            Vec<String>,
        > = Default::default();
        for r in reports.iter().filter(|r| r.status == "changed") {
            let key = (
                r.kind.clone(),
                r.origin.clone(),
                r.requested_rev.clone(),
                r.upstream.clone().unwrap_or_default(),
            );
            groups.entry(key).or_default().push(r.subpath.clone());
        }
        for ((kind, origin, rev, upstream_commit), paths) in groups {
            let fetch_dir = store
                .join("tmp")
                .join(format!("upstream-{}", std::process::id()));
            // a local path is re-checked against --allow-local now and read only through the
            // canonical path it resolves to (a symlink retargeted since the check is not followed)
            let src = if Path::new(&origin).is_absolute() {
                upstream::local_allowed(&origin, &allow)
                    .unwrap_or_else(|| Err(format!("{origin}: local origin no longer allowed")))
            } else {
                Ok(PathBuf::from(&origin))
            };
            let fetched = src.and_then(|src| {
                if kind == "git" {
                    let _ = fs::remove_dir_all(&fetch_dir);
                    fetch_git(&src.to_string_lossy(), &upstream_commit, &fetch_dir)
                        .map(|c| (fetch_dir.clone(), Some(c)))
                } else if store.starts_with(&src) {
                    Err("--store must not be inside a local source tree".to_string())
                } else {
                    Ok((src, None))
                }
            });
            match fetched {
                // one unreachable source is that source's error, not the run's
                Err(e) => {
                    apply_failed += paths.len();
                    staged.push(serde_json::json!({"outcome": "error", "origin": origin, "paths": paths, "error": e}));
                }
                Ok((tree, commit)) => {
                    for p in paths {
                        match stage_all(
                            &tree, &p, false, &store, &kind, &origin, &rev, &commit, now,
                        ) {
                            Ok((outcomes, _)) => {
                                for o in outcomes {
                                    apply_failed +=
                                        usize::from(matches!(o, Outcome::Rejected { .. }));
                                    staged.push(serde_json::to_value(o).unwrap());
                                }
                            }
                            Err(e) => {
                                apply_failed += 1;
                                staged.push(serde_json::json!({"outcome": "error", "subpath": p, "error": e}));
                            }
                        }
                    }
                }
            }
            if kind == "git" {
                let _ = fs::remove_dir_all(&fetch_dir);
            }
        }
    }
    let count = |s: &str| reports.iter().filter(|r| r.status == s).count();
    let errors = count("error");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "tracked": items.len(), "unchanged": count("unchanged"), "changed": count("changed"),
            "pinned": count("pinned"), "skipped": count("skipped"), "errors": errors,
            "apply_failed": apply_failed, "reports": reports, "staged": staged,
        }))
        .unwrap()
    );
    Ok(if errors == 0 && apply_failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
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
) -> Result<(Vec<Outcome>, Vec<String>), String> {
    let base = tree.join(path);
    let (mut skills, mut too_deep) = (Vec::new(), Vec::new());
    if all {
        find_skills(&base, 8, &mut skills, &mut too_deep);
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
    let too_deep = too_deep
        .iter()
        .map(|d| {
            d.strip_prefix(tree)
                .unwrap_or(d)
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    Ok((outcomes, too_deep))
}

fn scan_cmd(args: &[String]) -> Result<ExitCode, String> {
    let (mut store, mut digests, mut all) = (None, Vec::new(), false);
    let (mut skipped, mut errors) = (Vec::<String>::new(), Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--store" => store = Some(PathBuf::from(it.next().ok_or("--store needs a value")?)),
            "--all" => all = true,
            d if !d.starts_with('-') => digests.push(d.to_string()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let store = store.ok_or("scan needs --store")?;
    if all {
        let rd = fs::read_dir(store.join("objects")).map_err(|e| e.to_string())?;
        // only content digests; anything else in objects/ is reported, not scanned
        for e in rd.filter_map(Result::ok) {
            match e.file_name().into_string() {
                Ok(n) if scan::is_digest(&n) => digests.push(n),
                Ok(n) => skipped.push(n),
                Err(n) => skipped.push(n.to_string_lossy().into_owned()),
            }
        }
        digests.sort();
    }
    if digests.is_empty() {
        return Err("give digests or --all".into());
    }
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut rows = Vec::new();
    for d in &digests {
        let r = match scan::scan(&store, d).and_then(|r| scan::write_receipt(&store, &r).map(|_| r))
        {
            Ok(r) => r,
            Err(e) => {
                errors.push(serde_json::json!({"digest": d, "error": e}));
                continue;
            }
        };
        let v = serde_json::to_value(r.verdict).unwrap();
        *counts
            .entry(v.as_str().unwrap_or("?").to_string())
            .or_default() += 1;
        rows.push(serde_json::json!({"digest": d, "verdict": v, "findings": r.findings.len()}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"scanned": rows.len(), "verdicts": counts, "objects": rows, "skipped": skipped, "errors": errors})
        )
        .unwrap()
    );
    // a digest that could not be scanned keeps no stale receipt and fails the run
    for e in &errors {
        if let Some(d) = e["digest"].as_str().filter(|d| scan::is_digest(d)) {
            let _ = fs::remove_file(store.join("scans").join(format!("{d}.json")));
        }
    }
    Ok(if errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// `--store DIR` plus either digests or `--all` (every content digest under objects/).
fn store_and_digests(args: &[String]) -> Result<(PathBuf, Vec<String>), String> {
    let (mut store, mut digests, mut all) = (None, Vec::new(), false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--store" => store = Some(PathBuf::from(it.next().ok_or("--store needs a value")?)),
            "--all" => all = true,
            d if !d.starts_with('-') => digests.push(d.to_string()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let store = store.ok_or("--store is required")?;
    if all {
        for e in fs::read_dir(store.join("objects"))
            .map_err(|e| e.to_string())?
            .flatten()
        {
            if let Ok(n) = e.file_name().into_string() {
                if scan::is_digest(&n) {
                    digests.push(n);
                }
            }
        }
        digests.sort();
    }
    Ok((store, digests))
}

fn normalize_cmd(args: &[String]) -> Result<ExitCode, String> {
    let (store, digests) = store_and_digests(args)?;
    if digests.is_empty() {
        return Err("normalize needs digests or --all (and --all found no objects)".into());
    }
    let mut status = std::collections::BTreeMap::<String, usize>::new();
    let mut errors = Vec::new();
    for d in &digests {
        match normalize::card(&store, d).and_then(|c| normalize::write_card(&store, &c).map(|_| c))
        {
            Ok(c) => *status.entry(c.status).or_default() += 1,
            Err(e) => {
                // never leave a card behind for an object that can no longer be read
                if scan::is_digest(d) {
                    let _ = fs::remove_file(store.join("cards").join(format!("{d}.json")));
                }
                errors.push(serde_json::json!({"digest": d, "error": e}));
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({"cards": digests.len() - errors.len(), "status": status, "errors": errors})).unwrap()
    );
    Ok(if errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// Cards of objects that still exist, whose digest matches their file name (sorted by digest),
/// plus the names of card files that were skipped.
fn load_cards(store: &Path) -> Result<(Vec<normalize::Card>, Vec<String>), String> {
    let mut cards: Vec<normalize::Card> = Vec::new();
    let mut skipped = Vec::new();
    for e in fs::read_dir(store.join("cards"))
        .map_err(|e| format!("run dao normalize first: {e}"))?
        .flatten()
    {
        let path = e.path();
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|_| path.extension().is_some_and(|x| x == "json"))
        else {
            continue;
        };
        // only cards of objects that still exist, whose digest matches their file name
        let card: Option<normalize::Card> = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        match card {
            Some(c)
                if scan::is_digest(stem)
                    && c.digest == stem
                    && store.join("objects").join(stem).is_dir() =>
            {
                cards.push(c)
            }
            _ => skipped.push(stem.to_string()),
        }
    }
    cards.sort_by(|a, b| a.digest.cmp(&b.digest));
    Ok((cards, skipped))
}

/// Write JSON atomically (sibling temp file + rename; the temp file never survives a failure).
fn write_json(path: &Path, v: &impl serde::Serialize) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let res =
        fs::write(&tmp, serde_json::to_vec_pretty(v).unwrap()).and_then(|_| fs::rename(&tmp, path));
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res.map_err(|e| format!("{}: {e}", path.display()))
}

fn eval_cmd(args: &[String]) -> Result<ExitCode, String> {
    let (sub, rest) = args.split_first().ok_or("eval needs gen or run")?;
    let allowed: &[&str] = match sub.as_str() {
        "gen" => &["--store"],
        "run" => &[
            "--store",
            "--registry",
            "--searcher",
            "--top-k",
            "--floor",
            "--rel",
        ],
        other => return Err(format!("unknown eval subcommand: {other}")),
    };
    let mut opt: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let key = allowed
            .iter()
            .find(|k| **k == a.as_str())
            .ok_or(format!("unknown argument for eval {sub}: {a}"))?;
        opt.insert(
            key,
            it.next().ok_or(format!("{key} needs a value"))?.clone(),
        );
    }
    let store = PathBuf::from(opt.get("--store").ok_or("--store is required")?);
    let dir = store.join("evals");
    // the set is a pure function of the store; regenerating it is the only trustworthy freeze
    let regenerate = || -> Result<eval::Set, String> {
        let (cards, _) = load_cards(&store)?;
        let rels: Vec<relations::Relation> = serde_json::from_slice(
            &fs::read(store.join("relations.json"))
                .map_err(|e| format!("run dao relations first: {e}"))?,
        )
        .map_err(|e| format!("relations.json: {e}"))?;
        let set = eval::generate(&cards, &rels);
        if set.subjects.is_empty() {
            return Err("no usable cards: nothing to evaluate".into());
        }
        Ok(set)
    };
    if sub == "gen" {
        let set = regenerate()?;
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        write_json(&dir.join("set.json"), &set)?;
        let mut kinds = std::collections::BTreeMap::<&str, usize>::new();
        for c in &set.cases {
            *kinds.entry(c.kind.as_str()).or_default() += 1;
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"set_sha256": set.set_sha256, "subjects": set.subjects.len(), "cases": kinds})).unwrap()
        );
        return Ok(ExitCode::SUCCESS);
    }
    let set: eval::Set = serde_json::from_slice(
        &fs::read(dir.join("set.json")).map_err(|e| format!("run dao eval gen first: {e}"))?,
    )
    .map_err(|e| format!("set.json: {e}"))?;
    if regenerate()? != set {
        return Err("evals/set.json is not the set this store generates (edited, or cards changed: rerun dao eval gen)".into());
    }
    let registry = PathBuf::from(opt.get("--registry").ok_or("--registry is required")?);
    let searcher = eval::resolve_bin(
        opt.get("--searcher")
            .ok_or("--searcher (skill-hub binary) is required")?,
    )?;
    let before = eval::input_hashes(&registry, &searcher)?;
    let reg: serde_json::Value = serde_json::from_slice(
        &fs::read(&registry).map_err(|e| format!("{}: {e}", registry.display()))?,
    )
    .map_err(|e| format!("{}: {e}", registry.display()))?;
    let ids: std::collections::HashSet<String> = reg["skills"]
        .as_array()
        .ok_or("registry has no skills array")?
        .iter()
        .filter_map(|s| s["id"].as_str().map(str::to_string))
        .collect();
    let num = |k: &str, d: f64| -> Result<f64, String> {
        let v = opt
            .get(k)
            .map_or(Ok(d), |v| v.parse::<f64>().map_err(|e| format!("{k}: {e}")))?;
        if v.is_finite() && v >= 0.0 {
            Ok(v)
        } else {
            Err(format!("{k} must be a finite number ≥ 0"))
        }
    };
    let top_k: usize = opt.get("--top-k").map_or(Ok(3), |v| {
        v.parse()
            .map_err(|_| "--top-k must be a positive integer".to_string())
    })?;
    if top_k == 0 {
        return Err("--top-k must be a positive integer".into());
    }
    let n_skills = ids.len();
    let (floor, rel) = (
        num("--floor", eval::hook_floor(n_skills))?,
        num("--rel", 0.5)?,
    );
    let search = eval::skill_hub(searcher.clone(), registry.clone(), top_k);
    let scored = eval::run(&set, &search, &ids, top_k, floor, rel)?;
    let r = eval::receipt(
        &set, &registry, &searcher, before, n_skills, top_k, floor, rel, scored,
    )?;
    write_json(&dir.join(r.file_name()), &r)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({"receipt": r.file_name(), "set_sha256": r.set_sha256, "registry_sha256": r.registry_sha256, "floor": r.floor, "kinds": r.kinds, "failures": r.failures.len()})).unwrap()
    );
    Ok(ExitCode::SUCCESS)
}

fn relations_cmd(args: &[String]) -> Result<ExitCode, String> {
    let (store, digests) = store_and_digests(args)?;
    if !digests.is_empty() || args.iter().any(|a| a == "--all") {
        return Err("relations takes only --store (it reads every card)".into());
    }
    let (cards, mut skipped) = load_cards(&store)?;
    let rels = relations::build(&cards);
    let mut kinds = std::collections::BTreeMap::<&str, usize>::new();
    for r in &rels {
        *kinds.entry(r.kind.as_str()).or_default() += 1;
    }
    write_json(&store.join("relations.json"), &rels)?;
    skipped.sort();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"cards": cards.len(), "relations": kinds, "skipped": skipped})
        )
        .unwrap()
    );
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let Ok(args) = std::env::args_os()
        .skip(1)
        .map(|a| a.into_string())
        .collect::<Result<Vec<String>, _>>()
    else {
        eprintln!("error: arguments must be UTF-8");
        return ExitCode::from(2);
    };
    let result = match args.first().map(String::as_str) {
        Some("intake") => intake(&args[1..]),
        Some("upstream") => upstream_cmd(&args[1..]),
        Some("scan") => scan_cmd(&args[1..]),
        Some("normalize") => normalize_cmd(&args[1..]),
        Some("relations") => relations_cmd(&args[1..]),
        Some("eval") => eval_cmd(&args[1..]),
        Some("check") if args.len() <= 2 => {
            let r = check::check_skill(args.get(1).map(String::as_str).unwrap_or("."));
            println!("{}", check::to_json(&r));
            return ExitCode::from(check::exit_code(&r));
        }
        _ => return usage(),
    };
    result.unwrap_or_else(|e| {
        eprintln!("error: {e}");
        ExitCode::from(2)
    })
}
