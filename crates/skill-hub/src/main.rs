//! skill-hub — registry, drift check, lexical search and verified load for agent skills.
//!
//! Role: the hosted-skill layer of the DAO family. Agents call `search` to find a skill
//! whose description is not resident in context, then `get` to load its verified SKILL.md.
//! Depends on: serde/serde_json (registry file), sha2 (content identity); std only otherwise.
//! Called by: the thin `skill-hub` entry skill, CI tests, DAO admission scripts.
//! Invariants:
//! - `get` only serves ids present in the registry, and only when the skill directory still
//!   has the recorded realpath, SKILL.md sha256 and tree sha256. The tree hash covers every
//!   regular file and symlink (by link text, never followed), including bundled executables
//!   and dependencies; only VCS metadata and interpreter/test caches in `SKIP_NAMES` are left out.
//! - Skills the owner restricted to explicit/user invocation (`disable-model-invocation: true`
//!   in SKILL.md, or Codex `allow_implicit_invocation: false`) are never indexed, so the model
//!   cannot reach them through the hub. Policy values are read fail-closed: anything other
//!   than an explicit permissive value counts as restricted.
//! - `check` is red (exit 1) on any added, removed, changed or unreadable skill under the roots;
//!   one broken skill never stops the others from being indexed or served.
//! - The registry holds local absolute paths; it is a generated artifact, never committed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const SCHEMA: u32 = 3;
/// Characters of SKILL.md body kept in the registry for retrieval.
const BODY_CHARS: usize = 1500;
/// Weight of the body field relative to name+description in the ranking score.
const BODY_WEIGHT: f64 = 0.3;
const MAX_FILES: usize = 10_000;
/// Never executed by a skill, but rewritten by tools; hashing them would lock skills out.
const SKIP_NAMES: [&str; 6] = [
    ".git",
    "__pycache__",
    ".DS_Store",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
];

