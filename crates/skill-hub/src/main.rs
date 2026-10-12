//! skill-hub — registry, drift check, lexical search and verified load for agent skills.
//!
//! Role: the hosted-skill layer of the DAO family. Agents call `search` to find a skill
//! whose description is not resident in context, then `get` to load its verified SKILL.md.
//! Depends on: serde/serde_json (registry file), sha2 (content identity); std only otherwise.
//! Called by: the thin `skill-hub` entry skill, CI tests, DAO admission scripts.
//! Invariants:
//! - `get` only serves ids present in the registry, and only when the skill directory still
//!   has the recorded realpath, SKILL.md sha256 and whole-tree sha256.
//! - Skills the owner restricted to explicit/user invocation (`disable-model-invocation: true`
//!   in SKILL.md, or Codex `allow_implicit_invocation: false`) are never indexed, so the model
//!   cannot reach them through the hub.
//! - `check` is red (exit 1) on any added, removed or changed skill file under the roots.
//! - The registry holds local absolute paths; it is a generated artifact, never committed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const SCHEMA: u32 = 2;
const MAX_FILES: usize = 10_000;
const SKIP_NAMES: [&str; 5] = [".git", "__pycache__", ".DS_Store", "target", "node_modules"];

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
    /// Hash over every file in the skill directory (relative path + bytes).
    tree_sha256: String,
    /// Every `root:folder` location that resolves to this same content.
    locations: Vec<String>,
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
        user_only: field("disable-model-invocation").as_deref() == Some("true"),
    }
}

/// Codex keeps its invocation policy in `agents/openai.yaml`.
fn codex_explicit_only(skill_dir: &Path) -> bool {
    fs::read_to_string(skill_dir.join("agents/openai.yaml"))
        .map(|t| {
            t.lines()
                .any(|l| l.trim() == "allow_implicit_invocation: false")
        })
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

/// sha256 over sorted (relative path, file bytes) of the whole skill directory.
fn tree_hash(dir: &Path) -> Result<String, String> {
    fn walk(base: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), String> {
        if depth > 32 {
            return Err(format!("{}: directory nesting too deep", dir.display()));
        }
        for e in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let p = e.map_err(|e| e.to_string())?.path();
            let name = p.file_name().unwrap().to_string_lossy();
            if SKIP_NAMES.contains(&name.as_ref()) {
                continue;
            }
            if p.is_dir() {
                walk(base, &p, depth + 1, out)?;
            } else {
                out.push(p.strip_prefix(base).unwrap().to_path_buf());
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
        let bytes = fs::read(dir.join(&rel)).map_err(|e| format!("{}: {e}", rel.display()))?;
        h.update(rel.to_string_lossy().as_bytes());
        h.update([0]);
        h.update(hex(&Sha256::digest(&bytes)).as_bytes());
        h.update(b"\n");
    }
    Ok(hex(&h.finalize()))
}

fn scan(roots: &[Root]) -> Result<Vec<Skill>, String> {
    // canonical dir -> skill (merging locations that resolve to the same directory)
    let mut by_dir: BTreeMap<String, Skill> = BTreeMap::new();
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
            let dir = fs::canonicalize(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            let bytes = fs::read(&entry).map_err(|e| format!("{}: {e}", entry.display()))?;
            let front = parse_frontmatter(&String::from_utf8_lossy(&bytes));
            if front.user_only || codex_explicit_only(&dir) {
                continue;
            }
            let location = format!("{}:{}", root.label, folder);
            let key = dir.to_string_lossy().into_owned();
            if let Some(s) = by_dir.get_mut(&key) {
                s.locations.push(location);
                continue;
            }
            by_dir.insert(
                key.clone(),
                Skill {
                    id: String::new(),
                    name: front.name.unwrap_or_else(|| folder.clone()),
                    description: front.description,
                    tree_sha256: tree_hash(&dir)?,
                    dir: key,
                    entry_sha256: hex(&Sha256::digest(&bytes)),
                    locations: vec![location],
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
    Ok(merged)
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
    let reg: Registry =
        serde_json::from_str(&text).map_err(|e| format!("bad registry {path}: {e}"))?;
    if reg.schema != SCHEMA {
        return Err(format!(
            "registry schema {} != {SCHEMA}; run index",
            reg.schema
        ));
    }
    Ok(reg)
}

/// Differences between the registry and the disk; empty means in sync.
fn drift(reg: &Registry) -> Result<Vec<String>, String> {
    let now = scan(&reg.roots)?;
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
    out.sort();
    Ok(out)
}

fn search<'a>(reg: &'a Registry, query: &str, top_k: usize) -> Vec<(f64, &'a Skill)> {
    // ponytail: in-memory BM25 over name x3 + description; fine for hundreds of skills,
    // add an on-disk inverted index only if registries reach tens of thousands.
    let docs: Vec<Vec<String>> = reg
        .skills
        .iter()
        .map(|s| {
            let name = tokenize(&s.name);
            let mut d = Vec::new();
            for _ in 0..3 {
                d.extend(name.iter().cloned());
            }
            d.extend(tokenize(&s.description));
            d
        })
        .collect();
    let n = docs.len() as f64;
    if n == 0.0 {
        return Vec::new();
    }
    let avg = docs.iter().map(Vec::len).sum::<usize>() as f64 / n;
    let mut q = tokenize(query);
    q.sort();
    q.dedup();
    let (k1, b) = (1.2, 0.75);
    let mut scored: Vec<(f64, &Skill)> = docs
        .iter()
        .zip(&reg.skills)
        .map(|(d, s)| {
            let len = d.len() as f64;
            let score = q
                .iter()
                .map(|t| {
                    let tf = d.iter().filter(|x| *x == t).count() as f64;
                    if tf == 0.0 {
                        return 0.0;
                    }
                    let df = docs.iter().filter(|d| d.contains(t)).count() as f64;
                    let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                    idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * len / avg))
                })
                .sum::<f64>();
            (score, s)
        })
        .filter(|(score, _)| *score > 0.0)
        .collect();
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
    Ok((entry, String::from_utf8_lossy(&bytes).into_owned()))
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
         skill-hub search [--registry REGISTRY.json] [--top-k N] QUERY...\n  \
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
            let skills = scan(&roots)?;
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
                    println!("<!-- skill-hub: {} -->\n{text}", path.display());
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
            skills: scan(&roots).unwrap(),
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
        let skills = scan(&roots).unwrap();
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
    fn get_refuses_unknown_and_tampered() {
        let root = tmp("get");
        put(&root, "one", "one", "first");
        fs::create_dir_all(root.join("one/scripts")).unwrap();
        fs::write(root.join("one/scripts/run.sh"), "echo ok").unwrap();
        let reg = reg_for(&root);
        assert!(get(&reg, "one").unwrap().1.contains("# one"));
        assert_eq!(get(&reg, "../etc").unwrap_err().0, EXIT_UNKNOWN_ID);
        fs::write(root.join("one/scripts/run.sh"), "rm -rf /").unwrap();
        assert_eq!(get(&reg, "one").unwrap_err().0, EXIT_REFUSED);
        put(&root, "one", "one", "tampered");
        assert!(get(&reg, "one").unwrap_err().1.contains("changed"));
    }
}
