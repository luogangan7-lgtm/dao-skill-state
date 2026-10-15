//! relations — explicit, evidence-carrying relations between routing cards. Never merges.
//!
//! Depends on: normalize (Card). Called by: `main` (`dao relations`); later eval/admission
//! read `relations.json` to build parent/child and confusable routing tests.
//! Invariants:
//! - Every relation names both sides and says why (`evidence`, stating what actually differs);
//!   nothing is deleted, merged or renamed here. A name collision is classified, not resolved.
//! - Output is deterministic: relations sorted by (kind, a, b).
//!
//! Kinds (same-name pairs are classified in this order):
//! - `same_content`   one digest reached from several sources (exact duplicate)
//! - `stub_copy`      same name, one side is a placeholder body (`status: stub`)
//! - `translation`    same name, different language (the evidence says when the body itself
//!   was left untranslated)
//! - `same_entry`     same name and language, byte-identical body; the evidence says whether
//!   the frontmatter and the supporting files differ
//! - `drift`          same name and language, still similar (equal description, Jaccard ≥ 0.5
//!   or containment ≥ 0.8) or the same skill directory in one repository under several host
//!   trees: one skill's versions diverging, e.g. between hosts
//! - `name_conflict`  same name, dissimilar content from different sources (different
//!   implementations)
//! - `near_duplicate` different names, very similar bodies (stubs and invalid cards excluded)
//! - `family`         `a` is the name prefix of `b` (`beatra` → `beatra-ai-video-studio`)
//! - `references`     `a`'s body names `b` and at least two other known skills: a see-also or
//!   umbrella candidate, not proof of one (per name pair, not per copy)

use crate::normalize::Card;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Relation {
    pub kind: String,
    pub a: String,
    pub b: String,
    pub evidence: String,
}

const DRIFT_MIN: f64 = 0.5;
const CONTAIN_MIN: f64 = 0.8;
const NEAR_DUP_MIN: f64 = 0.85;

/// |a ∩ b| for sorted, deduplicated token lists.
fn overlap(a: &[String], b: &[String]) -> usize {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n
}

/// (Jaccard, containment = overlap / smaller set); 0 when either side has no tokens.
fn similarity(a: &[String], b: &[String]) -> (f64, f64) {
    if a.is_empty() || b.is_empty() {
        return (0.0, 0.0);
    }
    let n = overlap(a, b) as f64;
    let jac = n / (a.len() as f64 + b.len() as f64 - n);
    (jac, n / a.len().min(b.len()) as f64)
}

/// `name@digest12 (host/dir:subpath)` — two origin segments tell `.claude/skills` from
/// `.codex/skills`.
fn label(c: &Card) -> String {
    let src = c
        .sources
        .first()
        .map(|s| {
            let mut segs: Vec<&str> = s.origin.trim_end_matches('/').rsplit('/').take(2).collect();
            segs.reverse();
            format!("{}:{}", segs.join("/"), s.subpath)
        })
        .unwrap_or_default();
    let short = c.digest.get(..12).unwrap_or(&c.digest);
    format!("{}@{} ({src})", c.name.as_deref().unwrap_or("?"), short)
}

fn usable(c: &Card) -> bool {
    matches!(c.status.as_str(), "ok" | "stub" | "needs_description")
}

fn same_name(a: &Card, b: &Card) -> (&'static str, String) {
    let (jac, contain) = similarity(&a.tokens, &b.tokens);
    if a.status == "stub" || b.status == "stub" {
        let side = if a.status == "stub" { "a" } else { "b" };
        return ("stub_copy", format!("placeholder body on side {side}"));
    }
    // two Latin-script "languages" with near-identical bodies: a locale directory holding an
    // untranslated copy (docs/es/ with English text), not a translation
    let latin = |l: &str| {
        !matches!(
            l,
            "zh" | "zh-Hant"
                | "ja"
                | "ko"
                | "ru"
                | "ar"
                | "he"
                | "el"
                | "th"
                | "hi"
                | "uk"
                | "bg"
                | "sr"
                | "fa"
                | "ur"
        )
    };
    if a.language != b.language && latin(&a.language) && latin(&b.language) && jac >= 0.8 {
        return (
            "drift",
            format!(
                "locale {} / {} but the text is not translated, body similarity {jac:.2}",
                a.language, b.language
            ),
        );
    }
    if a.language != b.language {
        let body = if a.body_sha256 == b.body_sha256 {
            "; body identical (not translated)"
        } else {
            ""
        };
        return (
            "translation",
            format!(
                "languages {} / {}{body}, body similarity {jac:.2}",
                a.language, b.language
            ),
        );
    }
    if a.body_sha256 == b.body_sha256 {
        let fm = if a.frontmatter_sha256 == b.frontmatter_sha256 {
            "identical"
        } else {
            "differs"
        };
        let files = if a.files_sha256 == b.files_sha256 {
            "identical"
        } else {
            "differ"
        };
        return (
            "same_entry",
            format!("identical SKILL.md body; frontmatter {fm}; supporting files {files}"),
        );
    }
    if a.description.is_some() && a.description == b.description {
        return (
            "drift",
            format!("same description, body similarity {jac:.2} (containment {contain:.2})"),
        );
    }
    if jac >= DRIFT_MIN || contain >= CONTAIN_MIN {
        return (
            "drift",
            format!("body similarity {jac:.2}, containment {contain:.2}"),
        );
    }
    // one repository shipping the same name under several host directories (.kiro/, pi/,
    // .agents/, docs/<lang>/) is one skill's lineage, however far the copies have diverged
    // one repository shipping the same skill directory under several host trees (.kiro/,
    // pi/core/, .agents/): same origin AND same directory name. Two different directories
    // that merely share a frontmatter name stay a conflict.
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let shared = a.sources.iter().any(|s| {
        b.sources
            .iter()
            .any(|t| t.origin == s.origin && base(&t.subpath) == base(&s.subpath))
    });
    // a multi-vendor collection can hold two unrelated skills in same-named directories: the
    // ports really share content (containment 0.54–0.69 on ECC), unrelated ones don't
    if shared && contain >= 0.3 {
        return (
            "drift",
            format!("same skill directory in one source repository, body similarity {jac:.2}, containment {contain:.2}"),
        );
    }
    (
        "name_conflict",
        format!("body similarity {jac:.2}, containment {contain:.2}"),
    )
}

