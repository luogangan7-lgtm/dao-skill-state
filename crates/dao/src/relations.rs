//! relations — explicit, evidence-carrying relations between routing cards. Never merges.
//!
//! Depends on: normalize (Card). Called by: `main` (`dao relations`); later eval/admission
//! read `relations.json` to build parent/child and confusable routing tests.
//! Invariants:
//! - Every relation names both sides and says why (`evidence`); nothing is deleted, merged or
//!   renamed here. A name collision is classified, not resolved.
//! - Output is deterministic: relations sorted by (kind, a, b).
//!
//! Kinds:
//! - `same_content`       one digest reached from several sources (exact duplicate)
//! - `same_entry`         different digests, byte-identical SKILL.md body + name (files differ)
//! - `translation`        same name, different description language
//! - `drift`              same name and language, different content, still similar (versions
//!   of one skill diverging, e.g. between hosts)
//! - `name_conflict`      same name, dissimilar content (different implementations)
//! - `near_duplicate`     different names, very similar bodies
//! - `family`             `a` is the name prefix of `b` (`beatra` → `beatra-ai-video-studio`)
//! - `references`         `a`'s body names `b` and at least two other known skills (see-also
//!   or umbrella candidate; per name pair, not per copy)

use crate::normalize::Card;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Relation {
    pub kind: String,
    pub a: String,
    pub b: String,
    pub evidence: String,
}

const DRIFT_MIN: f64 = 0.5;
const NEAR_DUP_MIN: f64 = 0.85;

fn jaccard(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let sa: HashSet<&String> = a.iter().collect();
    let inter = b.iter().filter(|t| sa.contains(t)).count();
    inter as f64 / (a.len() + b.len() - inter) as f64
}

fn label(c: &Card) -> String {
    let src = c
        .sources
        .first()
        .map(|s| {
            format!(
                "{}:{}",
                s.origin.rsplit('/').next().unwrap_or(""),
                s.subpath
            )
        })
        .unwrap_or_default();
    format!("{}@{}", c.name.as_deref().unwrap_or("?"), &c.digest[..12]) + &format!(" ({src})")
}