/// Exit codes (documented in skill-hub/SKILL.md).
const EXIT_DRIFT: u8 = 1;
const EXIT_ERROR: u8 = 2;
const EXIT_REFUSED: u8 = 3;
const EXIT_UNKNOWN_ID: u8 = 4;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Root {
    label: String,
    path: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Skill {
    id: String,
    name: String,
    description: String,
    /// Canonical directory of the skill (symlinks resolved).
    dir: String,
    entry_sha256: String,
    /// Hash over the skill tree (see module docs for what is covered).
    tree_sha256: String,
    /// Every `root:folder` location that resolves to this same content.
    locations: Vec<String>,
    /// First `BODY_CHARS` characters of SKILL.md after the frontmatter (retrieval only).
    body: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Registry {
    schema: u32,
    roots: Vec<Root>,
    skills: Vec<Skill>,
}

#[derive(Debug, PartialEq)]
struct Front {
    name: Option<String>,
    description: String,
    user_only: bool,
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Minimal YAML-frontmatter reader for `name`, `description` and
/// `disable-model-invocation` (plain, quoted, multi-line plain, and `|`/`>` block scalars).
fn parse_frontmatter(text: &str) -> Front {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Front {
            name: None,
            description: String::new(),
            user_only: false,
        };
    }
    let body: Vec<&str> = lines.take_while(|l| l.trim_end() != "---").collect();
    let field = |key: &str| -> Option<String> {
        let prefix = format!("{key}:");
        let i = body.iter().position(|l| l.starts_with(&prefix))?;
        let rest = body[i][prefix.len()..].trim();
        let cont: Vec<&str> = body[i + 1..]
            .iter()
            .take_while(|l| l.is_empty() || l.starts_with(' ') || l.starts_with('\t'))
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        if rest.is_empty() || matches!(rest, "|" | ">" | "|-" | ">-" | "|+" | ">+") {
            return Some(cont.join(" "));
        }
        let joined = std::iter::once(rest)
            .chain(cont.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        let unquoted = joined
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| joined.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(&joined);
        Some(unquoted.to_string())
    };
    Front {
        name: field("name"),
        description: field("description").unwrap_or_default(),
        user_only: restricted(body.iter().copied(), "disable-model-invocation", "false"),
    }
}

/// YAML scalar of a policy key: trailing comment and quotes removed, lowercased.
fn policy_value(raw: &str) -> String {
    let v = raw.split(" #").next().unwrap_or("").trim();
    v.trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_ascii_lowercase()
}

/// Fail closed on the key's presence: every line that mentions `key` must be exactly
/// `key: <permissive>` (after removing comments and quotes), otherwise the skill is
/// restricted. Quoted keys, flow mappings or JSON spellings therefore restrict too.
fn restricted<'a>(lines: impl Iterator<Item = &'a str>, key: &str, permissive: &str) -> bool {
    let prefix = format!("{key}:");
    lines
        .filter(|l| l.contains(key))
        .any(|l| match l.trim().strip_prefix(prefix.as_str()) {
            Some(v) => policy_value(v) != permissive,
            None => true,
        })
}

/// Codex keeps its invocation policy in `agents/openai.yaml`.
fn codex_explicit_only(skill_dir: &Path) -> bool {
    fs::read_to_string(skill_dir.join("agents/openai.yaml"))
        .map(|t| restricted(t.lines(), "allow_implicit_invocation", "true"))
        .unwrap_or(false)
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x3040..=0x30FF | 0xAC00..=0xD7AF)
}

/// Lowercased latin/digit words plus CJK character bigrams
/// (a lone CJK character becomes a unigram). Punctuation, hyphens and underscores split words.
fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();
    let flush_cjk = |cjk: &mut Vec<char>, out: &mut Vec<String>| {
        match cjk.len() {
            0 => {}
            1 => out.push(cjk[0].to_string()),
            _ => out.extend(cjk.windows(2).map(|w| w.iter().collect::<String>())),
        }
        cjk.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() && !is_cjk(c) {
            flush_cjk(&mut cjk, &mut out);
            word.extend(c.to_lowercase());
            continue;
        }
        if !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        if is_cjk(c) {
            cjk.push(c);
        } else {
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_cjk(&mut cjk, &mut out);
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// sha256 over the sorted skill tree. Regular files contribute their content hash; a symlink
/// to a regular file contributes its link text and the target's content (file links cannot
/// loop); a directory symlink only its link text (never followed); a dangling link is an
/// error; other file types (FIFOs, sockets) only a marker, so hashing never blocks.
fn tree_hash(dir: &Path) -> Result<String, String> {
    fn walk(base: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), String> {
        if depth > 32 {
            return Err(format!("{}: directory nesting too deep", dir.display()));
        }
        for e in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let e = e.map_err(|e| e.to_string())?;
            let ft = e.file_type().map_err(|e| e.to_string())?;
            let name = e.file_name().to_string_lossy().into_owned();
            if SKIP_NAMES.contains(&name.as_str()) {
                continue;
            }
            if ft.is_dir() {
                walk(base, &e.path(), depth + 1, out)?;
            } else {
                out.push(e.path().strip_prefix(base).unwrap().to_path_buf());
                if out.len() > MAX_FILES {
                    return Err(format!("{}: too many files", base.display()));
                }
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, dir, 0, &mut files)?;
    files.sort();
    let mut h = Sha256::new();
    for rel in files {
        let p = dir.join(&rel);
        let err = |e: std::io::Error| format!("{}: {e}", rel.display());
        let ft = fs::symlink_metadata(&p).map_err(err)?.file_type();
        let id = if ft.is_symlink() {
            let link = fs::read_link(&p).map_err(err)?;
            let target =
                fs::metadata(&p).map_err(|e| format!("{}: dangling link: {e}", rel.display()))?;
            if target.is_file() {
                let content = hex(&Sha256::digest(fs::read(&p).map_err(err)?));
                format!("link:{}:{content}", link.display())
            } else {
                format!("link:{}", link.display())
            }
        } else if ft.is_file() {
            hex(&Sha256::digest(fs::read(&p).map_err(err)?))
        } else {
            "special".to_string()
        };
        h.update(rel.to_string_lossy().as_bytes());
        h.update([0]);
        h.update(id.as_bytes());
        h.update(b"\n");
    }
    Ok(hex(&h.finalize()))
}

/// Result of scanning the roots.
struct Scan {
    skills: Vec<Skill>,
    /// `broken: ...` lines for skills that could not be read (reported by `check`).
    broken: Vec<String>,
    /// Locations left out because their owner restricted them to explicit invocation.
    skipped: Vec<String>,
}

/// A missing root is fatal; a single unreadable skill is not.
fn scan(roots: &[Root]) -> Result<Scan, String> {
    // canonical dir -> skill (merging locations that resolve to the same directory)
    let mut by_dir: BTreeMap<String, Skill> = BTreeMap::new();
    let mut broken = Vec::new();
    let mut skipped = Vec::new();
    for root in roots {
        let mut entries: Vec<PathBuf> = fs::read_dir(&root.path)
            .map_err(|e| format!("cannot read root {}: {e}", root.path))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for p in entries {
            let entry = p.join("SKILL.md");
            let folder = p.file_name().unwrap().to_string_lossy().into_owned();
            if !entry.is_file() || folder.starts_with('.') {
                continue;
            }
            let location = format!("{}:{}", root.label, folder);
            let read = || -> Result<(PathBuf, Vec<u8>), String> {
                let dir = fs::canonicalize(&p).map_err(|e| e.to_string())?;
                Ok((dir, fs::read(&entry).map_err(|e| e.to_string())?))
            };
            let (dir, bytes) = match read() {
                Ok(v) => v,
                Err(e) => {
                    broken.push(format!("broken: {location}: {e}"));
                    continue;
                }
            };
            let front = parse_frontmatter(&String::from_utf8_lossy(&bytes));
            if front.user_only || codex_explicit_only(&dir) {
                skipped.push(location);
                continue;
            }
            let key = dir.to_string_lossy().into_owned();
            if let Some(s) = by_dir.get_mut(&key) {
                s.locations.push(location);
                continue;
            }
            let tree_sha256 = match tree_hash(&dir) {
                Ok(h) => h,
                Err(e) => {
                    broken.push(format!("broken: {location}: {e}"));
                    continue;
                }
            };
            by_dir.insert(
                key.clone(),
                Skill {
                    id: String::new(),
                    name: front.name.unwrap_or_else(|| folder.clone()),
                    description: front.description,
                    tree_sha256,
                    dir: key,
                    entry_sha256: hex(&Sha256::digest(&bytes)),
                    locations: vec![location],
                    body: body_excerpt(&String::from_utf8_lossy(&bytes)),
                },
            );
        }
    }
    // Identical content under different directories (e.g. a plugin copy) is one skill.
    let mut merged: Vec<Skill> = Vec::new();
    for s in by_dir.into_values() {
        if let Some(m) = merged
            .iter_mut()
            .find(|m| m.name == s.name && m.tree_sha256 == s.tree_sha256)
        {
            m.locations.extend(s.locations);
        } else {
            merged.push(s);
        }
    }
    // Ids: plain name when unique, otherwise `root:folder` of the first location.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for s in &merged {
        *counts.entry(s.name.clone()).or_default() += 1;
    }
    for s in &mut merged {
        s.locations.sort();
        s.id = if counts[&s.name] == 1 {
            s.name.clone()
        } else {
            s.locations[0].clone()
        };
    }
    merged.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Scan {
        skills: merged,
        broken,
        skipped,
    })
}

/// Absolute, unique-label roots; relative paths would break `check` from another cwd.
fn normalize_roots(roots: Vec<Root>) -> Result<Vec<Root>, String> {
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .map(|r| {
            if !seen.insert(r.label.clone()) {
                return Err(format!("duplicate root label: {}", r.label));
            }
            let path = fs::canonicalize(&r.path).map_err(|e| format!("root {}: {e}", r.path))?;
            Ok(Root {
                label: r.label,
                path: path.to_string_lossy().into_owned(),
            })
        })
        .collect()
}

fn load(path: &str) -> Result<Registry, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("cannot read registry {path}: {e}"))?;
    // Check the version before the full decode so an older registry gets the migration
    // hint instead of a "missing field" error for fields that version never had.
    let raw: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("bad registry {path}: {e}"))?;
    let schema = raw.get("schema").and_then(serde_json::Value::as_u64);
    if schema != Some(u64::from(SCHEMA)) {
        return Err(format!(
            "registry schema {} != {SCHEMA}; run index",
            schema.map_or("missing".to_string(), |s| s.to_string())
        ));
    }
    serde_json::from_value(raw).map_err(|e| format!("bad registry {path}: {e}"))
}

