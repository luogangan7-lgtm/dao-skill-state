//! eval — routing tests generated from routing cards and relations, and an offline runner.
//!
//! Depends on: normalize (Card), relations (Relation); an external searcher binary (skill-hub
//! `search` contract: `BIN search --registry REG --top-k K -- PROMPT` → JSON [{id, score}]).
//! Called by: `main` (`dao eval gen|run`); admission reads `evals/receipt-<id>.json`, where the
//! id hashes the set, registry, searcher and keep-rule parameters.
//! Invariants:
//! - Every prompt is copied from a card (a use-when / not-for sentence or the description) or
//!   from the fixed abstain list in this file; nothing is generated. Each case says where its
//!   prompt came from.
//! - A set is frozen by `set_sha256` (canonical JSON of its cases); a receipt binds the set,
//!   registry and searcher hashes, so a number always names exactly what produced it.
//! - Routing correctness only: this measures whether the right skill is put in front of the
//!   model, not whether the skill then does the task well (that is a separate gate, G4).
//! - Decision (D2): `should_not` is informational: its prompts are the card's own exclusion sentences, which
//!   share the skill's vocabulary, so retrieval keeps the skill and the model must apply the
//!   exclusion. Admission gates on trigger / choose / abstain, not on should_not.

use crate::normalize::Card;
use crate::relations::Relation;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SET_SCHEMA: &str = "dao-eval-set/1";
/// /2 (D2 review): receipts record which cases were scored and the registry entry hash of each
/// subject; /1 receipts lack both and are refused, so an unscored case can never pass.
pub const RECEIPT_SCHEMA: &str = "dao-eval-receipt/2";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Case {
    pub id: String,
    /// should_trigger | should_not | should_choose | should_abstain
    pub kind: String,
    pub prompt: String,
    /// Skill names one of which must be kept (trigger) or ranked first (choose).
    pub expect: Vec<String>,
    /// Skill names that must not be kept (should_not) or must not outrank `expect` (choose).
    pub expect_not: Vec<String>,
    /// Where the prompt text came from (`card:<digest>:use_when[0]`, `abstain[3]`, …).
    pub source: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Set {
    pub schema: String,
    /// The names under test, one representative card each.
    pub subjects: BTreeMap<String, String>,
    pub cases: Vec<Case>,
    pub set_sha256: String,
}

/// Prompts no skill should take (abstain), fixed so the set stays reproducible.
const ABSTAIN: [&str; 10] = [
    "What is 17 times 23?",
    "Say hello back to me.",
    "What day comes after Tuesday?",
    "Thanks, that is all for now.",
    "Is it going to be sunny tomorrow where I live?",
    "Tell me a short joke about cats.",
    "你好，今天过得怎么样？",
    "What does the word 'ephemeral' mean?",
    "Convert 5 kilometres to miles.",
    "ok",
];

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha_file(p: &Path) -> Result<String, String> {
    Ok(hex(&Sha256::digest(
        fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?,
    )))
}

/// One representative card per name: status ok, Latin-script first (the default language of
/// the corpus), then the smallest digest — deterministic.
fn subjects(cards: &[Card]) -> BTreeMap<String, &Card> {
    let mut by: BTreeMap<String, &Card> = BTreeMap::new();
    for c in cards.iter().filter(|c| c.status == "ok") {
        let Some(n) = c.name.clone() else { continue };
        let better = |old: &Card| {
            (c.language != "latin", &c.digest) < (old.language != "latin", &old.digest)
        };
        if by.get(&n).is_none_or(|old| better(old)) {
            by.insert(n, c);
        }
    }
    by
}

fn name_of(label: &str) -> &str {
    label.split('@').next().unwrap_or(label)
}

/// Build the set: triggers from use-when sentences (or the description), exclusions from
/// not-for sentences, choose cases for each name pair a relation says can be confused, and
/// the fixed abstain prompts.
pub fn generate(cards: &[Card], rels: &[Relation]) -> Set {
    let subj = subjects(cards);
    let mut cases = Vec::new();
    for (name, c) in &subj {
        let triggers: Vec<(String, String)> = if c.use_when.is_empty() {
            c.description
                .iter()
                .map(|d| (d.clone(), "description".to_string()))
                .collect()
        } else {
            c.use_when
                .iter()
                .enumerate()
                .map(|(i, s)| (s.clone(), format!("use_when[{i}]")))
                .collect()
        };
        for (p, src) in triggers {
            cases.push(Case {
                id: String::new(),
                kind: "should_trigger".into(),
                prompt: p,
                expect: vec![name.clone()],
                expect_not: vec![],
                source: format!("card:{}:{src}", c.digest),
            });
        }
        for (i, s) in c.not_for.iter().enumerate() {
            cases.push(Case {
                id: String::new(),
                kind: "should_not".into(),
                prompt: s.clone(),
                expect: vec![],
                expect_not: vec![name.clone()],
                source: format!("card:{}:not_for[{i}]", c.digest),
            });
        }
    }
    // confusable names (family, near duplicate): each side's own trigger must win over the other
    let mut pairs: Vec<(String, String)> = rels
        .iter()
        .filter(|r| matches!(r.kind.as_str(), "family" | "near_duplicate"))
        .map(|r| (name_of(&r.a).to_string(), name_of(&r.b).to_string()))
        .filter(|(a, b)| a != b && subj.contains_key(a) && subj.contains_key(b))
        .collect();
    pairs.sort();
    pairs.dedup();
    let first_prompt = |c: &Card| c.use_when.first().or(c.description.as_ref()).cloned();
    for (a, b) in pairs {
        if first_prompt(subj[&a]) == first_prompt(subj[&b]) {
            continue; // identical prompts expect opposite winners: not a test, a coin flip
        }
        for (me, other) in [(&a, &b), (&b, &a)] {
            let c = subj[me];
            if let Some(p) = c.use_when.first().or(c.description.as_ref()) {
                cases.push(Case {
                    id: String::new(),
                    kind: "should_choose".into(),
                    prompt: p.clone(),
                    expect: vec![me.clone()],
                    expect_not: vec![other.clone()],
                    source: format!(
                        "card:{}:{}",
                        c.digest,
                        if c.use_when.is_empty() {
                            "description"
                        } else {
                            "use_when[0]"
                        }
                    ),
                });
            }
        }
    }
    for (i, p) in ABSTAIN.iter().enumerate() {
        cases.push(Case {
            id: String::new(),
            kind: "should_abstain".into(),
            prompt: (*p).into(),
            expect: vec![],
            expect_not: vec![],
            source: format!("abstain[{i}]"),
        });
    }
    for (i, c) in cases.iter_mut().enumerate() {
        c.id = format!("{}-{i:05}", c.kind);
    }
    let subjects: BTreeMap<String, String> = subj
        .into_iter()
        .map(|(n, c)| (n, c.digest.clone()))
        .collect();
    let set_sha256 = set_hash(SET_SCHEMA, &subjects, &cases);
    Set {
        schema: SET_SCHEMA.into(),
        subjects,
        cases,
        set_sha256,
    }
}

/// The set's identity: schema, subjects and cases together.
fn set_hash(schema: &str, subjects: &BTreeMap<String, String>, cases: &[Case]) -> String {
    hex(&Sha256::digest(
        serde_json::to_vec(&(schema, subjects, cases)).unwrap(),
    ))
}

/// Recompute a set's hash; Err when it no longer matches (edited after freezing).
/// (Tamper resistance comes from `run` regenerating the set from the store, not from this.)
pub fn verify_set(s: &Set) -> Result<(), String> {
    let h = set_hash(&s.schema, &s.subjects, &s.cases);
    if s.schema != SET_SCHEMA || h != s.set_sha256 {
        return Err(format!(
            "eval set is not the frozen set (sha {h} != {})",
            s.set_sha256
        ));
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct KindScore {
    pub total: usize,
    pub passed: usize,
    /// Cases whose expected skill is not an id in the registry (not scored).
    pub unreachable: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Receipt {
    pub schema: String,
    pub set_sha256: String,
    pub registry_sha256: String,
    pub searcher_sha256: String,
    /// Skills in the registry (the hook's floor depends on it).
    pub n_skills: usize,
    pub top_k: usize,
    /// Hook keep rule: score ≥ max(floor, rel × top score).
    pub floor: f64,
    pub rel: f64,
    /// Kinds that gate admission; the others are reported only.
    pub gated: Vec<String>,
    pub kinds: BTreeMap<String, KindScore>,
    /// Case ids that failed, with the kept hits, for diagnosis.
    pub failures: Vec<(String, Vec<String>)>,
    /// Subject names the registry has no id for (their cases were not scored).
    pub unreachable: Vec<String>,
    /// Ids of the cases that were actually scored; a case not listed here never passes.
    pub scored: Vec<String>,
    /// Subject name → the evaluated registry's `entry_sha256` (SKILL.md) for that id, so
    /// admission can check the registry held this candidate's content.
    pub entries: BTreeMap<String, String>,
}

/// Per-kind scores, the failed case ids with the hits that were kept, and the scored case ids.
pub type Scored = (
    BTreeMap<String, KindScore>,
    Vec<(String, Vec<String>)>,
    Vec<String>,
);

/// `(id, score)` hits for one prompt from the searcher.
pub type Searcher<'a> = dyn Fn(&str) -> Result<Vec<(String, f64)>, String> + 'a;

/// Score every case. A case passes when, among the hits the hook would keep:
/// trigger — an expected name is kept; choose — the expected name is the first kept hit;
/// should_not — no excluded name is kept; abstain — nothing is kept.
pub fn run(
    set: &Set,
    search: &Searcher,
    known_ids: &std::collections::HashSet<String>,
    top_k: usize,
    floor: f64,
    rel: f64,
) -> Result<Scored, String> {
    verify_set(set)?;
    let mut kinds: BTreeMap<String, KindScore> = KINDS
        .iter()
        .map(|k| {
            (
                k.to_string(),
                KindScore {
                    total: 0,
                    passed: 0,
                    unreachable: 0,
                },
            )
        })
        .collect();
    let mut failures = Vec::new();
    let mut scored = Vec::new();
    for c in &set.cases {
        let k = kinds
            .get_mut(&c.kind)
            .ok_or(format!("unknown case kind {}", c.kind))?;
        // a named skill the registry cannot return is a broken case, not a routing result: an
        // absent expected skill cannot be kept, and an absent competitor (choose) cannot lose
        if c.expect
            .iter()
            .chain(&c.expect_not)
            .any(|e| !known_ids.contains(e))
        {
            k.unreachable += 1;
            continue;
        }
        let hits = search(&c.prompt)?;
        let top = hits.iter().map(|h| h.1).fold(0.0, f64::max);
        let kept: Vec<String> = hits
            .into_iter()
            .take(top_k)
            .filter(|(_, s)| *s >= floor.max(rel * top))
            .map(|(id, _)| id)
            .collect();
        let ok = match c.kind.as_str() {
            "should_trigger" => kept.iter().any(|k| c.expect.contains(k)),
            "should_choose" => kept.first().is_some_and(|k| c.expect.contains(k)),
            "should_not" => !kept.iter().any(|k| c.expect_not.contains(k)),
            _ => kept.is_empty(),
        };
        k.total += 1;
        k.passed += usize::from(ok);
        scored.push(c.id.clone());
        if !ok {
            failures.push((c.id.clone(), kept));
        }
    }
    Ok((kinds, failures, scored))
}

/// Every kind is always reported; only these gate admission (should_not is informational).
pub const KINDS: [&str; 4] = [
    "should_abstain",
    "should_choose",
    "should_not",
    "should_trigger",
];
pub const GATED: [&str; 3] = ["should_abstain", "should_choose", "should_trigger"];

/// The hook's absolute keep floor for a registry of `n` skills (skill-hub hook.rs
/// `floor_for`): BM25 IDF grows with corpus size, so the 6.0 floor calibrated at ~2,000
/// skills shrinks with the largest possible IDF. Must stay identical to the hook.
pub fn hook_floor(n: usize) -> f64 {
    let max_idf = |n: f64| ((n - 1.0 + 0.5) / 1.5 + 1.0).ln();
    6.0 * (max_idf(n.max(1) as f64) / max_idf(2000.0)).min(1.0)
}

/// The production hook's keep policy for a registry of `n` skills: (top_k, floor, rel). `dao
/// eval run` defaults to it and admission accepts only receipts produced under it.
pub fn policy(n: usize) -> (usize, f64, f64) {
    (3, hook_floor(n), 0.5)
}

/// The skill-hub `search` contract as a searcher.
pub fn skill_hub(
    bin: PathBuf,
    registry: PathBuf,
    top_k: usize,
) -> impl Fn(&str) -> Result<Vec<(String, f64)>, String> {
    move |prompt: &str| {
        let mut child = Command::new(&bin)
            .args(["search", "--registry"])
            .arg(&registry)
            .args(["--top-k", &top_k.to_string(), "--", prompt])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", bin.display()))?;
        // a hung searcher must not hang the run
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(SEARCH_TIMEOUT_S);
        while child.try_wait().map_err(|e| e.to_string())?.is_none() {
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("searcher timed out after {SEARCH_TIMEOUT_S}s"));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "searcher failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let v: Vec<serde_json::Value> =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("searcher output: {e}"))?;
        v.iter()
            .map(|h| {
                Ok((
                    h["id"]
                        .as_str()
                        .ok_or("searcher hit without id")?
                        .to_string(),
                    h["score"]
                        .as_f64()
                        .filter(|s| s.is_finite())
                        .ok_or("searcher hit without a finite score")?,
                ))
            })
            .collect()
    }
}

const SEARCH_TIMEOUT_S: u64 = 30;

/// Resolve a searcher the way the OS would (a bare name via PATH), so the binary that is hashed
/// is the binary that runs.
pub fn resolve_bin(bin: &str) -> Result<PathBuf, String> {
    let p = Path::new(bin);
    let found = if bin.contains('/') {
        Some(p.to_path_buf())
    } else {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|d| d.join(bin))
                .find(|c| c.is_file())
        })
    };
    let f = found.ok_or(format!("searcher {bin} not found"))?;
    fs::canonicalize(&f).map_err(|e| format!("{}: {e}", f.display()))
}

/// Hash the inputs now; `receipt` re-hashes them after the run and refuses if they changed.
pub fn input_hashes(registry: &Path, searcher: &Path) -> Result<(String, String), String> {
    Ok((sha_file(registry)?, sha_file(searcher)?))
}

#[allow(clippy::too_many_arguments)]
pub fn receipt(
    set: &Set,
    registry: &Path,
    searcher: &Path,
    before: (String, String),
    n_skills: usize,
    top_k: usize,
    floor: f64,
    rel: f64,
    scored: Scored,
) -> Result<Receipt, String> {
    if input_hashes(registry, searcher)? != before {
        return Err("registry or searcher changed during the run".into());
    }
    Ok(Receipt {
        schema: RECEIPT_SCHEMA.into(),
        set_sha256: set.set_sha256.clone(),
        registry_sha256: before.0,
        searcher_sha256: before.1,
        n_skills,
        top_k,
        floor,
        rel,
        gated: GATED.iter().map(|s| s.to_string()).collect(),
        kinds: scored.0,
        failures: scored.1,
        scored: scored.2,
        unreachable: Vec::new(),
        entries: BTreeMap::new(),
    })
}

impl Receipt {
    /// File name unique to everything that produced the numbers.
    pub fn file_name(&self) -> String {
        let id = format!(
            "{}|{}|{}|{}|{}|{}",
            self.set_sha256,
            self.registry_sha256,
            self.searcher_sha256,
            self.top_k,
            self.floor,
            self.rel
        );
        format!(
            "receipt-{}.json",
            &hex(&Sha256::digest(id.as_bytes()))[..16]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::SourceRef;

    fn card(name: &str, use_when: &[&str], not_for: &[&str], d: char) -> Card {
        Card {
            digest: std::iter::repeat_n(d, 64).collect(),
            status: "ok".into(),
            name: Some(name.into()),
            description: Some(format!("{name} does things")),
            language: "latin".into(),
            use_when: use_when.iter().map(|s| s.to_string()).collect(),
            not_for: not_for.iter().map(|s| s.to_string()).collect(),
            invocation: vec![],
            headings: vec![],
            body_sha256: String::new(),
            frontmatter_sha256: String::new(),
            files_sha256: String::new(),
            tokens: vec![],
            mentions: vec![],
            sources: vec![SourceRef {
                origin: "o".into(),
                subpath: name.into(),
                commit: None,
            }],
        }
    }

    #[test]
    fn generated_from_cards_scored_by_the_hook_rule() {
        let cards = vec![
            card(
                "deploy",
                &["Use for Vercel previews."],
                &["Not for Kubernetes."],
                'a',
            ),
            card("deploy-k8s", &["Use for Kubernetes rollouts."], &[], 'b'),
        ];
        let rels = vec![Relation {
            kind: "family".into(),
            a: "deploy".into(),
            b: "deploy-k8s".into(),
            evidence: String::new(),
        }];
        let s = generate(&cards, &rels);
        let known: std::collections::HashSet<String> =
            ["deploy", "deploy-k8s"].map(String::from).into();
        // every prompt is card text or the abstain list
        for c in &s.cases {
            let from_card = cards
                .iter()
                .any(|k| k.use_when.contains(&c.prompt) || k.not_for.contains(&c.prompt));
            assert!(from_card || ABSTAIN.contains(&c.prompt.as_str()), "{c:?}");
        }
        let count = |k: &str| s.cases.iter().filter(|c| c.kind == k).count();
        assert_eq!(
            (
                count("should_trigger"),
                count("should_not"),
                count("should_choose"),
                count("should_abstain")
            ),
            (2, 1, 2, 10)
        );
        assert_eq!(generate(&cards, &rels), s, "deterministic");
        // a perfect searcher passes everything; a hijacking one fails the choose cases
        let perfect = |p: &str| -> Result<Vec<(String, f64)>, String> {
            Ok(if p.contains("Vercel") {
                vec![("deploy".into(), 20.0)]
            } else if p.contains("Kubernetes rollouts") {
                vec![("deploy-k8s".into(), 20.0), ("deploy".into(), 3.0)]
            } else {
                vec![]
            })
        };
        let (k, f, _) = run(&s, &perfect, &known, 3, 6.0, 0.5).unwrap();
        assert!(f.is_empty(), "{f:?}");
        assert_eq!(k["should_abstain"].passed, 10);
        let hijack = |_: &str| -> Result<Vec<(String, f64)>, String> {
            Ok(vec![("deploy".into(), 20.0), ("deploy-k8s".into(), 19.0)])
        };
        let (k, _, _) = run(&s, &hijack, &known, 3, 6.0, 0.5).unwrap();
        assert_eq!(k["should_choose"].passed, 1);
        assert_eq!(k["should_abstain"].passed, 0);
        assert_eq!(
            k["should_not"].passed, 0,
            "Not for Kubernetes prompt kept deploy"
        );
        // a subject missing from the registry is unreachable, not a routing failure
        let only: std::collections::HashSet<String> = ["deploy"].map(String::from).into();
        let (k, _, _) = run(&s, &perfect, &only, 3, 6.0, 0.5).unwrap();
        assert_eq!(
            (k["should_trigger"].total, k["should_trigger"].unreachable),
            (1, 1)
        );
        // a choose case whose competitor is missing from the registry is not scored either:
        // leaving the sibling out must not let the candidate win its choose case for free
        assert_eq!(
            (k["should_choose"].total, k["should_choose"].unreachable),
            (0, 2)
        );
        // all four kinds are always reported
        assert_eq!(k.len(), 4);
        // two confusable skills with the same first sentence produce no choose case
        let twins = vec![
            card("a", &["Use for x."], &[], 'c'),
            card("b", &["Use for x."], &[], 'd'),
        ];
        let tr = vec![Relation {
            kind: "near_duplicate".into(),
            a: "a@cc".into(),
            b: "b@dd".into(),
            evidence: String::new(),
        }];
        assert!(generate(&twins, &tr)
            .cases
            .iter()
            .all(|c| c.kind != "should_choose"));
        // the floor matches skill-hub's hook (6.0 at >= 2000 skills, smaller below)
        assert!((hook_floor(5000) - 6.0).abs() < 1e-9 && (hook_floor(386) - 4.63).abs() < 0.01);
        // an edited set is rejected
        let mut bad = s.clone();
        bad.cases[0].prompt.push('!');
        assert!(run(&bad, &perfect, &known, 3, 6.0, 0.5).is_err());
    }
}