pub fn build(cards: &[Card]) -> Vec<Relation> {
    let mut out = Vec::new();
    let rel = |kind: &str, a: &Card, b: &Card, evidence: String| Relation {
        kind: kind.into(),
        a: label(a),
        b: label(b),
        evidence,
    };
    for c in cards.iter().filter(|c| c.sources.len() > 1) {
        let srcs: Vec<String> = c
            .sources
            .iter()
            .map(|s| format!("{}:{}", s.origin, s.subpath))
            .collect();
        out.push(Relation {
            kind: "same_content".into(),
            a: label(c),
            b: label(c),
            evidence: format!("{} sources: {}", srcs.len(), srcs.join(", ")),
        });
    }
    // same name: classify every pair
    let mut by_name: BTreeMap<&str, Vec<&Card>> = BTreeMap::new();
    for c in cards {
        if let Some(n) = c.name.as_deref() {
            by_name.entry(n).or_default().push(c);
        }
    }
    for group in by_name.values() {
        for (i, a) in group.iter().enumerate() {
            for b in &group[i + 1..] {
                let sim = jaccard(&a.tokens, &b.tokens);
                let (kind, why) = if a.body_sha256 == b.body_sha256 {
                    (
                        "same_entry",
                        "identical SKILL.md body; supporting files differ".to_string(),
                    )
                } else if a.language != b.language {
                    (
                        "translation",
                        format!(
                            "languages {} / {}, body similarity {sim:.2}",
                            a.language, b.language
                        ),
                    )
                } else if sim >= DRIFT_MIN {
                    (
                        "drift",
                        format!("same name and language, body similarity {sim:.2}"),
                    )
                } else {
                    (
                        "name_conflict",
                        format!("same name, body similarity {sim:.2}"),
                    )
                };
                out.push(rel(kind, a, b, why));
            }
        }
    }
    // different names: near duplicates (ponytail: O(n²) pairs; MinHash buckets past ~20k cards)
    for (i, a) in cards.iter().enumerate() {
        for b in &cards[i + 1..] {
            if a.name == b.name || a.tokens.len() < 20 || b.tokens.len() < 20 {
                continue;
            }
            let small = a.tokens.len().min(b.tokens.len()) as f64;
            let big = a.tokens.len().max(b.tokens.len()) as f64;
            if small / big < NEAR_DUP_MIN {
                continue; // jaccard cannot reach the bar
            }
            let sim = jaccard(&a.tokens, &b.tokens);
            if sim >= NEAR_DUP_MIN {
                out.push(rel(
                    "near_duplicate",
                    a,
                    b,
                    format!("different names, body similarity {sim:.2}"),
                ));
            }
        }
    }
    // families, per name pair (not per copy): name prefix, or an umbrella body naming ≥3
    // other skills including b
    let mut fam: BTreeMap<(&str, &str), String> = BTreeMap::new();
    for a in cards {
        let Some(an) = a.name.as_deref() else {
            continue;
        };
        let mentioned: Vec<&str> = a
            .mentions
            .iter()
            .map(String::as_str)
            .filter(|m| *m != an && by_name.contains_key(m))
            .collect();
        for &n in by_name.keys() {
            if n == an || fam.contains_key(&(an, n)) {
                continue;
            }
            if n.starts_with(&format!("{an}-")) {
                fam.insert((an, n), format!("name prefix `{an}-`"));
            } else if mentioned.len() >= 3 && mentioned.contains(&n) {
                fam.insert(
                    (an, n),
                    format!(
                        "umbrella body names {} skills incl. this one ({})",
                        mentioned.len(),
                        label(a)
                    ),
                );
            }
        }
    }
    for ((a, b), why) in fam {
        // a body that names other skills is a "see also", not proof of an umbrella/child pair
        out.push(Relation {
            kind: if why.starts_with("name prefix") {
                "family"
            } else {
                "references"
            }
            .into(),
            a: a.into(),
            b: b.into(),
            evidence: why,
        });
    }
    out.sort_by(|x, y| (&x.kind, &x.a, &x.b).cmp(&(&y.kind, &y.a, &y.b)));
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::{tokens, SourceRef};

    fn card(name: &str, lang: &str, body: &str, digest: char) -> Card {
        Card {
            digest: std::iter::repeat_n(digest, 64).collect(),
            status: "ok".into(),
            name: Some(name.into()),
            description: Some("d".into()),
            language: lang.into(),
            use_when: vec![],
            not_for: vec![],
            invocation: vec![],
            headings: vec![],
            body_sha256: format!("{body:?}"),
            tokens: tokens(body),
            mentions: vec![],
            sources: vec![SourceRef {
                origin: "o".into(),
                subpath: name.into(),
                commit: None,
            }],
        }
    }

    #[test]
    fn classifies_collisions_and_families_without_merging() {
        let long = "alpha beta gamma delta epsilon zeta theta iota kappa lambda omicron sigma tau upsilon phi chi psi omega rust cargo crate";
        let cards = vec![
            card("council", "latin", long, 'a'),
            card("council", "zh", "议会 技能 中文 说明", 'b'),
            card("ponytail", "latin", &format!("{long} lazy"), 'c'),
            card("ponytail", "latin", &format!("{long} lazy senior"), 'd'),
            card("deploy", "latin", "totally different words here", 'e'),
            card(
                "deploy",
                "latin",
                "nothing shared with that one at all",
                'f',
            ),
            card("beatra", "latin", "media", '1'),
            card("beatra-ai-video-studio", "latin", "video", '2'),
            card("copycat", "latin", long, '3'),
        ];
        let r = build(&cards);
        let kinds = |k: &str| r.iter().filter(|x| x.kind == k).count();
        assert_eq!(kinds("translation"), 1);
        assert_eq!(kinds("drift"), 1);
        assert_eq!(kinds("name_conflict"), 1);
        assert_eq!(kinds("family"), 1);
        assert!(r
            .iter()
            .any(|x| x.kind == "near_duplicate" && x.a.starts_with("co") && x.b.starts_with("co")));
        assert_eq!(build(&cards), r, "deterministic");
    }
}