/// Differences between the registry and the disk; empty means in sync.
fn drift(reg: &Registry) -> Result<Vec<String>, String> {
    let Scan {
        skills: now,
        broken,
        ..
    } = scan(&reg.roots)?;
    let old: HashMap<&str, &Skill> = reg.skills.iter().map(|s| (s.id.as_str(), s)).collect();
    let new: HashMap<&str, &Skill> = now.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut out = Vec::new();
    for (id, s) in &new {
        match old.get(id) {
            None => out.push(format!("added: {id}")),
            Some(o) if *o != *s => out.push(format!("changed: {id}")),
            _ => {}
        }
    }
    for id in old.keys().filter(|id| !new.contains_key(*id)) {
        out.push(format!("removed: {id}"));
    }
    out.extend(broken);
    out.sort();
    Ok(out)
}

/// SKILL.md text after the frontmatter, truncated to `BODY_CHARS` characters.
fn body_excerpt(text: &str) -> String {
    let text = text.trim_start_matches('\u{feff}');
    let body = text
        .strip_prefix("---")
        .and_then(|rest| rest.find("\n---").map(|i| &rest[i + 4..]))
        .unwrap_or(text);
    body.chars().take(BODY_CHARS).collect()
}

/// Query-side synonyms so "TypeScript service" reaches `*-ts` skills and so on.
const SYNONYMS: [(&str, &[&str]); 8] = [
    ("typescript", &["ts"]),
    ("javascript", &["js"]),
    ("nodejs", &["ts", "js"]),
    ("python", &["py"]),
    ("csharp", &["dotnet"]),
    ("golang", &["go"]),
    ("kotlin", &["kt"]),
    ("postgresql", &["postgres"]),
];

/// Name suffixes that mark per-language SDK variants (`azure-foo-ts`, `azure-foo-py`, ...).
const LANG_SUFFIXES: [&str; 8] = ["ts", "js", "py", "java", "dotnet", "rust", "go", "kt"];

/// Unambiguous ways a query names a language, mapped to the variant suffix. Short forms
/// such as "go", "net", "ts" or "py" are ordinary words or too ambiguous to count.
const LANGUAGE_NAMES: [(&str, &str); 10] = [
    ("typescript", "ts"),
    ("javascript", "js"),
    ("python", "py"),
    ("java", "java"),
    ("csharp", "dotnet"),
    ("dotnet", "dotnet"),
    ("rust", "rust"),
    ("golang", "go"),
    ("kotlin", "kt"),
    ("nodejs", "js"),
];

