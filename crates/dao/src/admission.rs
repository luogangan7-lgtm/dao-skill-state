//! admission — decide whether a staged candidate may become champion, and move champion
//! pointers through a crash-safe journal.
//!
//! Depends on: quarantine (verify_object, Manifest), scan (Receipt, Verdict), normalize (Card),
//! eval (Set, Receipt).
//! Called by: `main` (`dao admit | promote | rollback | recover`); publish reads `champions/`.
//! Invariants:
//! - Admission is a pure decision over evidence already on disk; it writes one receipt
//!   (`admissions/<digest>.json`) and never changes a champion. Every gate it applies is named
//!   in the receipt with its evidence, admitted or not.
//! - Gates: the object verifies against its manifest; a scan receipt exists with verdict CLEAN,
//!   or REVIEW_REQUIRED plus a human approval file `approvals/<digest>.json` naming the digest;
//!   the card is `ok`; the eval set regenerates from the store and names this digest as the
//!   subject of its name; the newest `dao-eval-receipt/2` for that set scored every gated case
//!   expecting the name (an unscored case never passes), its registry held this object's
//!   SKILL.md under the name, and no gated case failed that expects the name or kept it
//!   (over-triggering on abstain / another skill's choose prompt counts against it).
//! - `promote` re-runs this decision from the evidence on disk; the admissions/ file is a
//!   record, never an input.
//! - A champion pointer (`champions/<name>.json`) only moves through the journal:
//!   prepare (journal entry, fsync) → pointer (tmp + fsync + rename + dir fsync) → commit
//!   (journal entry updated). `recover` finishes or aborts any prepared entry by looking at the
//!   pointer, so a crash at any point leaves either the old or the new champion, never a mix.
//!   Promoting the current champion again is a no-op; rollback restores the previous champion
//!   through the same journal. Moves and recovery hold `journal.lock` (O_EXCL), so concurrent
//!   promotions serialize; names that differ only in case are refused (one file on macOS).

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Gate {
    pub name: String,
    pub passed: bool,
    pub evidence: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Admission {
    pub schema: String,
    pub digest: String,
    pub name: Option<String>,
    pub admitted: bool,
    pub gates: Vec<Gate>,
}

/// Evidence gathered by the caller (main), so this module stays a pure decision.
pub struct Evidence {
    pub object_ok: Result<(), String>,
    pub scan: Option<crate::scan::Receipt>,
    pub approval: Option<serde_json::Value>,
    pub card: Option<crate::normalize::Card>,
    /// (set as regenerated from the store, whether the stored set equals it, receipt for it)
    pub eval: Option<(crate::eval::Set, bool, Option<crate::eval::Receipt>)>,
    /// sha256 of the object's SKILL.md per its manifest (compared with the evaluated registry).
    pub entry_sha256: Option<String>,
}

/// First 12 chars for display, without slicing inside a multibyte char.
fn short(s: &str) -> &str {
    s.char_indices().nth(12).map_or(s, |(i, _)| &s[..i])
}

/// The eval gate for `n` against one receipt. A case counts for `n` when it is gated and either
/// expects `n` or failed with `n` among its kept hits (over-triggering: abstain prompts `n`
/// took, choose prompts `n` stole). A case expecting `n` passes only if it was scored.
fn eval_gate(
    n: &str,
    set: &crate::eval::Set,
    r: &crate::eval::Receipt,
    entry: Option<&str>,
) -> (bool, String) {
    if r.schema != crate::eval::RECEIPT_SCHEMA {
        return (
            false,
            format!(
                "receipt schema {} is not {} (rerun dao eval run)",
                r.schema,
                crate::eval::RECEIPT_SCHEMA
            ),
        );
    }
    let gated = |c: &&crate::eval::Case| r.gated.contains(&c.kind);
    let mine: Vec<&str> = set
        .cases
        .iter()
        .filter(gated)
        .filter(|c| c.expect.iter().any(|e| e == n))
        .map(|c| c.id.as_str())
        .collect();
    if mine.is_empty() {
        return (false, format!("no gated eval case expects {n}"));
    }
    let unscored: Vec<&str> = mine
        .iter()
        .copied()
        .filter(|id| !r.scored.iter().any(|s| s == id))
        .collect();
    if !unscored.is_empty() {
        return (
            false,
            format!(
                "{} of {} cases were not scored ({n} is not an id in the evaluated registry?): {}",
                unscored.len(),
                mine.len(),
                unscored.join(", ")
            ),
        );
    }
    match (r.entries.get(n), entry) {
        (Some(h), Some(e)) if h == e => {}
        (h, e) => {
            return (
                false,
                format!(
                    "the evaluated registry's {n} SKILL.md is {}, this object's is {}",
                    h.map_or("unknown", |h| short(h)),
                    e.map_or("unknown", short)
                ),
            )
        }
    }
    let gated_ids: Vec<&str> = set
        .cases
        .iter()
        .filter(gated)
        .map(|c| c.id.as_str())
        .collect();
    let failed: Vec<&str> = r
        .failures
        .iter()
        .filter(|(id, kept)| {
            gated_ids.contains(&id.as_str())
                && (mine.contains(&id.as_str()) || kept.iter().any(|k| k == n))
        })
        .map(|(id, _)| id.as_str())
        .collect();
    if failed.is_empty() {
        (
            true,
            format!(
                "{} cases passed, no gated failure kept {n} (receipt registry {})",
                mine.len(),
                short(&r.registry_sha256)
            ),
        )
    } else {
        (
            false,
            format!(
                "{} gated cases failed with {n} expected or kept ({} expect it): {}",
                failed.len(),
                mine.len(),
                failed.join(", ")
            ),
        )
    }
}

pub fn decide(digest: &str, ev: &Evidence) -> Admission {
    let mut gates = Vec::new();
    let mut gate = |name: &str, passed: bool, evidence: String| {
        gates.push(Gate {
            name: name.into(),
            passed,
            evidence,
        });
    };
    gate(
        "object",
        ev.object_ok.is_ok(),
        ev.object_ok
            .clone()
            .err()
            .unwrap_or_else(|| "object matches its manifest".into()),
    );
    match &ev.scan {
        None => gate("scan", false, "no scan receipt (run dao scan)".into()),
        Some(r) if r.digest != digest => {
            gate("scan", false, "scan receipt is for another digest".into())
        }
        Some(r) => {
            use crate::scan::Verdict::*;
            let approved = ev.approval.as_ref().is_some_and(|a| {
                a["digest"].as_str() == Some(digest)
                    && a["approved_by"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty())
            });
            let (ok, why) = match r.verdict {
                Clean => (true, "CLEAN".to_string()),
                ReviewRequired if approved => (
                    true,
                    "REVIEW_REQUIRED, approved by a human (approvals/<digest>.json)".into(),
                ),
                ReviewRequired => (
                    false,
                    format!(
                        "REVIEW_REQUIRED with {} findings and no approval file",
                        r.findings.len()
                    ),
                ),
                Block => (false, "BLOCK".into()),
                Unresolved => (false, format!("UNRESOLVED: {}", r.unresolved.join("; "))),
            };
            gate(
                "scan",
                ok,
                format!("{why} (rules {})", short(&r.rules_sha256)),
            );
        }
    }
    let name = ev.card.as_ref().and_then(|c| c.name.clone());
    match &ev.card {
        None => gate("card", false, "no routing card (run dao normalize)".into()),
        Some(c) => gate("card", c.status == "ok", format!("status {}", c.status)),
    }
    match (&ev.eval, &name) {
        (None, _) => gate("eval", false, "no eval set (run dao eval gen / run)".into()),
        (_, None) => gate("eval", false, "no name to evaluate".into()),
        (Some((set, fresh, receipt)), Some(n)) => {
            let subject = set.subjects.get(n);
            let (ok, why) = if !fresh {
                (
                    false,
                    "evals/set.json is stale: rerun dao eval gen".to_string(),
                )
            } else if subject.map(String::as_str) != Some(digest) {
                (
                    false,
                    format!(
                        "the eval set's subject for {n} is {}, not this digest",
                        subject.map_or("none", |s| short(s))
                    ),
                )
            } else {
                match receipt {
                    None => (
                        false,
                        "no eval receipt for this set (run dao eval run)".into(),
                    ),
                    Some(r) => eval_gate(n, set, r, ev.entry_sha256.as_deref()),
                }
            };
            gate("eval", ok, why);
        }
    }
    Admission {
        schema: "dao-admission/1".into(),
        digest: digest.into(),
        name,
        admitted: gates.iter().all(|g| g.passed),
        gates,
    }
}

// ---------- champion pointers and the journal ----------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Pointer {
    pub name: String,
    pub digest: String,
    /// Earlier champions, newest first (rollback target is `history[0]`).
    pub history: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Entry {
    pub seq: u64,
    pub name: String,
    pub from: Option<String>,
    pub to: String,
    /// prepared | committed | aborted
    pub state: String,
}

/// Where a simulated crash happens (tests and `DAO_FAULT`): after the journal prepare, after
/// the pointer write, or never.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Fault {
    None,
    AfterPrepare,
    AfterPointer,
}

