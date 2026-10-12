//! normalize — derive a deterministic routing card from a staged object, without inventing.
//!
//! Depends on: check (PyYAML-compatible frontmatter), quarantine (Manifest), scan (is_digest).
//! Called by: `main` (`dao normalize`), relations (reads cards).
//! Invariants:
//! - Every text field is copied from the skill, never paraphrased or generated: name and
//!   description are the YAML-decoded values (so folded/quoted scalars are decoded, not byte
//!   substrings); sentences, headings and invocation lines are verbatim substrings. A skill
//!   without a usable value gets a status (`needs_description`, `needs_name`, `stub`,
//!   `invalid_frontmatter`, `invalid_encoding`), never a made-up value.
//! - The card is a pure function of the object bytes, its manifest and its locks
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
    /// ok | stub | needs_description | needs_name | invalid_frontmatter | invalid_encoding
    pub status: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Base language: the source path's locale (`docs/tr/…` → `tr`) when the description's or
    /// the body's script agrees with it, else the description's script (body if none):
    /// zh | ja | ko | ru(cyrillic) | ar | he | el | th | hi(devanagari) | latin | unknown.
    pub language: String,
    /// Sentences of the description that say when to use it (verbatim; exclusions removed).
    pub use_when: Vec<String>,
    /// Sentences of the description that exclude uses (verbatim).
    pub not_for: Vec<String>,
    /// Frontmatter lines that change how hosts invoke it (verbatim, with continuation lines).
    pub invocation: Vec<String>,
    /// First `## ` headings of the body (verbatim, at most 12).
    pub headings: Vec<String>,
    /// sha256 of SKILL.md after the frontmatter.
    pub body_sha256: String,
    /// sha256 of the frontmatter lines.
    pub frontmatter_sha256: String,
    /// sha256 of the manifest entries other than SKILL.md (the supporting files).
    pub files_sha256: String,
    /// Lowercased words (any script, ≥ 3 chars) plus CJK bigrams of the body, sorted unique.
    pub tokens: Vec<String>,
    /// Hyphenated identifiers named in the body (`foo-bar`), sorted: candidate skill names.
    pub mentions: Vec<String>,
    pub sources: Vec<SourceRef>,
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Script of a text by letter counts.
pub fn language(text: &str) -> &'static str {
    let mut n = [0usize; 10]; // han kana hangul cyr arab heb greek thai deva latin
    for c in text.chars() {
        let i = match c {
            '\u{3040}'..='\u{30ff}' => 1,
            '\u{ac00}'..='\u{d7af}' | '\u{1100}'..='\u{11ff}' => 2,
            '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' => 0,
            '\u{0400}'..='\u{04ff}' => 3,
            '\u{0600}'..='\u{06ff}' => 4,
            '\u{0590}'..='\u{05ff}' => 5,
            '\u{0370}'..='\u{03ff}' => 6,
            '\u{0e00}'..='\u{0e7f}' => 7,
            '\u{0900}'..='\u{097f}' => 8,
            c if c.is_alphabetic() => 9,
            _ => continue,
        };
        n[i] += 1;
    }
    let total: usize = n.iter().sum();
    if total == 0 {
        return "unknown";
    }
    // ponytail: ratio heuristic; ≥20% of letters in a non-Latin script decides the script
    if n[1] > 0 && (n[0] + n[1]) * 5 > total {
        return "ja";
    }
    let names = ["zh", "ja", "ko", "ru", "ar", "he", "el", "th", "hi"];
    (0..9)
        .filter(|&i| i != 1)
        .max_by_key(|&i| n[i])
        .filter(|&i| n[i] * 5 > total)
        .map(|i| names[i])
        .unwrap_or("latin")
}

/// ISO 639-1 codes accepted as locale directories (not `go`, `js`, `api`, …).
const LOCALES: [&str; 44] = [
    "ar", "bg", "bn", "ca", "cs", "da", "de", "el", "en", "es", "et", "fa", "fi", "fr", "he", "hi",
    "hr", "hu", "id", "it", "ja", "ko", "lt", "lv", "ms", "nb", "nl", "no", "pl", "pt", "ro", "ru",
    "sk", "sl", "sr", "sv", "sw", "th", "tl", "tr", "uk", "ur", "vi", "zh",
];