/// `needle` occurs with no letter or digit right before or after it (".NET", not "example.net").
fn standalone(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + needle.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// Languages the query names explicitly (including the spellings ".NET" and "C#").
fn named_languages(query: &str) -> Vec<&'static str> {
    let lower = query.to_lowercase();
    let tokens = tokenize(query);
    let mut langs: Vec<&str> = LANGUAGE_NAMES
        .iter()
        .filter(|(n, _)| tokens.iter().any(|t| t == n))
        .map(|(_, s)| *s)
        .collect();
    if standalone(&lower, ".net") || lower.contains("c#") {
        langs.push("dotnet");
    }
    langs.sort();
    langs.dedup();
    langs
}

/// Query terms with weights: original tokens 1.0; synonyms and joined neighbour words
/// ("app configuration" -> "appconfiguration", matching compound skill names) 0.5.
fn expand_query(query: &str) -> Vec<(String, f64)> {
    let base = tokenize(query);
    let mut q: Vec<(String, f64)> = base.iter().map(|t| (t.clone(), 1.0)).collect();
    if named_languages(query).contains(&"dotnet") {
        q.push(("dotnet".to_string(), 0.5));
    }
    for t in &base {
        for (_, v) in SYNONYMS.iter().filter(|(k, _)| k == t) {
            q.extend(v.iter().map(|s| (s.to_string(), 0.5)));
        }
    }
    let latin = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric());
    for w in base.windows(2).filter(|w| latin(&w[0]) && latin(&w[1])) {
        q.push((format!("{}{}", w[0], w[1]), 0.5));
    }
    // keep the highest weight per term
    q.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.total_cmp(&a.1)));
    q.dedup_by(|a, b| a.0 == b.0);
    q
}

/// Okapi BM25 of `q` against every document of one field.
fn bm25(docs: &[Vec<String>], q: &[(String, f64)]) -> Vec<f64> {
    let (k1, b) = (1.2, 0.75);
    let n = docs.len() as f64;
    let avg = (docs.iter().map(Vec::len).sum::<usize>() as f64 / n).max(1.0);
    let mut df: HashMap<&str, f64> = HashMap::new();
    for d in docs {
        let present: HashSet<&str> = d.iter().map(String::as_str).collect();
        for (t, _) in q.iter().filter(|(t, _)| present.contains(t.as_str())) {
            *df.entry(t.as_str()).or_default() += 1.0;
        }
    }
    docs.iter()
        .map(|d| {
            let len = d.len() as f64;
            q.iter()
                .filter_map(|(t, w)| {
                    let tf = d.iter().filter(|x| *x == t).count() as f64;
                    let df = *df.get(t.as_str())?;
                    let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                    Some(w * idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * len / avg)))
                })
                .sum()
        })
        .collect()
}