pub(crate) fn fsync_dir(dir: &Path) -> Result<(), String> {
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| format!("{}: {e}", dir.display()))
}

/// Durable atomic write: temp file in the same dir, fsync, rename, fsync the dir.
pub(crate) fn durable_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static TMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = path.parent().ok_or("no parent")?;
    if !dir.is_dir() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        // a new directory's own entry lives in its parent: make that durable too
        fsync_dir(dir.parent().ok_or("no parent")?)?;
    }
    // pid + per-process counter: concurrent writers (threads or processes) never share a temp
    let tmp = dir.join(format!(
        ".{}.{}.{}",
        path.file_name().ok_or("no file name")?.to_string_lossy(),
        std::process::id(),
        TMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let res = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res.map_err(|e| format!("{}: {e}", path.display()))?;
    fsync_dir(dir)
}

/// A skill name usable as one path component (champion pointer, publication dir).
pub(crate) fn valid_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.');
    if ok {
        Ok(())
    } else {
        Err(format!("not a valid skill name: {name:?}"))
    }
}

fn pointer_path(store: &Path, name: &str) -> Result<PathBuf, String> {
    valid_name(name)?;
    Ok(store.join("champions").join(format!("{name}.json")))
}

/// Exclusive lock over champion moves (`journal.lock`, O_EXCL), released on drop.
/// ponytail: a crashed holder leaves the file; the error names it for manual removal. Add
/// pid-liveness stealing if that ever happens in practice.
struct Lock(PathBuf);