pub fn build(cards: &[Card]) -> Vec<Relation> {
    let cards: Vec<&Card> = cards.iter().filter(|c| usable(c)).collect();
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
        out.push(rel(
            "same_content",
            c,
            c,
            format!("{} sources: {}", srcs.len(), srcs.join(", ")),
        ));
    }
    let mut by_name: BTreeMap<&str, Vec<&Card>> = BTreeMap::new();
    for c in &cards {
        if let Some(n) = c.name.as_deref() {
            by_name.entry(n).or_default().push(c);
        }
    }
    for group in by_name.values() {
        for (i, a) in group.iter().enumerate() {
            for b in &group[i + 1..] {
                let (kind, why) = same_name(a, b);
                out.push(rel(kind, a, b, why));
            }
        }
    }
    // different names: near duplicates among real skills (ponytail: O(n²) pairs with a cheap
    // size bound; MinHash buckets past ~20k cards)
    let full: Vec<&Card> = cards
        .iter()
        .copied()
        .filter(|c| c.status == "ok" && c.tokens.len() >= 20)
        .collect();
    for (i, a) in full.iter().enumerate() {
        for b in &full[i + 1..] {
            if a.name == b.name {
                continue;
            }
            let (small, big) = (
                a.tokens.len().min(b.tokens.len()) as f64,
                a.tokens.len().max(b.tokens.len()) as f64,
            );
            if small / big < NEAR_DUP_MIN {
                continue; // Jaccard cannot reach the bar
            }
            let (jac, _) = similarity(&a.tokens, &b.tokens);
            if jac >= NEAR_DUP_MIN {
                out.push(rel(
                    "near_duplicate",
                    a,
                    b,
                    format!("different names, body similarity {jac:.2}"),
                ));
            }
        }
    }
    // families and references, per name pair (not per copy)
    let names: Vec<&str> = by_name.keys().copied().collect(); // sorted
    let mut fam: BTreeMap<(&str, &str), (&str, String)> = BTreeMap::new();
    for a in &cards {
        let Some(an) = a.name.as_deref() else {
            continue;
        };
        let prefix = format!("{an}-");
        let start = names.partition_point(|n| *n < prefix.as_str());
        for &n in names[start..].iter().take_while(|n| n.starts_with(&prefix)) {
            fam.entry((an, n))
                .or_insert(("family", format!("name prefix `{prefix}`")));
        }
        let mentioned: Vec<&str> = a
            .mentions
            .iter()
            .map(String::as_str)
            .filter(|m| *m != an && by_name.contains_key(m))
            .collect();
        if mentioned.len() >= 3 {
            for &n in &mentioned {
                fam.entry((an, n)).or_insert((
                    "references",
                    format!(
                        "body names {} known skills incl. this one ({}); see-also, not proof of an umbrella",
                        mentioned.len(),
                        label(a)
                    ),
                ));
            }
        }
    }
    for ((a, b), (kind, why)) in fam {
        out.push(Relation {
            kind: kind.into(),
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
            description: Some(format!("d {digest}")),
            language: lang.into(),
            use_when: vec![],
            not_for: vec![],
            invocation: vec![],
            headings: vec![],
            body_sha256: format!("{body:?}"),
            frontmatter_sha256: format!("fm {digest}"),
            files_sha256: "files".into(),
            tokens: tokens(body),
            mentions: vec![],
            form: String::new(),
            state_evidence: vec![],
            state_gaps: vec![],
            sources: vec![SourceRef {
                origin: "/h/.claude/skills".into(),
                subpath: name.into(),
                commit: None,
            }],
        }
    }

    #[test]
    fn classifies_collisions_and_families_without_merging() {
        let long = "alpha beta gamma delta epsilon zeta theta iota kappa lambda omicron sigma tau upsilon phi chi psi omega rust cargo crate";
        // cards from another repository: same name there is a real conflict, not lineage
        let other = |mut c: Card| {
            c.sources[0].origin = "https://github.com/other/skills.git".into();
            c
        };
        let mut stub = card("council", "ja", "翻訳中", '9');
        stub.status = "stub".into();
        let cards = vec![
            card("council", "latin", long, 'a'),
            card("council", "zh", "议会 技能 中文 说明", 'b'),
            stub,
            card("ponytail", "latin", &format!("{long} lazy"), 'c'),
            card("ponytail", "latin", &format!("{long} lazy senior"), 'd'),
            card("ponytail", "latin", &format!("{long} lazy"), 'x'),
            card("deploy", "latin", "totally different words here", 'e'),
            other(card(
                "deploy",
                "latin",
                "nothing shared with that one at all",
                'f',
            )),
            card("beatra", "latin", "media", '1'),
            card("beatra-ai-video-studio", "latin", "video", '2'),
            card("copycat", "latin", long, '3'),
            card("пример", "ru", "совсем другой текст навыка", '4'),
            other(card("пример", "ru", "ничего общего здесь нет вообще", '5')),
        ];
        let r = build(&cards);
        let kinds = |k: &str| r.iter().filter(|x| x.kind == k).count();
        assert_eq!(kinds("translation"), 1);
        assert_eq!(kinds("stub_copy"), 2);
        assert_eq!(kinds("family"), 1);
        // same body: says frontmatter differs and files are identical, not "files differ"
        let se: Vec<_> = r.iter().filter(|x| x.kind == "same_entry").collect();
        assert_eq!(se.len(), 1);
        assert!(
            se[0].evidence.contains("frontmatter differs")
                && se[0].evidence.contains("supporting files identical")
        );
        // non-ASCII bodies are compared on real tokens, not two empty sets
        assert!(
            r.iter()
                .any(|x| x.kind == "name_conflict" && x.a.starts_with("пример")),
            "{r:?}"
        );
        assert!(r
            .iter()
            .any(|x| x.kind == "name_conflict" && x.a.starts_with("deploy")));
        assert!(r
            .iter()
            .any(|x| x.kind == "near_duplicate" && x.a.starts_with("co") && x.b.starts_with("co")));
        assert!(r
            .iter()
            .filter(|x| x.kind != "family")
            .all(|x| x.a.contains(".claude/skills")));
        assert_eq!(build(&cards), r, "deterministic");
        // same repository, same skill directory, diverged port that still shares content:
        // lineage (drift); unrelated bodies in same-named vendor directories: a conflict
        let mut kiro = card(
            "deploy",
            "latin",
            "vercel preview production rollback kiro hooks",
            'g',
        );
        kiro.sources[0].subpath = ".kiro/skills/deploy".into();
        let r2 = build(&[
            card(
                "deploy",
                "latin",
                "vercel preview production rollback steps",
                'e',
            ),
            kiro,
        ]);
        assert_eq!(r2[0].kind, "drift", "{r2:?}");
        let mut va = card("deploy2", "latin", "vercel preview production", 'k');
        va.sources[0].subpath = "vendor-a/deploy2".into();
        let mut vb = card("deploy2", "latin", "kubernetes helm charts rollout", 'l');
        vb.sources[0].subpath = "vendor-b/deploy2".into();
        assert_eq!(build(&[va, vb])[0].kind, "name_conflict");
        let mut legacy = card("deploy", "latin", "kubernetes rollout helm charts", 'h');
        legacy.sources[0].subpath = "deploy-legacy".into();
        let r3 = build(&[
            card("deploy", "latin", "vercel preview production", 'e'),
            legacy,
        ]);
        assert_eq!(r3[0].kind, "name_conflict", "{r3:?}");
        // a locale directory holding an untranslated copy is not a translation
        let mut es = card(
            "guide",
            "es",
            "same english words here for every reader of guide",
            'i',
        );
        es.sources[0].subpath = "docs/es/guide".into();
        let r4 = build(&[
            card(
                "guide",
                "latin",
                "same english words here for every reader of guide",
                'j',
            ),
            es,
        ]);
        assert_eq!(r4[0].kind, "drift", "{r4:?}");
        // a short digest no longer panics
        let mut bad = card("x", "latin", "y", 'z');
        bad.digest = "abc".into();
        let _ = build(&[bad]);
    }
}