fn search<'a>(reg: &'a Registry, query: &str, top_k: usize) -> Vec<(f64, &'a Skill)> {
    // ponytail: in-memory BM25 over name x3 + description, plus a weighted body field;
    // fine for thousands of skills, add an on-disk inverted index beyond ~50k.
    if reg.skills.is_empty() {
        return Vec::new();
    }
    let head: Vec<Vec<String>> = reg
        .skills
        .iter()
        .map(|s| {
            let name = tokenize(&s.name);
            let mut d: Vec<String> = name.iter().chain(&name).chain(&name).cloned().collect();
            d.extend(tokenize(&s.description));
            d
        })
        .collect();
    let body: Vec<Vec<String>> = reg.skills.iter().map(|s| tokenize(&s.body)).collect();
    let q = expand_query(query);
    let (h, bd) = (bm25(&head, &q), bm25(&body, &q));
    let mut scored: Vec<(f64, &Skill)> = h
        .iter()
        .zip(&bd)
        .zip(&reg.skills)
        .map(|((h, bd), s)| (h + BODY_WEIGHT * bd, s))
        .filter(|(score, _)| *score > 0.0)
        .collect();
    // A query naming one language demotes SDK variants for a different language.
    let langs = named_languages(query);
    if !langs.is_empty() {
        for (score, s) in scored.iter_mut() {
            let suffix = s.name.rsplit('-').next().unwrap_or("");
            if LANG_SUFFIXES.contains(&suffix) && !langs.contains(&suffix) {
                *score *= 0.5;
            }
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    scored.truncate(top_k);
    scored
}

/// Verified load: the id must be registered and the skill directory must still match.
/// Errors carry the exit code to use.
fn get(reg: &Registry, id: &str) -> Result<(PathBuf, String), (u8, String)> {
    let s = reg
        .skills
        .iter()
        .find(|s| s.id == id)
        .ok_or((EXIT_UNKNOWN_ID, format!("unknown skill id: {id}")))?;
    let refused = |m: String| (EXIT_REFUSED, format!("{id}: {m}"));
    let dir = fs::canonicalize(&s.dir).map_err(|e| refused(e.to_string()))?;
    if dir != Path::new(&s.dir) {
        return Err(refused("directory moved; run index".into()));
    }
    let entry = dir.join("SKILL.md");
    let bytes = fs::read(&entry).map_err(|e| refused(e.to_string()))?;
    if hex(&Sha256::digest(&bytes)) != s.entry_sha256
        || tree_hash(&dir).map_err(refused)? != s.tree_sha256
    {
        return Err(refused(
            "skill files changed since indexing; run check/index".into(),
        ));
    }
    Ok((dir, String::from_utf8_lossy(&bytes).into_owned()))
}

/// Replace the registry atomically so a concurrent `get` never reads a partial file.
fn write_registry(out: &str, reg: &Registry) -> Result<(), String> {
    let tmp = format!("{out}.tmp-{}", std::process::id());
    fs::write(&tmp, serde_json::to_string_pretty(reg).unwrap() + "\n")
        .map_err(|e| format!("{tmp}: {e}"))?;
    fs::rename(&tmp, out).map_err(|e| format!("{out}: {e}"))
}

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  skill-hub index --root LABEL=DIR [--root ...] --out REGISTRY.json\n  \
         skill-hub check  [--registry REGISTRY.json]\n  \
         skill-hub search [--registry REGISTRY.json] [--top-k N] [--] QUERY...\n  \
         skill-hub get    [--registry REGISTRY.json] ID\n\
         check/search/get default to $SKILL_HUB_REGISTRY.\n\
         exit: 0 ok, 1 drift, 2 error/usage, 3 get refused (changed/moved), 4 unknown id"
    );
    ExitCode::from(EXIT_ERROR)
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let Some(cmd) = args.first() else {
        return Ok(usage());
    };
    let mut roots = Vec::new();
    let (mut out, mut registry, mut top_k, mut rest) = (None, None, 3usize, Vec::new());
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        if a == "--" {
            rest.extend(it.by_ref().cloned());
            break;
        }
        let allowed: &[&str] = match cmd.as_str() {
            "index" => &["--root", "--out"],
            "search" => &["--registry", "--top-k"],
            _ => &["--registry"],
        };
        if a.starts_with("--") && !allowed.contains(&a.as_str()) {
            return Err(format!("option {a} is not valid for {cmd}"));
        }
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--root" => {
                let v = val()?;
                let (label, path) = v.split_once('=').ok_or("--root expects LABEL=DIR")?;
                roots.push(Root {
                    label: label.into(),
                    path: path.into(),
                });
            }
            "--out" => out = Some(val()?),
            "--registry" => registry = Some(val()?),
            "--top-k" => top_k = val()?.parse().map_err(|_| "--top-k expects a number")?,
            _ => rest.push(a.clone()),
        }
    }
    // Stray arguments would silently drop intent (e.g. a mistyped `--root`), so reject them.
    match (cmd.as_str(), rest.len()) {
        ("index" | "check", 0) | ("get", 1) | ("search", _) => {}
        ("index" | "check" | "get", n) if n > 1 || cmd != "get" => {
            return Err(format!(
                "unexpected arguments for {cmd}: {}",
                rest.join(" ")
            ))
        }
        _ => return Ok(usage()),
    }
    let registry = || -> Result<Registry, String> {
        let path = registry
            .clone()
            .or_else(|| std::env::var("SKILL_HUB_REGISTRY").ok())
            .ok_or(format!("{cmd} needs --registry or SKILL_HUB_REGISTRY"))?;
        load(&path)
    };
    match cmd.as_str() {
        "index" => {
            let out = out.ok_or("index needs --out")?;
            if roots.is_empty() {
                return Err("index needs at least one --root".into());
            }
            let roots = normalize_roots(roots)?;
            let Scan {
                skills,
                broken,
                skipped,
            } = scan(&roots)?;
            broken.iter().for_each(|b| eprintln!("warning: {b}"));
            skipped
                .iter()
                .for_each(|s| eprintln!("skipped (explicit-only): {s}"));
            let n = skills.len();
            write_registry(
                &out,
                &Registry {
                    schema: SCHEMA,
                    roots,
                    skills,
                },
            )?;
            println!("indexed {n} skills -> {out}");
            Ok(ExitCode::SUCCESS)
        }
        "check" => {
            let reg = registry()?;
            let d = drift(&reg)?;
            if d.is_empty() {
                println!("in sync: {} skills", reg.skills.len());
                return Ok(ExitCode::SUCCESS);
            }
            d.iter().for_each(|l| println!("{l}"));
            Ok(ExitCode::from(EXIT_DRIFT))
        }
        "search" => {
            let reg = registry()?;
            let hits: Vec<_> = search(&reg, &rest.join(" "), top_k)
                .into_iter()
                .map(|(score, s)| {
                    serde_json::json!({"id": s.id, "score": (score * 1000.0).round() / 1000.0, "description": s.description})
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&hits).unwrap());
            Ok(ExitCode::SUCCESS)
        }
        "get" => {
            let reg = registry()?;
            let id = rest.first().ok_or("get needs an ID")?;
            match get(&reg, id) {
                Ok((path, text)) => {
                    println!("<!-- skill-hub dir: {} -->\n{text}", path.display());
                    Ok(ExitCode::SUCCESS)
                }
                Err((code, e)) => {
                    eprintln!("refused: {e}");
                    Ok(ExitCode::from(code))
                }
            }
        }
        _ => Ok(usage()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    run(&args).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        ExitCode::from(EXIT_ERROR)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("skill-hub-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn put_raw(root: &Path, folder: &str, text: &str) {
        fs::create_dir_all(root.join(folder)).unwrap();
        fs::write(root.join(folder).join("SKILL.md"), text).unwrap();
    }

    fn put(root: &Path, folder: &str, name: &str, desc: &str) {
        put_raw(
            root,
            folder,
            &format!("---\nname: {name}\ndescription: {desc}\n---\n# {name}\n"),
        );
    }

    fn reg_for(root: &Path) -> Registry {
        let roots = normalize_roots(vec![Root {
            label: "t".into(),
            path: root.to_string_lossy().into(),
        }])
        .unwrap();
        Registry {
            schema: SCHEMA,
            skills: scan(&roots).unwrap().skills,
            roots,
        }
    }

    #[test]
    fn frontmatter_variants() {
        let f = parse_frontmatter("---\nname: a\ndescription: \"quoted: x\"\n---\n");
        assert_eq!(
            (f.name.as_deref(), f.description.as_str()),
            (Some("a"), "quoted: x")
        );
        let f = parse_frontmatter(
            "---\nname: b\ndescription: |\n  line one\n  line two\nmetadata:\n  v: 1\n---\n",
        );
        assert_eq!(f.description, "line one line two");
        let f = parse_frontmatter(
            "\u{feff}---\r\nname: c\r\ndescription: plain\r\n  continued\r\n---\r\n",
        );
        assert_eq!(
            (f.name.as_deref(), f.description.as_str()),
            (Some("c"), "plain continued")
        );
        let f = parse_frontmatter("---\nname: d\ndescription: \"multi\n  line\"\n---\n");
        assert_eq!(f.description, "multi line");
        assert!(parse_frontmatter("---\nname: e\ndisable-model-invocation: true\n---\n").user_only);
        assert_eq!(parse_frontmatter("no frontmatter").name, None);
    }

    #[test]
    fn tokenize_latin_and_cjk() {
        assert_eq!(
            tokenize("SEO-operator, a GEO"),
            vec!["seo", "operator", "a", "geo"]
        );
        assert_eq!(tokenize("小红书图文"), vec!["小红", "红书", "书图", "图文"]);
        assert_eq!(tokenize("用1688运营"), vec!["用", "1688", "运营"]);
    }

    #[test]
    fn search_ranks_relevant_first_and_drops_zero() {
        let root = tmp("search");
        put(
            &root,
            "seo-operator",
            "seo-operator",
            "Audit organic website search and keywords",
        );
        put(
            &root,
            "xhs",
            "xiaohongshu-content",
            "整理小红书图文笔记 package for Xiaohongshu",
        );
        put(&root, "x", "x-operator", "Prepare posts and replies");
        put(&root, "pdf", "pdf", "Read and merge PDF files");
        let reg = reg_for(&root);
        let hits = search(&reg, "小红书 图文", 3);
        assert_eq!(hits[0].1.name, "xiaohongshu-content");
        assert!(hits.iter().all(|(s, _)| *s > 0.0));
        assert_eq!(
            search(&reg, "keywords website", 1)[0].1.name,
            "seo-operator"
        );
        assert_eq!(search(&reg, "X posts", 1)[0].1.name, "x-operator");
        assert!(search(&reg, "zzzz", 3).is_empty());
    }

    #[test]
    fn language_synonyms_and_body_reach_variants() {
        let root = tmp("synonyms");
        put(&root, "ts", "azure-blob-ts", "Azure Blob Storage SDK");
        put(&root, "py", "azure-blob-py", "Azure Blob Storage SDK");
        put_raw(&root, "b", "---\nname: deploy-helper\ndescription: Ship things\n---\nUses the flyctl CLI for Fly.io apps.\n");
        let reg = reg_for(&root);
        assert_eq!(
            search(&reg, "TypeScript upload to Azure Blob Storage", 1)[0]
                .1
                .name,
            "azure-blob-ts"
        );
        assert_eq!(
            search(&reg, "Python Azure Blob Storage", 1)[0].1.name,
            "azure-blob-py"
        );
        assert_eq!(search(&reg, "flyctl", 1)[0].1.name, "deploy-helper");
        // joined words reach compound names; the named language wins over other variants
        put(
            &root,
            "ac-ts",
            "azure-appconfiguration-ts",
            "Feature flags and dynamic refresh",
        );
        put(
            &root,
            "ac-py",
            "azure-appconfiguration-py",
            "Feature flags and dynamic refresh for Python apps",
        );
        let reg = reg_for(&root);
        assert_eq!(
            search(
                &reg,
                "TypeScript service reading Azure App Configuration",
                1
            )[0]
            .1
            .name,
            "azure-appconfiguration-ts"
        );
    }

    #[test]
    fn only_unambiguous_language_names_constrain_variants() {
        assert_eq!(named_languages("go upload this file"), Vec::<&str>::new());
        assert_eq!(named_languages("net revenue report"), Vec::<&str>::new());
        assert_eq!(named_languages("our .NET service"), vec!["dotnet"]);
        assert_eq!(
            named_languages("fetch data from example.net"),
            Vec::<&str>::new()
        );
        assert_eq!(
            named_languages("a C# client and Python worker"),
            vec!["dotnet", "py"]
        );
        let root = tmp("lang-words");
        put(&root, "a", "upload-ts", "Upload files to storage");
        put(&root, "b", "upload-py", "Upload files to storage quickly");
        let reg = reg_for(&root);
        // "go" is not a language here, so the better-matching -py variant is not halved
        assert_eq!(
            search(&reg, "go upload files to storage quickly", 1)[0]
                .1
                .name,
            "upload-py"
        );
    }

    #[test]
    fn older_registry_gets_migration_hint() {
        let dir = tmp("schema2");
        let p = dir.join("r.json");
        fs::write(
            &p,
            r#"{"schema":2,"roots":[],"skills":[{"id":"x","name":"x"}]}"#,
        )
        .unwrap();
        let err = load(p.to_str().unwrap()).unwrap_err();
        assert!(err.contains("schema 2 != 3; run index"), "{err}");
    }

    #[test]
    fn explicit_only_skills_are_never_indexed() {
        let root = tmp("explicit");
        put_raw(
            &root,
            "deploy",
            "---\nname: deploy-prod\ndescription: deploy production\ndisable-model-invocation: true\n---\n",
        );
        put(&root, "dao", "dao", "deploy helper");
        fs::create_dir_all(root.join("dao/agents")).unwrap();
        fs::write(
            root.join("dao/agents/openai.yaml"),
            "policy:\n  allow_implicit_invocation: false\n",
        )
        .unwrap();
        put(&root, "ok", "ok", "deploy preview");
        let reg = reg_for(&root);
        let ids: Vec<&str> = reg.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["ok"]);
        assert_eq!(get(&reg, "deploy-prod").unwrap_err().0, EXIT_UNKNOWN_ID);
    }

    #[test]
    fn policy_values_fail_closed() {
        for v in ["true", "True", "\"true\"", "true # user only", "yes", ""] {
            let fm = format!("---\nname: a\ndisable-model-invocation: {v}\n---\n");
            assert!(parse_frontmatter(&fm).user_only, "{v:?} must restrict");
        }
        assert!(
            !parse_frontmatter("---\nname: a\ndisable-model-invocation: false\n---\n").user_only
        );
        let twice =
            "---\nname: a\ndisable-model-invocation: false\ndisable-model-invocation: true\n---\n";
        assert!(parse_frontmatter(twice).user_only);
        for v in [
            "False",
            "FALSE",
            "\"false\"",
            "'false'",
            "false   # explicit",
        ] {
            let y = format!("policy:\n  allow_implicit_invocation: {v}\n");
            assert!(
                restricted(y.lines(), "allow_implicit_invocation", "true"),
                "{v:?}"
            );
        }
        assert!(!restricted(
            "  allow_implicit_invocation: true".lines(),
            "allow_implicit_invocation",
            "true"
        ));
        for k in [
            "\"disable-model-invocation\": true",
            "'disable-model-invocation': true",
            "disable-model-invocation : true",
        ] {
            assert!(
                parse_frontmatter(&format!("---\nname: a\n{k}\n---\n")).user_only,
                "{k}"
            );
        }
        for y in [
            "policy: {allow_implicit_invocation: false}",
            "\"allow_implicit_invocation\": false",
            "{\"policy\": {\"allow_implicit_invocation\": false}}",
            "  allow_implicit_invocation : false",
        ] {
            assert!(
                restricted(y.lines(), "allow_implicit_invocation", "true"),
                "{y}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn hostile_trees_neither_hang_nor_block_other_skills() {
        let root = tmp("hostile");
        put(&root, "good", "good", "fine");
        put(&root, "loop", "loop", "has a symlink loop and a fifo");
        std::os::unix::fs::symlink("..", root.join("loop/up")).unwrap();
        let fifo = root.join("loop/pipe");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let reg = reg_for(&root);
        assert_eq!(reg.skills.len(), 2);
        assert!(get(&reg, "loop").is_ok());
        // a cache directory written by the skill does not lock it out
        fs::create_dir_all(root.join("good/.pytest_cache")).unwrap();
        fs::write(root.join("good/.pytest_cache/x"), "cache").unwrap();
        assert!(get(&reg, "good").is_ok());
        assert!(drift(&reg).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_file_targets_are_verified() {
        let root = tmp("symlink-target");
        fs::write(root.join("shared.md"), "v1").unwrap();
        put(&root, "one", "one", "uses a shared contract");
        std::os::unix::fs::symlink("../shared.md", root.join("one/contract.md")).unwrap();
        let reg = reg_for(&root);
        fs::write(root.join("shared.md"), "v2").unwrap();
        assert_eq!(drift(&reg).unwrap(), vec!["changed: one"]);
        assert_eq!(get(&reg, "one").unwrap_err().0, EXIT_REFUSED);
        fs::remove_file(root.join("shared.md")).unwrap();
        assert!(drift(&reg).unwrap().iter().any(|l| l.contains("dangling")));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_skill_is_reported_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp("unreadable");
        put(&root, "good", "good", "fine");
        put(&root, "bad", "bad", "locked");
        fs::create_dir_all(root.join("bad/refs")).unwrap();
        let reg = reg_for(&root);
        fs::set_permissions(root.join("bad/refs"), fs::Permissions::from_mode(0o000)).unwrap();
        let d = drift(&reg).unwrap();
        fs::set_permissions(root.join("bad/refs"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(d.iter().any(|l| l.starts_with("broken: t:bad")), "{d:?}");
        assert!(d.iter().all(|l| !l.contains("good")));
    }

    #[test]
    fn duplicate_content_merges_and_conflicts_get_root_ids() {
        let a = tmp("dup-a");
        let b = tmp("dup-b");
        put(&a, "p", "ponytail", "same");
        put(&b, "p", "ponytail", "same");
        put(&a, "q", "review", "one");
        put(&b, "q", "review", "two");
        let roots = normalize_roots(vec![
            Root {
                label: "a".into(),
                path: a.to_string_lossy().into(),
            },
            Root {
                label: "b".into(),
                path: b.to_string_lossy().into(),
            },
        ])
        .unwrap();
        let skills = scan(&roots).unwrap().skills;
        let ids: Vec<&str> = skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["a:q", "b:q", "ponytail"]);
        assert_eq!(skills[2].locations, vec!["a:p", "b:p"]);
    }

    #[test]
    fn roots_must_have_unique_labels_and_become_absolute() {
        let a = tmp("label");
        let dup = vec![
            Root {
                label: "c".into(),
                path: a.to_string_lossy().into(),
            },
            Root {
                label: "c".into(),
                path: a.to_string_lossy().into(),
            },
        ];
        assert!(normalize_roots(dup).unwrap_err().contains("duplicate"));
        let r = normalize_roots(vec![Root {
            label: "t".into(),
            path: ".".into(),
        }])
        .unwrap();
        assert!(Path::new(&r[0].path).is_absolute());
    }

    #[test]
    fn check_detects_added_changed_removed_including_references() {
        let root = tmp("drift");
        put(&root, "one", "one", "first");
        let reg = reg_for(&root);
        assert!(drift(&reg).unwrap().is_empty());
        put(&root, "two", "two", "second");
        put(&root, "one", "one", "edited");
        assert_eq!(drift(&reg).unwrap(), vec!["added: two", "changed: one"]);
        fs::remove_dir_all(root.join("one")).unwrap();
        fs::remove_dir_all(root.join("two")).unwrap();
        assert_eq!(drift(&reg).unwrap(), vec!["removed: one"]);

        let root = tmp("drift-ref");
        put(&root, "one", "one", "first");
        fs::create_dir_all(root.join("one/references")).unwrap();
        fs::write(root.join("one/references/a.md"), "v1").unwrap();
        let reg = reg_for(&root);
        fs::write(root.join("one/references/a.md"), "v2").unwrap();
        assert_eq!(drift(&reg).unwrap(), vec!["changed: one"]);
    }

    #[test]
    fn stray_arguments_are_rejected() {
        let a = |v: &[&str]| run(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(a(&["index", "--root", "a=.", "--rot", "b=.", "--out", "x"])
            .unwrap_err()
            .contains("option --rot is not valid for index"));
        assert!(a(&["check", "--root", "prod=/new"])
            .unwrap_err()
            .contains("not valid for check"));
        assert!(a(&["index", "--registry", "r", "--root", "a=.", "--out", "x"]).is_err());
        assert!(a(&["get", "--top-k", "3", "one"]).is_err());
        assert!(a(&["index", "--root", "a=.", "extra", "--out", "x"])
            .unwrap_err()
            .contains("unexpected arguments"));
        assert!(a(&["get", "one", "two"])
            .unwrap_err()
            .contains("unexpected"));
    }

    #[test]
    fn get_refuses_unknown_and_tampered() {
        let root = tmp("get");
        put(&root, "one", "one", "first");
        fs::create_dir_all(root.join("one/scripts")).unwrap();
        fs::write(root.join("one/scripts/run.sh"), "echo ok").unwrap();
        fs::create_dir_all(root.join("one/node_modules/helper")).unwrap();
        fs::write(root.join("one/node_modules/helper/index.js"), "ok()").unwrap();
        let reg = reg_for(&root);
        let (dir, text) = get(&reg, "one").unwrap();
        assert!(text.contains("# one") && dir.join("SKILL.md").is_file());
        fs::write(root.join("one/node_modules/helper/index.js"), "evil()").unwrap();
        assert_eq!(get(&reg, "one").unwrap_err().0, EXIT_REFUSED);
        fs::write(root.join("one/node_modules/helper/index.js"), "ok()").unwrap();
        assert_eq!(get(&reg, "../etc").unwrap_err().0, EXIT_UNKNOWN_ID);
        fs::write(root.join("one/scripts/run.sh"), "rm -rf /").unwrap();
        assert_eq!(get(&reg, "one").unwrap_err().0, EXIT_REFUSED);
        put(&root, "one", "one", "tampered");
        assert!(get(&reg, "one").unwrap_err().1.contains("changed"));
    }
}