impl Lock {
    fn take(store: &Path) -> Result<Lock, String> {
        let p = store.join("journal.lock");
        fs::create_dir_all(store).map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                Ok(mut f) => {
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(Lock(p));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if std::time::Instant::now() > deadline {
                        return Err(format!(
                            "{} is held by another dao process; if none is running, remove it and run dao recover",
                            p.display()
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => return Err(format!("{}: {e}", p.display())),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn champion(store: &Path, name: &str) -> Result<Option<Pointer>, String> {
    let p = pointer_path(store, name)?;
    match fs::read(&p) {
        Ok(b) => serde_json::from_slice::<Pointer>(&b)
            .map_err(|e| {
                format!(
                    "{}: corrupt champion pointer ({e}); restore it by hand from the last committed journal entry for {name}",
                    p.display()
                )
            })
            .map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

fn journal_dir(store: &Path) -> PathBuf {
    store.join("journal")
}

fn entries(store: &Path) -> Result<Vec<(PathBuf, Entry)>, String> {
    let mut out = Vec::new();
    let dir = journal_dir(store);
    // only a missing journal is an empty one; any other failure must not look like "no entries"
    let rd = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for e in rd {
        let e = e.map_err(|e| format!("{}: {e}", dir.display()))?;
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json")
            && !e.file_name().to_string_lossy().starts_with('.')
        {
            let b = fs::read(&p).map_err(|e| e.to_string())?;
            out.push((
                p.clone(),
                serde_json::from_slice(&b).map_err(|err| {
                    format!(
                        "{}: corrupt journal entry ({err}); repair or move it aside by hand, then run dao recover",
                        p.display()
                    )
                })?,
            ));
        }
    }
    out.sort_by_key(|(_, e)| e.seq);
    Ok(out)
}

/// Finish or abort every prepared entry: if the pointer already names `to`, the move happened
/// (commit); otherwise it did not (abort). Returns what was resolved.
pub fn recover(store: &Path) -> Result<Vec<Entry>, String> {
    let _lock = Lock::take(store)?;
    recover_locked(store)
}

fn recover_locked(store: &Path) -> Result<Vec<Entry>, String> {
    let mut done = Vec::new();
    for (path, mut e) in entries(store)? {
        if e.state != "prepared" {
            continue;
        }
        let now = champion(store, &e.name)?.map(|p| p.digest);
        e.state = if now.as_deref() == Some(e.to.as_str()) {
            "committed"
        } else {
            "aborted"
        }
        .into();
        durable_write(&path, &serde_json::to_vec_pretty(&e).unwrap())?;
        done.push(e);
    }
    Ok(done)
}

/// Move `name`'s champion to `to` (promotion), or with `to = None` back to the previous champion
/// (rollback), through the journal. Recovery runs first, under the lock: when it completes an
/// interrupted move of this name, a rollback returns that move instead of stepping back again,
/// so retrying a rollback that crashed after its pointer write steps back exactly once.
pub fn move_champion(
    store: &Path,
    name: &str,
    to: Option<&str>,
    fault: Fault,
) -> Result<Entry, String> {
    valid_name(name)?;
    let _lock = Lock::take(store)?;
    let recovered = recover_locked(store)?;
    let rollback = to.is_none();
    if rollback {
        if let Some(done) = recovered
            .into_iter()
            .rfind(|e| e.name == name && e.state == "committed")
        {
            return Ok(done);
        }
    }
    let target;
    let to = match to {
        Some(t) => t,
        None => {
            target = champion(store, name)?
                .and_then(|p| p.history.first().cloned())
                .ok_or_else(|| format!("{name}: no previous champion to roll back to"))?;
            &target
        }
    };
    // on a case-insensitive filesystem `Skill` and `skill` are one file: refuse the near-miss
    if let Ok(rd) = fs::read_dir(store.join("champions")) {
        let want = format!("{name}.json");
        for e in rd.flatten() {
            let f = e.file_name().to_string_lossy().into_owned();
            if f != want && f.to_lowercase() == want.to_lowercase() {
                return Err(format!(
                    "{name}: champion {f} differs only in case (one file on case-insensitive filesystems)"
                ));
            }
        }
    }
    let cur = champion(store, name)?;
    if cur.as_ref().is_some_and(|p| p.digest == to) {
        return Err(format!(
            "{name}: {} is already the champion (no-op)",
            short(to)
        ));
    }
    let seq = match entries(store)?.last() {
        None => 1,
        Some((_, e)) => e
            .seq
            .checked_add(1)
            .ok_or("journal sequence is exhausted")?,
    };
    let mut entry = Entry {
        seq,
        name: name.into(),
        from: cur.as_ref().map(|p| p.digest.clone()),
        to: to.into(),
        state: "prepared".into(),
    };
    let jpath = journal_dir(store).join(format!("{seq:08}.json"));
    durable_write(&jpath, &serde_json::to_vec_pretty(&entry).unwrap())?;
    if fault == Fault::AfterPrepare {
        return Err("injected fault after prepare".into());
    }
    let history = match (&cur, rollback) {
        // rollback pops the history; promotion pushes the outgoing champion
        (Some(p), true) => p.history.iter().skip(1).cloned().collect(),
        (Some(p), false) => std::iter::once(p.digest.clone())
            .chain(p.history.iter().cloned())
            .collect(),
        (None, _) => Vec::new(),
    };
    let ptr = Pointer {
        name: name.into(),
        digest: to.into(),
        history,
    };
    durable_write(
        &pointer_path(store, name)?,
        &serde_json::to_vec_pretty(&ptr).unwrap(),
    )?;
    if fault == Fault::AfterPointer {
        return Err("injected fault after pointer write".into());
    }
    entry.state = "committed".into();
    durable_write(&jpath, &serde_json::to_vec_pretty(&entry).unwrap())?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    use serde_json::json;

    /// Evidence for digest `a` named `x`: one trigger case t1 expecting x, one abstain case
    /// ab1, one should_not case n1. `receipt` overrides fields of an all-passing receipt.
    fn ev(
        verdict: &str,
        approval: Option<serde_json::Value>,
        receipt: serde_json::Value,
    ) -> Evidence {
        let a = d('a');
        let mut r = json!({"schema": crate::eval::RECEIPT_SCHEMA, "set_sha256": "h",
            "registry_sha256": "short", "searcher_sha256": "q", "n_skills": 1,
            "top_k": 3, "floor": 0.0, "rel": 0.5,
            "gated": ["should_abstain", "should_choose", "should_trigger"], "kinds": {},
            "failures": [], "unreachable": [], "scored": ["t1", "ab1", "n1"],
            "entries": {"x": d('e')}});
        for (k, v) in receipt.as_object().unwrap() {
            r[k] = v.clone();
        }
        Evidence {
            object_ok: Ok(()),
            scan: serde_json::from_value(json!({"digest": a, "scanner": "s",
            "rules_sha256": "r", "verdict": verdict, "findings": [],
            "findings_truncated": 0, "unresolved": []}))
            .ok(),
            approval,
            card: serde_json::from_value(json!({"digest": a, "status": "ok", "name": "x",
            "description": null, "language": "en", "use_when": [], "not_for": [],
            "invocation": [], "headings": [], "body_sha256": "", "frontmatter_sha256": "",
            "files_sha256": "", "tokens": [], "mentions": [], "sources": []}))
            .ok(),
            eval: Some((
                serde_json::from_value(json!({"schema": "s", "subjects": {"x": a},
                "cases": [
                    {"id": "t1", "kind": "should_trigger", "prompt": "p",
                     "expect": ["x"], "expect_not": [], "source": "use_when"},
                    {"id": "ab1", "kind": "should_abstain", "prompt": "hi",
                     "expect": [], "expect_not": [], "source": "abstain[0]"},
                    {"id": "n1", "kind": "should_not", "prompt": "not",
                     "expect": [], "expect_not": ["x"], "source": "not_for[0]"}],
                "set_sha256": "h"}))
                .unwrap(),
                true,
                serde_json::from_value(r).ok(),
            )),
            entry_sha256: Some(d('e')),
        }
    }

    fn eval_gate_of(e: &Evidence) -> Gate {
        let a = decide(&d('a'), e);
        a.gates.into_iter().find(|g| g.name == "eval").unwrap()
    }

    #[test]
    fn every_gate_must_pass_and_review_needs_a_named_approver() {
        let a = d('a');
        let ok = decide(&a, &ev("CLEAN", None, json!({})));
        assert!(ok.admitted, "{:?}", ok.gates);
        assert!(!decide(&a, &ev("CLEAN", None, json!({"failures": [["t1", []]]}))).admitted);
        assert!(!decide(&a, &ev("REVIEW_REQUIRED", None, json!({}))).admitted);
        let blank = json!({"digest": a, "approved_by": " "});
        assert!(!decide(&a, &ev("REVIEW_REQUIRED", Some(blank), json!({}))).admitted);
        let other = json!({"digest": d('b'), "approved_by": "me"});
        assert!(!decide(&a, &ev("REVIEW_REQUIRED", Some(other), json!({}))).admitted);
        let named = json!({"digest": a, "approved_by": "me"});
        assert!(decide(&a, &ev("REVIEW_REQUIRED", Some(named), json!({}))).admitted);
        assert!(!decide(&a, &ev("BLOCK", None, json!({}))).admitted);
        assert!(
            !decide(&d('b'), &ev("CLEAN", None, json!({}))).admitted,
            "receipts for another digest"
        );
    }

    /// B1: a case that was never scored (subject missing from the registry) cannot pass, and a
    /// pre-/2 receipt (no `scored`) does not even parse.
    #[test]
    fn an_unscored_case_never_passes() {
        let g = eval_gate_of(&ev(
            "CLEAN",
            None,
            json!({"scored": ["ab1", "n1"], "unreachable": ["x"]}),
        ));
        assert!(!g.passed && g.evidence.contains("not scored"), "{g:?}");
        let mut legacy = json!({"schema": "dao-eval-receipt/1", "set_sha256": "h",
            "registry_sha256": "r", "searcher_sha256": "q", "n_skills": 1, "top_k": 3,
            "floor": 0.0, "rel": 0.5, "gated": [], "kinds": {}, "failures": []});
        assert!(serde_json::from_value::<crate::eval::Receipt>(legacy.clone()).is_err());
        // even with the new fields, an old schema is refused
        legacy["scored"] = json!(["t1"]);
        legacy["unreachable"] = json!([]);
        legacy["entries"] = json!({"x": d('e')});
        let g = eval_gate_of(&ev("CLEAN", None, legacy));
        assert!(!g.passed && g.evidence.contains("schema"), "{g:?}");
    }

    /// B2: a gated failure that kept the candidate counts against it (over-triggering on an
    /// abstain prompt); one that kept someone else, or a should_not case, does not.
    #[test]
    fn over_triggering_counts_against_the_candidate() {
        let g = eval_gate_of(&ev("CLEAN", None, json!({"failures": [["ab1", ["x"]]]})));
        assert!(!g.passed && g.evidence.contains("ab1"), "{g:?}");
        let g = eval_gate_of(&ev("CLEAN", None, json!({"failures": [["ab1", ["y"]]]})));
        assert!(g.passed, "{g:?}");
        let g = eval_gate_of(&ev("CLEAN", None, json!({"failures": [["n1", ["x"]]]})));
        assert!(g.passed, "should_not is informational: {g:?}");
    }

    /// S1: the evaluated registry must have held this object's SKILL.md under the name.
    #[test]
    fn the_evaluated_registry_must_hold_this_content() {
        let g = eval_gate_of(&ev("CLEAN", None, json!({"entries": {"x": d('f')}})));
        assert!(!g.passed && g.evidence.contains("SKILL.md"), "{g:?}");
        let g = eval_gate_of(&ev("CLEAN", None, json!({"entries": {}})));
        assert!(!g.passed, "{g:?}");
        // multibyte hashes are shown without panicking
        let g = eval_gate_of(&ev(
            "CLEAN",
            None,
            json!({"entries": {"x": "é".repeat(20)}}),
        ));
        assert!(!g.passed);
    }

    /// S2: concurrent promotions of one name serialize; no journal entry or history is lost.
    #[test]
    fn concurrent_promotions_lose_nothing() {
        let store = std::env::temp_dir().join(format!("dao-adm-conc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&store);
        let digests: Vec<String> = "0123456789abcdef".chars().take(8).map(d).collect();
        std::thread::scope(|s| {
            for x in &digests {
                let store = &store;
                s.spawn(move || move_champion(store, "skill", Some(x), Fault::None).unwrap());
            }
        });
        let es = entries(&store).unwrap();
        let seqs: std::collections::BTreeSet<u64> = es.iter().map(|(_, e)| e.seq).collect();
        assert_eq!((es.len(), seqs.len()), (8, 8), "{es:?}");
        assert!(es.iter().all(|(_, e)| e.state == "committed"));
        let p = champion(&store, "skill").unwrap().unwrap();
        assert_eq!(p.history.len(), 7, "{p:?}");
        let _ = fs::remove_dir_all(&store);
    }

    /// An unreadable journal is an error, never an empty journal (fail closed).
    #[test]
    fn an_unreadable_journal_fails_closed() {
        use std::os::unix::fs::PermissionsExt;
        let store = std::env::temp_dir().join(format!("dao-adm-jr-{}", std::process::id()));
        let _ = fs::remove_dir_all(&store);
        assert!(
            recover(&store).unwrap().is_empty(),
            "missing journal = empty"
        );
        fs::create_dir_all(journal_dir(&store)).unwrap();
        fs::set_permissions(journal_dir(&store), fs::Permissions::from_mode(0o000)).unwrap();
        let r = recover(&store);
        fs::set_permissions(journal_dir(&store), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(r.is_err(), "{r:?}");
        let _ = fs::remove_dir_all(&store);
    }

    /// S3: names differing only in case are one file on macOS; refuse instead of clobbering.
    #[test]
    fn a_case_only_name_collision_is_refused() {
        let store = std::env::temp_dir().join(format!("dao-adm-case-{}", std::process::id()));
        let _ = fs::remove_dir_all(&store);
        move_champion(&store, "skill", Some(&d('a')), Fault::None).unwrap();
        let e = move_champion(&store, "Skill", Some(&d('b')), Fault::None).unwrap_err();
        assert!(e.contains("case"), "{e}");
        assert_eq!(champion(&store, "skill").unwrap().unwrap().digest, d('a'));
        // a corrupt pointer names its file and the way out
        fs::write(store.join("champions/skill.json"), b"{").unwrap();
        let e = champion(&store, "skill").unwrap_err();
        assert!(e.contains("skill.json") && e.contains("journal"), "{e}");
        let _ = fs::remove_dir_all(&store);
    }

    #[test]
    fn journal_survives_a_crash_at_every_point_and_rolls_back() {
        let store = std::env::temp_dir().join(format!("dao-admission-{}", std::process::id()));
        let _ = fs::remove_dir_all(&store);
        fs::create_dir_all(&store).unwrap();
        let (a, b, c) = (d('a'), d('b'), d('c'));
        move_champion(&store, "skill", Some(&a), Fault::None).unwrap();
        assert_eq!(champion(&store, "skill").unwrap().unwrap().digest, a);
        // promoting the champion again is a no-op, not a second promotion
        assert!(move_champion(&store, "skill", Some(&a), Fault::None)
            .unwrap_err()
            .contains("no-op"));
        // crash after prepare: recovery aborts, the old champion stays
        assert!(move_champion(&store, "skill", Some(&b), Fault::AfterPrepare).is_err());
        assert_eq!(champion(&store, "skill").unwrap().unwrap().digest, a);
        let r = recover(&store).unwrap();
        assert_eq!((r.len(), r[0].state.as_str()), (1, "aborted"));
        // crash after the pointer write: recovery commits, the new champion stays
        assert!(move_champion(&store, "skill", Some(&b), Fault::AfterPointer).is_err());
        let r = recover(&store).unwrap();
        assert_eq!((r.len(), r[0].state.as_str()), (1, "committed"));
        assert_eq!(champion(&store, "skill").unwrap().unwrap().digest, b);
        assert!(
            recover(&store).unwrap().is_empty(),
            "recovery is idempotent"
        );
        // promote c, then roll back twice: c → b → a
        move_champion(&store, "skill", Some(&c), Fault::None).unwrap();
        let p = champion(&store, "skill").unwrap().unwrap();
        assert_eq!(
            (p.digest.as_str(), p.history.clone()),
            (c.as_str(), vec![b.clone(), a.clone()])
        );
        // a rollback that crashes after its pointer write, retried, steps back exactly once
        assert!(move_champion(&store, "skill", None, Fault::AfterPointer).is_err());
        let e = move_champion(&store, "skill", None, Fault::None).unwrap();
        assert_eq!((e.to.as_str(), e.state.as_str()), (b.as_str(), "committed"));
        assert_eq!(champion(&store, "skill").unwrap().unwrap().digest, b);
        move_champion(&store, "skill", None, Fault::None).unwrap();
        let p = champion(&store, "skill").unwrap().unwrap();
        assert_eq!((p.digest.as_str(), p.history.len()), (a.as_str(), 0));
        assert!(move_champion(&store, "skill", None, Fault::None).is_err());
        // names cannot escape champions/
        assert!(move_champion(&store, "../x", Some(&a), Fault::None).is_err());
        // every journal entry ended committed or aborted, none left prepared
        assert!(entries(&store)
            .unwrap()
            .iter()
            .all(|(_, e)| e.state != "prepared"));
        let _ = fs::remove_dir_all(&store);
    }
}
