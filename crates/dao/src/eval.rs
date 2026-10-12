//! eval — routing tests generated from routing cards and relations, and an offline runner.
//!
//! Depends on: normalize (Card), relations (Relation); an external searcher binary (skill-hub
//! `search` contract: `BIN search --registry REG --top-k K -- PROMPT` → JSON [{id, score}]).
//! Called by: `main` (`dao eval gen|run`); admission reads `evals/receipt.json`.
//! Invariants:
//! - Every prompt is copied from a card (a use-when / not-for sentence or the description) or
//!   from the fixed abstain list in this file; nothing is generated. Each case says where its
//!   prompt came from.
//! - A set is frozen by `set_sha256` (canonical JSON of its cases); a receipt binds the set,
//!   registry and searcher hashes, so a number always names exactly what produced it.
//! - Routing correctness only: this measures whether the right skill is put in front of the
//!   model, not whether the skill then does the task well (that is a separate gate, G4).
//! - `should_not` is informational: its prompts are the card's own exclusion sentences, which
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
pub const RECEIPT_SCHEMA: &str = "dao-eval-receipt/1";

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
    for (a, b) in pairs {
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
    let set_sha256 = hex(&Sha256::digest(serde_json::to_vec(&cases).unwrap()));
    Set {
        schema: SET_SCHEMA.into(),
        subjects: subj
            .into_iter()
            .map(|(n, c)| (n, c.digest.clone()))
            .collect(),
        cases,
        set_sha256,
    }
}

/// Recompute a set's hash; Err when the cases no longer match it (edited after freezing).
pub fn verify_set(s: &Set) -> Result<(), String> {
    let h = hex(&Sha256::digest(serde_json::to_vec(&s.cases).unwrap()));
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
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Receipt {
    pub schema: String,
    pub set_sha256: String,
    pub registry_sha256: String,
    pub searcher_sha256: String,
    pub top_k: usize,
    /// Hook keep rule: score ≥ max(floor, rel × top score).
    pub floor: f64,
    pub rel: f64,
    pub kinds: BTreeMap<String, KindScore>,
    /// Case ids that failed, with the kept hits, for diagnosis.
    pub failures: Vec<(String, Vec<String>)>,
}

/// Per-kind scores and the failed case ids with the hits that were kept.
pub type Scored = (BTreeMap<String, KindScore>, Vec<(String, Vec<String>)>);

/// `(id, score)` hits for one prompt from the searcher.
pub type Searcher<'a> = dyn Fn(&str) -> Result<Vec<(String, f64)>, String> + 'a;

/// Score every case. A case passes when, among the hits the hook would keep:
/// trigger — an expected name is kept; choose — the expected name is the first kept hit;
/// should_not — no excluded name is kept; abstain — nothing is kept.
pub fn run(
    set: &Set,
    search: &Searcher,
    top_k: usize,
    floor: f64,
    rel: f64,
) -> Result<Scored, String> {
    verify_set(set)?;
    let mut kinds: BTreeMap<String, KindScore> = BTreeMap::new();
    let mut failures = Vec::new();
    for c in &set.cases {
        let hits = search(&c.prompt)?;
        let top = hits.first().map(|h| h.1).unwrap_or(0.0);
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
            "should_abstain" => kept.is_empty(),
            other => return Err(format!("unknown case kind {other}")),
        };
        let k = kinds.entry(c.kind.clone()).or_insert(KindScore {
            total: 0,
            passed: 0,
        });
        k.total += 1;
        k.passed += usize::from(ok);
        if !ok {
            failures.push((c.id.clone(), kept));
        }
    }
    Ok((kinds, failures))
}

/// The skill-hub `search` contract as a searcher.
pub fn skill_hub(
    bin: PathBuf,
    registry: PathBuf,
    top_k: usize,
) -> impl Fn(&str) -> Result<Vec<(String, f64)>, String> {
    move |prompt: &str| {
        let out = Command::new(&bin)
            .args(["search", "--registry"])
            .arg(&registry)
            .args(["--top-k", &top_k.to_string(), "--", prompt])
            .output()
            .map_err(|e| format!("{}: {e}", bin.display()))?;
        if !out.status.success() {
            return Err(format!(
                "searcher failed: {}",
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
                    h["score"].as_f64().ok_or("searcher hit without score")?,
                ))
            })
            .collect()
    }
}

pub fn receipt(
    set: &Set,
    registry: &Path,
    searcher: &Path,
    top_k: usize,
    floor: f64,
    rel: f64,
    scored: Scored,
) -> Result<Receipt, String> {
    Ok(Receipt {
        schema: RECEIPT_SCHEMA.into(),
        set_sha256: set.set_sha256.clone(),
        registry_sha256: sha_file(registry)?,
        searcher_sha256: sha_file(searcher)?,
        top_k,
        floor,
        rel,
        kinds: scored.0,
        failures: scored.1,
    })
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
        let (k, f) = run(&s, &perfect, 3, 6.0, 0.5).unwrap();
        assert!(f.is_empty(), "{f:?}");
        assert_eq!(k["should_abstain"].passed, 10);
        let hijack = |_: &str| -> Result<Vec<(String, f64)>, String> {
            Ok(vec![("deploy".into(), 20.0), ("deploy-k8s".into(), 19.0)])
        };
        let (k, _) = run(&s, &hijack, 3, 6.0, 0.5).unwrap();
        assert_eq!(k["should_choose"].passed, 1);
        assert_eq!(k["should_abstain"].passed, 0);
        assert_eq!(
            k["should_not"].passed, 0,
            "Not for Kubernetes prompt kept deploy"
        );
        // an edited set is rejected
        let mut bad = s.clone();
        bad.cases[0].prompt.push('!');
        assert!(run(&bad, &perfect, 3, 6.0, 0.5).is_err());
    }
}
