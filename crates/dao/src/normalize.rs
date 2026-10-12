//! normalize — derive a deterministic routing card from a staged object, without inventing.
//!
//! Depends on: check (PyYAML-compatible frontmatter), quarantine (Manifest), source (locks).
//! Called by: `main` (`dao normalize`), relations (reads cards).
//! Invariants:
//! - Every text field is copied verbatim from the skill (description, trigger/exclusion
//!   sentences, headings, invocation lines). Nothing is paraphrased or generated; a skill with
//!   no usable description gets `status: needs_description`, never a made-up one.
//! - The card is a pure function of the object bytes and its locks: same input, same card
//!   (`cards/<digest>.json`). A changed card means a changed object, i.e. a new candidate.

use crate::check::{parse_frontmatter, Val};
use crate::quarantine::Manifest;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SourceRef {
    pub origin: String,
    pub subpath: String,
    pub commit: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Card {
    pub digest: String,
    /// ok | needs_description | invalid_frontmatter
    pub status: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Locale from the source path (`docs/tr/…`, `docs/zh-CN/…`) when one names it, else the
    /// script of the description (body if none): zh | ja | ko | latin | unknown
    pub language: String,
    /// Sentences of the description that say when to use it (verbatim).
    pub use_when: Vec<String>,
    /// Sentences of the description that exclude uses (verbatim).
    pub not_for: Vec<String>,
    /// Frontmatter lines that change how hosts invoke it (verbatim `key: value`).
    pub invocation: Vec<String>,
    /// First `## ` headings of the body (verbatim, at most 12).
    pub headings: Vec<String>,
    /// sha256 of SKILL.md after the frontmatter: same entry text, different supporting files.
    pub body_sha256: String,
    /// Lowercased word / CJK-bigram set of the body, for relations (sorted, deduplicated).
    pub tokens: Vec<String>,
    /// Hyphenated identifiers named in the body (`foo-bar`), sorted: candidate skill names.
    pub mentions: Vec<String>,
    pub sources: Vec<SourceRef>,
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Script of a text by letter counts: kana → ja, Hangul → ko, Han → zh, else latin.
pub fn language(text: &str) -> &'static str {
    let (mut han, mut kana, mut hangul, mut latin) = (0, 0, 0, 0);
    for c in text.chars() {
        match c {
            '\u{3040}'..='\u{30ff}' => kana += 1,
            '\u{ac00}'..='\u{d7af}' | '\u{1100}'..='\u{11ff}' => hangul += 1,
            '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' => han += 1,
            c if c.is_alphabetic() => latin += 1,
            _ => {}
        }
    }
    let cjk = han + kana + hangul;
    if cjk + latin == 0 {
        "unknown"
    } else if kana * 10 > cjk + latin && kana > 0 {
        "ja"
    } else if hangul * 5 > cjk + latin {
        "ko"
    } else if (han + kana) * 5 > cjk + latin {
        // ponytail: ratio heuristic; mixed zh/en descriptions with ≥20% Han count as zh
        if kana > 0 {
            "ja"
        } else {
            "zh"
        }
    } else {
        "latin"
    }
}

/// A locale directory in a source path (`docs/tr/skills/x` → `tr`, `i18n/zh-CN/x` → `zh-CN`):
/// a two-letter code, optionally with a region, directly under docs/, i18n/ or locales/.
pub fn path_locale(subpath: &str) -> Option<String> {
    let parts: Vec<&str> = subpath.split('/').collect();
    parts.windows(2).find_map(|w| {
        let (dir, code) = (w[0], w[1]);
        let (lang, region) = code.split_once('-').unwrap_or((code, ""));
        let ok = matches!(dir, "docs" | "i18n" | "locales" | "translations")
            && lang.len() == 2
            && lang.chars().all(|c| c.is_ascii_lowercase())
            && (region.is_empty()
                || (region.len() == 2 && region.chars().all(|c| c.is_ascii_uppercase())));
        ok.then(|| code.to_string())
    })
}

/// Words (lowercased ASCII alphanumerics, length ≥ 3) plus CJK character bigrams.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut word = String::new();
    let mut prev_cjk: Option<char> = None;
    for c in text.chars().chain(std::iter::once(' ')) {
        let cjk = matches!(c, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '\u{ac00}'..='\u{d7af}');
        if cjk {
            if let Some(p) = prev_cjk {
                out.insert(format!("{p}{c}"));
            }
            prev_cjk = Some(c);
        } else {
            prev_cjk = None;
        }
        if c.is_ascii_alphanumeric() {
            word.push(c.to_ascii_lowercase());
        } else {
            if word.len() >= 3 {
                out.insert(std::mem::take(&mut word));
            }
            word.clear();
        }
    }
    out.into_iter().collect()
}

fn sentences(text: &str) -> Vec<String> {
    static SPLIT: OnceLock<Regex> = OnceLock::new();
    let re = SPLIT.get_or_init(|| Regex::new(r"(?:[.!?](?:\s+|$)|[。！？；;]\s*|\n+)").unwrap());
    re.split(text)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

struct Patterns {
    use_when: Regex,
    not_for: Regex,
    invocation: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        use_when: Regex::new(r"(?i)\b(use\s+(this|it|when|for|whenever|on|if)|triggers?\s+(on|when)|invoke\s+when|USE WHEN)\b|使用场景|适用于|用于|当.{0,20}时|使用する|場合に").unwrap(),
        not_for: Regex::new(r"(?i)\b(not\s+for|do\s+not\s+use|don't\s+use|not\s+a\s+|not\s+(?:intended|meant)|excludes?|never\s+use|instead\s+use)\b|不用于|不适用|不要用于|不是|ではない|使わない").unwrap(),
        invocation: Regex::new(r"(?i)^(disable-model-invocation|user-invocable|allowed-tools|tools|model|context|agent|license)\s*:").unwrap(),
    })
}

