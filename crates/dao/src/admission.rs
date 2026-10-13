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
//!   the card is `ok`; the eval set regenerates from the store, names this digest as the subject
//!   of its name, and the receipt for that set has no failed and no unreachable case expecting
//!   this name (D1 review: unreachable cases must not pass silently).
//! - A champion pointer (`champions/<name>.json`) only moves through the journal:
//!   prepare (journal entry, fsync) → pointer (tmp + fsync + rename + dir fsync) → commit
//!   (journal entry updated). `recover` finishes or aborts any prepared entry by looking at the
//!   pointer, so a crash at any point leaves either the old or the new champion, never a mix.
//!   Promoting the current champion again is a no-op; rollback restores the previous champion
//!   through the same journal.

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
                format!(
                    "{why} (rules {})",
                    &r.rules_sha256[..12.min(r.rules_sha256.len())]
                ),
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
            let mine: Vec<&crate::eval::Case> =
                set.cases.iter().filter(|c| c.expect.contains(n)).collect();
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
                        subject.map_or("none", |s| &s[..12.min(s.len())])
                    ),
                )
            } else if mine.is_empty() {
                (false, format!("no eval case expects {n}"))
            } else {
                match receipt {
                    None => (
                        false,
                        "no eval receipt for this set (run dao eval run)".into(),
                    ),
                    Some(r) => {
                        let failed: Vec<&str> = mine
                            .iter()
                            .filter(|c| r.failures.iter().any(|(id, _)| *id == c.id))
                            .map(|c| c.id.as_str())
                            .collect();
                        let unreachable = r.unreachable.contains(n);
                        if unreachable {
                            (false, format!("{n} is not an id in the evaluated registry"))
                        } else if !failed.is_empty() {
                            (
                                false,
                                format!(
                                    "{} of {} cases failed: {}",
                                    failed.len(),
                                    mine.len(),
                                    failed.join(", ")
                                ),
                            )
                        } else {
                            (
                                true,
                                format!(
                                    "{} cases passed (receipt registry {})",
                                    mine.len(),
                                    &r.registry_sha256[..12.min(r.registry_sha256.len())]
                                ),
                            )
                        }
                    }
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

fn fsync_dir(dir: &Path) -> Result<(), String> {
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| format!("{}: {e}", dir.display()))
}

/// Durable atomic write: temp file in the same dir, fsync, rename, fsync the dir.
pub(crate) fn durable_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(
        ".{}.{}",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
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

/// A skill name that is safe as one path component (champion pointer, published directory).
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

pub fn champion(store: &Path, name: &str) -> Result<Option<Pointer>, String> {
    let p = pointer_path(store, name)?;
    match fs::read(&p) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

fn journal_dir(store: &Path) -> PathBuf {
    store.join("journal")
}

fn entries(store: &Path) -> Result<Vec<(PathBuf, Entry)>, String> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(journal_dir(store)) else {
        return Ok(out);
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json")
            && !e.file_name().to_string_lossy().starts_with('.')
        {
            let b = fs::read(&p).map_err(|e| e.to_string())?;
            out.push((
                p.clone(),
                serde_json::from_slice(&b).map_err(|err| format!("{}: {err}", p.display()))?,
            ));
        }
    }
    out.sort_by_key(|(_, e)| e.seq);
    Ok(out)
}

/// Finish or abort every prepared entry: if the pointer already names `to`, the move happened
/// (commit); otherwise it did not (abort). Returns what was resolved.
pub fn recover(store: &Path) -> Result<Vec<Entry>, String> {
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

/// Move `name`'s champion to `to` (promotion or rollback) through the journal.
pub fn move_champion(
    store: &Path,
    name: &str,
    to: &str,
    rollback: bool,
    fault: Fault,
) -> Result<Entry, String> {
    recover(store)?;
    let cur = champion(store, name)?;
    if cur.as_ref().is_some_and(|p| p.digest == to) {
        return Err(format!(
            "{name}: {} is already the champion (no-op)",
            &to[..12.min(to.len())]
        ));
    }
    let seq = entries(store)?.last().map_or(1, |(_, e)| e.seq + 1);
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

/// The champion `name` would roll back to (the previous one), if any.
pub fn rollback_target(store: &Path, name: &str) -> Result<String, String> {
    champion(store, name)?
        .and_then(|p| p.history.first().cloned())
        .ok_or_else(|| format!("{name}: no previous champion to roll back to"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    #[test]
    fn every_gate_must_pass_and_review_needs_a_named_approver() {
        use serde_json::json;
        let a = d('a');
        let ev =
            |verdict: &str, approval: Option<serde_json::Value>, failures: serde_json::Value| {
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
                        "cases": [{"id": "t1", "kind": "should_trigger", "prompt": "p",
                                   "expect": ["x"], "expect_not": [], "source": "use_when"}],
                        "set_sha256": "h"}))
                        .unwrap(),
                        true,
                        serde_json::from_value(json!({"schema": "s", "set_sha256": "h",
                        "registry_sha256": "short", "searcher_sha256": "q", "n_skills": 1,
                        "top_k": 3, "floor": 0.0, "rel": 0.5, "gated": [], "kinds": {},
                        "failures": failures}))
                        .ok(),
                    )),
                }
            };
        let ok = decide(&a, &ev("CLEAN", None, json!([])));
        assert!(ok.admitted, "{:?}", ok.gates);
        assert!(!decide(&a, &ev("CLEAN", None, json!([["t1", []]]))).admitted);
        assert!(!decide(&a, &ev("REVIEW_REQUIRED", None, json!([]))).admitted);
        let blank = json!({"digest": a, "approved_by": " "});
        assert!(!decide(&a, &ev("REVIEW_REQUIRED", Some(blank), json!([]))).admitted);
        let other = json!({"digest": d('b'), "approved_by": "me"});
        assert!(!decide(&a, &ev("REVIEW_REQUIRED", Some(other), json!([]))).admitted);
        let named = json!({"digest": a, "approved_by": "me"});
        assert!(decide(&a, &ev("REVIEW_REQUIRED", Some(named), json!([]))).admitted);
        assert!(!decide(&a, &ev("BLOCK", None, json!([]))).admitted);
        assert!(
            !decide(&d('b'), &ev("CLEAN", None, json!([]))).admitted,
            "receipts for another digest"
        );
    }

    #[test]
    fn journal_survives_a_crash_at_every_point_and_rolls_back() {
        let store = std::env::temp_dir().join(format!("dao-admission-{}", std::process::id()));
        let _ = fs::remove_dir_all(&store);
        fs::create_dir_all(&store).unwrap();
        let (a, b, c) = (d('a'), d('b'), d('c'));
        move_champion(&store, "skill", &a, false, Fault::None).unwrap();
        assert_eq!(champion(&store, "skill").unwrap().unwrap().digest, a);
        // promoting the champion again is a no-op, not a second promotion
        assert!(move_champion(&store, "skill", &a, false, Fault::None)
            .unwrap_err()
            .contains("no-op"));
        // crash after prepare: recovery aborts, the old champion stays
        assert!(move_champion(&store, "skill", &b, false, Fault::AfterPrepare).is_err());
        assert_eq!(champion(&store, "skill").unwrap().unwrap().digest, a);
        let r = recover(&store).unwrap();
        assert_eq!((r.len(), r[0].state.as_str()), (1, "aborted"));
        // crash after the pointer write: recovery commits, the new champion stays
        assert!(move_champion(&store, "skill", &b, false, Fault::AfterPointer).is_err());
        let r = recover(&store).unwrap();
        assert_eq!((r.len(), r[0].state.as_str()), (1, "committed"));
        assert_eq!(champion(&store, "skill").unwrap().unwrap().digest, b);
        assert!(
            recover(&store).unwrap().is_empty(),
            "recovery is idempotent"
        );
        // promote c, then roll back twice: c → b → a
        move_champion(&store, "skill", &c, false, Fault::None).unwrap();
        let p = champion(&store, "skill").unwrap().unwrap();
        assert_eq!(
            (p.digest.as_str(), p.history.clone()),
            (c.as_str(), vec![b.clone(), a.clone()])
        );
        let back = rollback_target(&store, "skill").unwrap();
        move_champion(&store, "skill", &back, true, Fault::None).unwrap();
        assert_eq!(champion(&store, "skill").unwrap().unwrap().digest, b);
        let back = rollback_target(&store, "skill").unwrap();
        move_champion(&store, "skill", &back, true, Fault::None).unwrap();
        let p = champion(&store, "skill").unwrap().unwrap();
        assert_eq!((p.digest.as_str(), p.history.len()), (a.as_str(), 0));
        assert!(rollback_target(&store, "skill").is_err());
        // names cannot escape champions/
        assert!(move_champion(&store, "../x", &a, false, Fault::None).is_err());
        // every journal entry ended committed or aborted, none left prepared
        assert!(entries(&store)
            .unwrap()
            .iter()
            .all(|(_, e)| e.state != "prepared"));
        let _ = fs::remove_dir_all(&store);
    }
}