/// The base language of a locale directory in a source path (`docs/zh-CN/x` → `zh`,
/// `i18n/zh-Hans/x` → `zh`, `docs/tr/x` → `tr`) directly under docs/, i18n/, locales/ or
/// translations/.
pub fn path_locale(subpath: &str) -> Option<String> {
    let parts: Vec<&str> = subpath.split('/').collect();
    parts.windows(2).find_map(|w| {
        let (dir, code) = (w[0], w[1]);
        let (lang, rest) = code.split_once(['-', '_']).unwrap_or((code, ""));
        let rest_ok = rest.is_empty()
            || (rest.len() == 2 && rest.chars().all(|c| c.is_ascii_uppercase()))
            || (rest.len() == 4 && rest.chars().next().is_some_and(|c| c.is_ascii_uppercase()));
        let ok = matches!(dir, "docs" | "i18n" | "locales" | "translations")
            && LOCALES.contains(&lang)
            && rest_ok;
        // Traditional Chinese is a different written language from Simplified
        let hant = lang == "zh" && matches!(rest, "TW" | "HK" | "MO" | "Hant");
        ok.then(|| {
            if hant {
                "zh-Hant".to_string()
            } else {
                lang.to_string()
            }
        })
    })
}

/// The script a language is written in, for checking a path locale against the text.
fn script_of(lang: &str) -> &'static str {
    match lang {
        "zh" | "zh-Hant" => "zh",
        "ja" => "ja",
        "ko" => "ko",
        "ru" | "uk" | "bg" | "sr" => "ru",
        "ar" | "fa" | "ur" => "ar",
        "he" => "he",
        "el" => "el",
        "th" => "th",
        "hi" => "hi",
        _ => "latin",
    }
}