/// Split SKILL.md into (frontmatter lines, body) the way check_skill delimits it.
fn split(text: &str) -> (Vec<&str>, &str) {
    let mut lines = text.split_inclusive('\n');
    let mut consumed = 0;
    match lines.next() {
        Some(first) if first.trim_end_matches(['\r', '\n']) == "---" => consumed += first.len(),
        _ => return (Vec::new(), text),
    }
    let mut fm = Vec::new();
    for l in lines {
        consumed += l.len();
        let t = l.trim_end_matches(['\r', '\n']);
        if t == "---" {
            return (fm, &text[consumed..]);
        }
        fm.push(t);
    }
    (Vec::new(), text)
}

/// Build the card for `objects/<digest>`; sources come from `locks/<digest>/*.json`.
pub fn card(store: &Path, digest: &str) -> Result<Card, String> {
    let mpath = store.join("manifests").join(format!("{digest}.json"));
    let _: Manifest =
        serde_json::from_slice(&fs::read(&mpath).map_err(|e| format!("{}: {e}", mpath.display()))?)
            .map_err(|e| e.to_string())?;
    let raw =
        fs::read(store.join("objects").join(digest).join("SKILL.md")).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw).replace("\r\n", "\n");
    let (fm_lines, body) = split(&text);
    let fm = parse_frontmatter(&text);
    let field = |k: &str| match &fm {
        Ok(m) => m
            .iter()
            .find(|(key, _)| key == k)
            .and_then(|(_, v)| match v {
                Val::Str(s) => Some(s.trim().to_string()),
                _ => None,
            }),
        Err(_) => None,
    };
    let (name, description) = (
        field("name"),
        field("description").filter(|d| !d.is_empty()),
    );
    let status = match (&fm, &description) {
        (Err(_), _) => "invalid_frontmatter",
        (Ok(_), None) => "needs_description",
        _ => "ok",
    };
    let p = patterns();
    let desc_sentences = description.as_deref().map(sentences).unwrap_or_default();
    let mut sources: Vec<SourceRef> = fs::read_dir(store.join("locks").join(digest))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| fs::read(e.path()).ok())
                .filter_map(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .map(|v| SourceRef {
                    origin: v["origin"].as_str().unwrap_or("").into(),
                    subpath: v["subpath"].as_str().unwrap_or("").into(),
                    commit: v["commit"].as_str().map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    sources.sort_by(|a, b| (&a.origin, &a.subpath).cmp(&(&b.origin, &b.subpath)));
    Ok(Card {
        digest: digest.into(),
        status: status.into(),
        language: sources
            .iter()
            .find_map(|s| path_locale(&s.subpath))
            .unwrap_or_else(|| language(description.as_deref().unwrap_or(body)).into()),
        use_when: desc_sentences
            .iter()
            .filter(|s| p.use_when.is_match(s))
            .cloned()
            .collect(),
        not_for: desc_sentences
            .iter()
            .filter(|s| p.not_for.is_match(s))
            .cloned()
            .collect(),
        invocation: fm_lines
            .iter()
            .filter(|l| p.invocation.is_match(l))
            .map(|l| l.trim().to_string())
            .collect(),
        headings: body
            .lines()
            .filter_map(|l| l.strip_prefix("## "))
            .map(|h| h.trim().to_string())
            .take(12)
            .collect(),
        body_sha256: hex(&Sha256::digest(body.as_bytes())),
        tokens: tokens(body),
        mentions: {
            static ID: OnceLock<Regex> = OnceLock::new();
            let re = ID.get_or_init(|| Regex::new(r"\b[a-z0-9]+(?:-[a-z0-9]+)+\b").unwrap());
            let set: std::collections::BTreeSet<String> =
                re.find_iter(body).map(|m| m.as_str().to_string()).collect();
            set.into_iter().collect()
        },
        name,
        description,
        sources,
    })
}

pub fn write_card(store: &Path, c: &Card) -> Result<(), String> {
    let dir = store.join("cards");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{}.{}", c.digest, std::process::id()));
    let res = fs::write(&tmp, serde_json::to_vec_pretty(c).unwrap())
        .and_then(|_| fs::rename(&tmp, dir.join(format!("{}.json", c.digest))));
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_fields_and_language() {
        let d =
            "Create carousels from a brief. Use for social slides. Not a generic image generator.";
        let s = sentences(d);
        let p = patterns();
        let uw: Vec<_> = s.iter().filter(|x| p.use_when.is_match(x)).collect();
        let nf: Vec<_> = s.iter().filter(|x| p.not_for.is_match(x)).collect();
        assert_eq!(uw, ["Use for social slides"]);
        assert_eq!(nf, ["Not a generic image generator"]);
        assert_eq!(language("为 1688 制作一个元 skill"), "zh");
        assert_eq!(language("スキルを使用する場合"), "ja");
        assert_eq!(language("Plain English text"), "latin");
        assert_eq!(
            path_locale("docs/tr/skills/golang-testing").as_deref(),
            Some("tr")
        );
        assert_eq!(
            path_locale("docs/zh-CN/skills/council").as_deref(),
            Some("zh-CN")
        );
        assert_eq!(path_locale("skills/docs-writer"), None);
        assert_eq!(path_locale("docs/api/x"), None);
        assert_eq!(
            tokens("Rust 化 crate, rust! 道法"),
            vec!["crate", "rust", "道法"]
        );
        let (fm, body) = split("---\nname: a\n---\n## H\nx\n");
        assert_eq!((fm, body), (vec!["name: a"], "## H\nx\n"));
    }
}