/// Words (lowercased alphanumerics of any script, ≥ 3 chars, CJK excluded) plus CJK bigrams.
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
        if c.is_alphanumeric() && !cjk {
            word.extend(c.to_lowercase());
        } else {
            if word.chars().count() >= 3 {
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
        invocation: Regex::new(r"(?i)^(disable-model-invocation|user-invocable|allowed-tools|tools|model|context|agent)\s*:").unwrap(),
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

/// Invocation-affecting frontmatter lines, each with its indented/list continuation lines.
fn invocation_lines(fm: &[&str]) -> Vec<String> {
    let p = patterns();
    let mut out = Vec::new();
    let mut i = 0;
    while i < fm.len() {
        if p.invocation.is_match(fm[i]) {
            let mut v = fm[i].trim_end().to_string();
            while i + 1 < fm.len()
                && (fm[i + 1].starts_with([' ', '\t']) || fm[i + 1].starts_with('-'))
            {
                i += 1;
                v.push('\n');
                v.push_str(fm[i].trim_end());
            }
            out.push(v);
        }
        i += 1;
    }
    out
}

/// Body text shorter than this (non-whitespace characters) is a placeholder, not a skill.
const STUB_CHARS: usize = 300;
/// Bodies are tokenized up to this many bytes (cards stay small for huge bodies).
const TOKEN_BYTES: usize = 1 << 20;

/// Build the card for `objects/<digest>`; sources come from `locks/<digest>/*.json`.
pub fn card(store: &Path, digest: &str) -> Result<Card, String> {
    if !crate::scan::is_digest(digest) {
        return Err(format!("not a digest: {digest}"));
    }
    let mpath = store.join("manifests").join(format!("{digest}.json"));
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(&mpath).map_err(|e| format!("{}: {e}", mpath.display()))?)
            .map_err(|e| format!("{}: {e}", mpath.display()))?;
    let raw =
        fs::read(store.join("objects").join(digest).join("SKILL.md")).map_err(|e| e.to_string())?;
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
    let mut files = Sha256::new();
    for f in manifest.files.iter().filter(|f| f.path != "SKILL.md") {
        files.update(format!("{}\0{}\0{}\n", f.path, f.sha256, f.executable));
    }
    let base = Card {
        digest: digest.into(),
        status: String::new(),
        name: None,
        description: None,
        language: "unknown".into(),
        use_when: vec![],
        not_for: vec![],
        invocation: vec![],
        headings: vec![],
        body_sha256: String::new(),
        frontmatter_sha256: String::new(),
        files_sha256: hex(&files.finalize()),
        tokens: vec![],
        mentions: vec![],
        sources,
    };
    // check_skill calls non-UTF-8 "uncheckable": no lossy text ever reaches a card
    let Ok(text) = String::from_utf8(raw) else {
        return Ok(Card {
            status: "invalid_encoding".into(),
            ..base
        });
    };
    let text = text.replace("\r\n", "\n");
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
    let name = field("name").filter(|n| !n.is_empty());
    let description = field("description").filter(|d| !d.is_empty());
    let status = match (&fm, &name, &description) {
        (Err(_), _, _) => "invalid_frontmatter",
        (_, None, _) => "needs_name",
        (_, _, None) => "needs_description",
        _ if body.chars().filter(|c| !c.is_whitespace()).count() < STUB_CHARS => "stub",
        _ => "ok",
    };
    let p = patterns();
    let desc_sentences = description.as_deref().map(sentences).unwrap_or_default();
    let script = language(description.as_deref().unwrap_or(body));
    // a locale directory only counts when the description or the body is actually written in
    // that script (an untranslated copy under docs/ja-JP/ is not Japanese; a copy whose body
    // was translated but whose description was not still is)
    let lang = base
        .sources
        .iter()
        .find_map(|s| path_locale(&s.subpath))
        .filter(|l| script_of(l) == script || script_of(l) == language(body))
        .unwrap_or_else(|| script.to_string());
    let mut cut = body.len().min(TOKEN_BYTES);
    while !body.is_char_boundary(cut) {
        cut -= 1;
    }
    static ID: OnceLock<Regex> = OnceLock::new();
    let id_re = ID.get_or_init(|| Regex::new(r"\b[a-z0-9]+(?:-[a-z0-9]+)+\b").unwrap());
    let mentions: std::collections::BTreeSet<String> = id_re
        .find_iter(&body[..cut])
        .map(|m| m.as_str().to_string())
        .collect();
    Ok(Card {
        status: status.into(),
        language: lang,
        use_when: desc_sentences
            .iter()
            .filter(|s| p.use_when.is_match(s) && !p.not_for.is_match(s))
            .cloned()
            .collect(),
        not_for: desc_sentences
            .iter()
            .filter(|s| p.not_for.is_match(s))
            .cloned()
            .collect(),
        invocation: invocation_lines(&fm_lines),
        headings: body
            .lines()
            .filter_map(|l| l.strip_prefix("## "))
            .map(|h| h.trim().to_string())
            .take(12)
            .collect(),
        body_sha256: hex(&Sha256::digest(body.as_bytes())),
        frontmatter_sha256: hex(&Sha256::digest(fm_lines.join("\n").as_bytes())),
        tokens: tokens(&body[..cut]),
        mentions: mentions.into_iter().collect(),
        name,
        description,
        ..base
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
        let d = "Create carousels from a brief. Use for social slides. Do NOT use for generic images. Not a generic image generator.";
        let s = sentences(d);
        let p = patterns();
        let uw: Vec<_> = s
            .iter()
            .filter(|x| p.use_when.is_match(x) && !p.not_for.is_match(x))
            .collect();
        let nf: Vec<_> = s.iter().filter(|x| p.not_for.is_match(x)).collect();
        assert_eq!(uw, ["Use for social slides"]);
        assert_eq!(
            nf,
            [
                "Do NOT use for generic images",
                "Not a generic image generator"
            ]
        );
        assert_eq!(language("为 1688 制作一个元 skill"), "zh");
        assert_eq!(language("スキルを使用する場合"), "ja");
        assert_eq!(language("Plain English text"), "latin");
        assert_eq!(language("Навык для работы с базой"), "ru");
        assert_eq!(
            tokens("Rust 化 crate, rust! 道法 Навык"),
            vec!["crate", "rust", "навык", "道法"]
        );
        let (fm, body) = split("---\nname: a\n---\n## H\nx\n");
        assert_eq!((fm, body), (vec!["name: a"], "## H\nx\n"));
        assert_eq!(
            path_locale("docs/tr/skills/golang-testing").as_deref(),
            Some("tr")
        );
        assert_eq!(
            path_locale("docs/zh-CN/skills/council").as_deref(),
            Some("zh")
        );
        assert_eq!(path_locale("i18n/zh-Hans/x").as_deref(), Some("zh"));
        assert_eq!(path_locale("docs/zh-TW/x").as_deref(), Some("zh-Hant"));
        assert_eq!(path_locale("docs/go/x"), None);
        assert_eq!(path_locale("skills/docs-writer"), None);
        assert_eq!(
            invocation_lines(&[
                "name: a",
                "allowed-tools:",
                "  - Read",
                "  - Bash",
                "license: MIT"
            ]),
            vec!["allowed-tools:\n  - Read\n  - Bash"]
        );
    }
}
