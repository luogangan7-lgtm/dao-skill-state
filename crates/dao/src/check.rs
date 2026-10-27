//! check — Rust port of `dao-skill-state/scripts/check_skill.py` (read-only frontmatter lint).
//!
//! Depends on: saphyr-parser (YAML events only); regex (Unicode categories for `isalnum`).
//! Called by: `main` (`dao check`). Differential gate: tests/fixtures/check_skill.json frozen
//! from the Python checker (M0) and `tools/check_skill_diff.py` over real corpora.
//! Invariants:
//! - Same JSON (key order path, errors, warnings, status), same messages, same exit codes
//!   (0 valid, 1 invalid, 2 uncheckable) as the Python checker for every input it handles.
//! - YAML is interpreted like PyYAML's SafeLoader + the checker's strict mapping constructor:
//!   YAML 1.1 implicit types (yes/no/on/off, 0o17-style ints, sexagesimal, timestamps), string
//!   keys only, no duplicate keys, unknown tags rejected, recursive aliases through mappings
//!   rejected. Inputs on which Python crashes with a traceback are reported here as invalid
//!   frontmatter (documented divergence; the checker must never crash).

use regex::Regex;
use saphyr_parser::{Event, Parser, ScalarStyle};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Issue {
    pub kind: String,
    pub message: String,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub path: String,
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
    pub status: String,
}

fn issue(kind: &str, message: impl Into<String>) -> Issue {
    Issue {
        kind: kind.into(),
        message: message.into(),
    }
}

// ---------- Python string semantics ----------

/// `str.isspace()`: White_Space plus the C0 separators \x1c-\x1f.
pub(crate) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `str.splitlines()` boundaries (after universal-newline translation of \r and \r\n).
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if matches!(
            c,
            '\n' | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            out.push(&s[start..i]);
            start = i + c.len_utf8();
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `str(pathlib.PurePosixPath(arg))`.
pub(crate) fn py_path_str(arg: &str) -> String {
    if arg.is_empty() {
        return ".".into();
    }
    let lead = if arg.starts_with("//") && !arg.starts_with("///") {
        "//"
    } else if arg.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = arg
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    let body = parts.join("/");
    match (lead, body.is_empty()) {
        ("", true) => ".".into(),
        (l, _) => format!("{l}{body}"),
    }
}

fn py_join(base: &str, name: &str) -> String {
    if base == "." {
        name.into()
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// `os.path.realpath(path, strict=True)` as CPython's posixpath implements it: lexical walk,
/// lstat + readlink per component (no case canonicalisation, unlike realpath(3) on macOS).
fn py_realpath(path: &Path) -> std::io::Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut seen: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    fn walk(
        base: PathBuf,
        rest: &Path,
        seen: &mut HashMap<PathBuf, Option<PathBuf>>,
    ) -> std::io::Result<PathBuf> {
        // ponytail: recursion per symlink hop; past 4096 hops report it as unresolvable
        // (CPython walks iteratively) instead of risking the stack
        if seen.len() > 4096 {
            return Err(std::io::Error::from_raw_os_error(62)); // ELOOP
        }
        let mut cur = base;
        let comps: Vec<_> = rest.components().collect();
        for (k, comp) in comps.iter().copied().enumerate() {
            // CPython 3.14 strict realpath: a non-directory cannot have further components
            if k > 0 && !fs::metadata(&cur).map(|m| m.is_dir()).unwrap_or(false) {
                return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
            }
            use std::path::Component::*;
            match comp {
                RootDir => cur = PathBuf::from("/"),
                CurDir | Prefix(_) => {}
                ParentDir => {
                    cur.pop();
                    if cur.as_os_str().is_empty() {
                        cur = PathBuf::from("/");
                    }
                }
                Normal(name) => {
                    let next = cur.join(name);
                    let meta = fs::symlink_metadata(&next)?;
                    if !meta.file_type().is_symlink() {
                        cur = next;
                        continue;
                    }
                    match seen.get(&next) {
                        Some(Some(done)) => {
                            cur = done.clone();
                            continue;
                        }
                        Some(None) => {
                            return Err(std::io::Error::from_raw_os_error(62)); // ELOOP
                        }
                        None => {}
                    }
                    seen.insert(next.clone(), None);
                    let target = fs::read_link(&next)?;
                    let resolved = walk(cur.clone(), &target, seen)?;
                    seen.insert(next, Some(resolved.clone()));
                    cur = resolved;
                }
            }
        }
        Ok(cur)
    }
    walk(PathBuf::from("/"), &abs, &mut seen)
}

// ---------- YAML (PyYAML SafeLoader semantics over saphyr events) ----------

enum Node {
    Scalar {
        value: String,
        plain: bool,
        tag: Option<String>,
    },
    Seq {
        items: Vec<usize>,
        tag: Option<String>,
    },
    Map {
        pairs: Vec<(usize, usize)>,
        tag: Option<String>,
    },
    Alias(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Val {
    Str(String),
    /// Shared like PyYAML's object references: an alias never copies the subtree.
    Map(Rc<Vec<(String, Val)>>),
    Seq,
    /// Any other constructed value (null, bool, int, float, timestamp, bytes, set, …);
    /// `hashable` matters only for `!!set` keys.
    Other {
        hashable: bool,
    },
}

const CORE: &str = "tag:yaml.org,2002:";
/// Collections open at once; see the composer note in `load`.
const MAX_DEPTH: usize = 490;

struct Resolvers {
    bool_: Regex,
    float: Regex,
    int: Regex,
    null: Regex,
    ts: Regex,
    ts_ctor: Regex,
}

impl Resolvers {
    fn new() -> Self {
        let r = |p: &str| Regex::new(p).unwrap();
        Resolvers {
            bool_: r(
                r"^(?:yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF)$",
            ),
            float: r(
                r"^(?:[-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?|\.[0-9][0-9_]*(?:[eE][-+][0-9]+)?|[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*|[-+]?\.(?:inf|Inf|INF)|\.(?:nan|NaN|NAN))$",
            ),
            int: r(
                r"^(?:[-+]?0b[0-1_]+|[-+]?0[0-7_]+|[-+]?(?:0|[1-9][0-9_]*)|[-+]?0x[0-9a-fA-F_]+|[-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+)$",
            ),
            null: r(r"^(?:~|null|Null|NULL|)$"),
            ts: r(
                r"^(?:[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]|[0-9][0-9][0-9][0-9]-[0-9][0-9]?-[0-9][0-9]?(?:[Tt]|[ \t]+)[0-9][0-9]?:[0-9][0-9]:[0-9][0-9](?:\.[0-9]*)?(?:[ \t]*(?:Z|[-+][0-9][0-9]?(?::[0-9][0-9])?))?)$",
            ),
            ts_ctor: r(
                r"^([0-9][0-9][0-9][0-9])-([0-9][0-9]?)-([0-9][0-9]?)(?:(?:[Tt]|[ \t]+)([0-9][0-9]?):([0-9][0-9]):([0-9][0-9])(?:\.([0-9]*))?(?:[ \t]*(Z|([-+])([0-9][0-9]?)(?::([0-9][0-9]))?))?)?$",
            ),
        }
    }

    /// Implicit tag of an untagged plain scalar, in PyYAML's first-character order.
    fn implicit(&self, value: &str) -> &'static str {
        // resolvers are keyed on the first character of the value as is, but Python's `$`
        // also matches just before one trailing newline (e.g. `! |` block "null\n")
        let first = value.chars().next();
        let v = value.strip_suffix('\n').unwrap_or(value);
        if first == Some('\n') {
            return "str";
        }
        let order: &[(&str, &Regex)] = match first {
            None => &[("null", &self.null)],
            Some(c) if "yYnNtTfFoO".contains(c) => &[("bool", &self.bool_), ("null", &self.null)],
            Some(c) if c.is_ascii_digit() => &[
                ("float", &self.float),
                ("int", &self.int),
                ("timestamp", &self.ts),
            ],
            Some('-' | '+') => &[("float", &self.float), ("int", &self.int)],
            Some('.') => &[("float", &self.float)],
            Some('~') => &[("null", &self.null)],
            Some('<') if v == "<<" => return "merge",
            Some('=') if v == "=" => return "value",
            // PyYAML's documentation-only resolver: no constructor, so loading fails
            Some('!' | '&' | '*') if v.chars().count() == 1 => return "yaml",
            _ => &[],
        };
        // `null` is only registered for '~', 'n', 'N' and '' (not for y/t/f/o)
        for (tag, re) in order {
            if *tag == "null" && !matches!(first, None | Some('~' | 'n' | 'N')) {
                continue;
            }
            if re.is_match(v) {
                return tag;
            }
        }
        "str"
    }
}

/// PyYAML's construction outcome: `Invalid` = a caught YAMLError/ValueError; `Crash` = an
/// exception the Python checker does not catch (traceback). Both print as invalid here.
#[derive(Debug, PartialEq)]
enum Fail {
    Invalid,
    Crash,
}

/// Flattened `!!set` pairs: (key node, value node, key is a retagged `=` string).
type Flat = Rc<Vec<(usize, usize, bool)>>;

struct Loader<'a> {
    nodes: Vec<Node>,
    res: &'a Resolvers,
    done: HashMap<usize, Val>,
    in_progress: HashSet<usize>,
    flat_memo: std::cell::RefCell<HashMap<usize, Flat>>,
    flat_busy: std::cell::RefCell<HashSet<usize>>,
}

/// Value of a Unicode decimal digit (`\p{Nd}`) as Python's `int()` reads it. Nd digits come in
/// runs of ten starting at zero, so the value is the distance from the run start mod 10.
fn py_digit(c: char) -> Option<u32> {
    if let Some(d) = c.to_digit(10) {
        return Some(d);
    }
    static ND: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let nd = ND.get_or_init(|| Regex::new(r"^\p{Nd}$").unwrap());
    let is_nd = |c: char| nd.is_match(c.encode_utf8(&mut [0; 4]));
    if !is_nd(c) {
        return None;
    }
    let mut k = 0;
    let mut p = c as u32;
    while let Some(prev) = p
        .checked_sub(1)
        .and_then(char::from_u32)
        .filter(|x| is_nd(*x))
    {
        k += 1;
        p = prev as u32;
    }
    Some(k % 10)
}

/// Python `int(s, base)` after `_` removal: whitespace, sign, optional matching prefix.
fn py_int_ok(s: &str, base: u32) -> bool {
    let mut t = py_strip(s);
    t = t.strip_prefix(['+', '-']).unwrap_or(t);
    let pre = match base {
        2 => ["0b", "0B"],
        8 => ["0o", "0O"],
        16 => ["0x", "0X"],
        _ => ["", ""],
    };
    if base != 10 {
        for p in pre {
            if let Some(r) = t.strip_prefix(p) {
                t = r;
                break;
            }
        }
    }
    !t.is_empty()
        && t.chars()
            .all(|c| c.is_digit(base) || py_digit(c).is_some_and(|d| d < base && !c.is_ascii()))
}

fn py_float_ok(s: &str) -> bool {
    // Python float() accepts Unicode decimal digits too: map them to ASCII first
    let t: String = py_strip(s)
        .chars()
        .map(|c| match py_digit(c) {
            Some(d) if !c.is_ascii() => char::from_digit(d, 10).unwrap(),
            _ => c,
        })
        .collect();
    !t.is_empty() && t.is_ascii() && t.parse::<f64>().is_ok()
}

pub(crate) fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        2 if (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Loader<'_> {
    fn timestamp(&self, v: &str) -> Result<Val, Fail> {
        // Python's `$` also matches before one trailing newline
        let Some(c) = self.res.ts_ctor.captures(v.strip_suffix('\n').unwrap_or(v)) else {
            return Err(Fail::Crash); // match is None → AttributeError
        };
        let n = |i: usize| c.get(i).map(|m| m.as_str().parse::<u32>().unwrap());
        let (y, mo, d) = (n(1).unwrap(), n(2).unwrap(), n(3).unwrap());
        if y == 0 || !(1..=12).contains(&mo) || d == 0 || d > days_in_month(y, mo) {
            return Err(Fail::Invalid);
        }
        if let Some(h) = n(4) {
            if h > 23 || n(5).unwrap() > 59 || n(6).unwrap() > 59 {
                return Err(Fail::Invalid);
            }
            if c.get(9).is_some() && n(10).unwrap() * 60 + n(11).unwrap_or(0) >= 24 * 60 {
                return Err(Fail::Invalid);
            }
        }
        Ok(Val::Other { hashable: true })
    }

    fn scalar(&self, value: &str, tag: &str) -> Result<Val, Fail> {
        let other = Ok(Val::Other { hashable: true });
        match tag.strip_prefix(CORE) {
            Some("str") => Ok(Val::Str(value.into())),
            Some("null") => other,
            Some("bool") => {
                if matches!(
                    value.to_lowercase().as_str(),
                    "yes" | "no" | "true" | "false" | "on" | "off"
                ) {
                    other
                } else {
                    Err(Fail::Crash) // KeyError
                }
            }
            Some("int") => {
                let v = value.replace('_', "");
                let body = v.strip_prefix(['+', '-']).unwrap_or(&v);
                let ok = if v.is_empty() || body.is_empty() {
                    return Err(Fail::Crash); // value[0] IndexError
                } else if body == "0" {
                    true
                } else if let Some(r) = body.strip_prefix("0b") {
                    py_int_ok(r, 2)
                } else if let Some(r) = body.strip_prefix("0x") {
                    py_int_ok(r, 16)
                } else if body.starts_with('0') {
                    py_int_ok(body, 8)
                } else if body.contains(':') {
                    body.split(':').all(|p| py_int_ok(p, 10))
                } else {
                    py_int_ok(body, 10)
                };
                if ok {
                    other
                } else {
                    Err(Fail::Invalid)
                }
            }
            Some("float") => {
                let v = value.replace('_', "").to_lowercase();
                let body = v.strip_prefix(['+', '-']).unwrap_or(&v);
                if v.is_empty() || body.is_empty() {
                    return Err(Fail::Crash);
                }
                let ok = body == ".inf"
                    || body == ".nan"
                    || if body.contains(':') {
                        body.split(':').all(py_float_ok)
                    } else {
                        py_float_ok(body)
                    };
                if ok {
                    other
                } else {
                    Err(Fail::Invalid)
                }
            }
            Some("binary") => {
                // value.encode('ascii') then CPython 3.14 binascii.a2b_base64 (non-strict):
                // non-alphabet characters are skipped, a pad only counts after two data
                // characters, decoding continues after a completed pad, and the final length
                // check is skipped only when the last significant character completed a pad.
                // Checked against binascii on every string over {A, =, .} up to length 9.
                if !value.is_ascii() {
                    return Err(Fail::Invalid);
                }
                let (mut q, mut pads, mut done) = (0usize, 0usize, false);
                for c in value.chars() {
                    if c == '=' {
                        if q >= 2 {
                            pads += 1;
                            done |= q + pads >= 4;
                        }
                    } else if c.is_ascii_alphanumeric() || c == '+' || c == '/' {
                        pads = 0;
                        done = false;
                        q = (q + 1) % 4;
                    }
                }
                if done || q == 0 {
                    other
                } else {
                    Err(Fail::Invalid)
                }
            }
            Some("timestamp") => self.timestamp(value),
            _ => Err(Fail::Invalid), // undefined tag (incl. merge, value, python/*, local !tags)
        }
    }

    fn construct(&mut self, id: usize, chain: &mut Vec<usize>) -> Result<Val, Fail> {
        if let Node::Alias(t) = self.nodes[id] {
            return self.construct(t, chain);
        }
        if let Some(v) = self.done.get(&id) {
            return Ok(v.clone());
        }
        if chain.contains(&id) {
            return Err(Fail::Invalid); // found unconstructable recursive node
        }
        if self.in_progress.contains(&id) {
            // Python would have finished this node already (we got here through a deferred
            // sequence); only its kind can be observed, never inspected further.
            return Ok(match self.nodes[id] {
                Node::Map { .. } => Val::Map(Rc::default()),
                _ => Val::Seq,
            });
        }
        self.in_progress.insert(id);
        let v = self.construct_new(id, chain);
        self.in_progress.remove(&id);
        let v = v?;
        self.done.insert(id, v.clone());
        Ok(v)
    }

    fn construct_new(&mut self, id: usize, chain: &mut Vec<usize>) -> Result<Val, Fail> {
        match &self.nodes[id] {
            Node::Alias(_) => unreachable!(),
            Node::Scalar { value, plain, tag } => {
                let value = value.clone();
                let tag = match tag.as_deref() {
                    // PyYAML resolves `!`-tagged scalars implicitly too, whatever their style
                    None if *plain => format!("{CORE}{}", self.res.implicit(&value)),
                    Some("!") => format!("{CORE}{}", self.res.implicit(&value)),
                    None => format!("{CORE}str"),
                    Some(t) => t.to_string(),
                };
                match tag.strip_prefix(CORE) {
                    // `!!map` on a scalar: iterating the string's characters as pairs
                    Some("map") => {
                        if value.is_empty() {
                            Ok(Val::Map(Rc::default()))
                        } else {
                            Err(Fail::Invalid)
                        }
                    }
                    Some("seq" | "omap" | "pairs" | "set") => Err(Fail::Invalid),
                    _ => self.scalar(&value, &tag),
                }
            }
            Node::Seq { items, tag } => {
                let items = items.clone();
                let tag = tag.clone().unwrap_or_else(|| format!("{CORE}seq"));
                match tag.as_str() {
                    t if t == format!("{CORE}seq") || t == "!" => {
                        // a deferred generator: children are built after this node returns
                        let mut fresh = Vec::new();
                        for c in items {
                            self.construct(c, &mut fresh)?;
                        }
                        Ok(Val::Seq)
                    }
                    t if t == format!("{CORE}omap") || t == format!("{CORE}pairs") => {
                        let mut fresh = Vec::new();
                        for c in items {
                            let c = self.deref(c);
                            let Node::Map { pairs, .. } = &self.nodes[c] else {
                                return Err(Fail::Invalid);
                            };
                            if pairs.len() != 1 {
                                return Err(Fail::Invalid);
                            }
                            let (k, v) = pairs[0];
                            self.construct(k, &mut fresh)?;
                            self.construct(v, &mut fresh)?;
                        }
                        Ok(Val::Seq)
                    }
                    t if t == format!("{CORE}map") => {
                        if items.is_empty() {
                            Ok(Val::Map(Rc::default()))
                        } else {
                            Err(Fail::Crash) // TypeError: cannot unpack non-iterable node
                        }
                    }
                    _ => Err(Fail::Invalid),
                }
            }
            Node::Map { pairs, tag } => {
                let pairs = pairs.clone();
                let tag = tag.clone().unwrap_or_else(|| format!("{CORE}map"));
                const SCALAR_CTORS: [&str; 7] =
                    ["str", "null", "bool", "int", "float", "binary", "timestamp"];
                if let Some(s) = tag.strip_prefix(CORE).filter(|s| SCALAR_CTORS.contains(s)) {
                    let text = self.scalar_text(id)?;
                    // construct_yaml_timestamp then regex-matches node.value (a list): TypeError
                    if s == "timestamp" {
                        return Err(Fail::Crash);
                    }
                    return self.scalar(&text, &tag);
                }
                if tag == format!("{CORE}map") || tag == "!" {
                    chain.push(id);
                    let mut out: Vec<(String, Val)> = Vec::new();
                    let mut keys: HashSet<String> = HashSet::new();
                    for (k, v) in pairs {
                        let key = self.construct(k, chain)?;
                        let Val::Str(key) = key else {
                            chain.pop();
                            return Err(Fail::Invalid);
                        };
                        if !keys.insert(key.clone()) {
                            chain.pop();
                            return Err(Fail::Invalid);
                        }
                        let val = self.construct(v, chain);
                        let Ok(val) = val else {
                            chain.pop();
                            return val;
                        };
                        out.push((key, val));
                    }
                    chain.pop();
                    Ok(Val::Map(Rc::new(out)))
                } else if tag == format!("{CORE}set") {
                    // SafeConstructor.construct_mapping (generator): flatten merges, then every
                    // key must be hashable; merged entries are constructed like the rest
                    let mut fresh = Vec::new();
                    for &(k, v, key_is_str) in self.flatten(id)?.iter() {
                        if !key_is_str {
                            match self.construct(k, &mut fresh)? {
                                Val::Map(_) | Val::Seq | Val::Other { hashable: false } => {
                                    return Err(Fail::Invalid)
                                }
                                _ => {}
                            }
                        }
                        self.construct(v, &mut fresh)?;
                    }
                    Ok(Val::Other { hashable: false })
                } else {
                    Err(Fail::Invalid)
                }
            }
        }
    }

    /// Tag a scalar node resolves to (PyYAML composer + resolver), or None for collections.
    fn scalar_tag(&self, id: usize) -> Option<String> {
        match &self.nodes[self.deref(id)] {
            Node::Scalar { value, plain, tag } => Some(match tag.as_deref() {
                None if *plain => format!("{CORE}{}", self.res.implicit(value)),
                Some("!") => format!("{CORE}{}", self.res.implicit(value)),
                None => format!("{CORE}str"),
                Some(t) => t.to_string(),
            }),
            _ => None,
        }
    }

    /// SafeConstructor.construct_scalar: a mapping stands for the value of its `=` key.
    /// A `=` chain that loops back (`&m {=: *m}`) is Python's RecursionError: invalid.
    fn scalar_text(&self, id: usize) -> Result<String, Fail> {
        let mut seen = HashSet::new();
        let mut id = self.deref(id);
        loop {
            if !seen.insert(id) {
                return Err(Fail::Invalid);
            }
            match &self.nodes[id] {
                Node::Scalar { value, .. } => return Ok(value.clone()),
                Node::Map { pairs, .. } => {
                    let Some(&(_, v)) = pairs.iter().find(|(k, _)| {
                        self.scalar_tag(*k).as_deref() == Some(&format!("{CORE}value"))
                    }) else {
                        return Err(Fail::Invalid);
                    };
                    id = self.deref(v);
                }
                _ => return Err(Fail::Invalid),
            }
        }
    }

    /// SafeConstructor.flatten_mapping: `<<` merges first (sequence merges in reverse),
    /// then the node's own pairs; `=` keys become string keys. Returns (key, value, key_is_str).
    /// PyYAML rewrites `node.value` in place, so a node is flattened once: results are memoised,
    /// and a node reached again while it is being flattened has already lost its merge keys
    /// (its own pairs only), which also ends self-referencing merges.
    fn flatten(&self, id: usize) -> Result<Flat, Fail> {
        let id = self.deref(id);
        if let Some(done) = self.flat_memo.borrow().get(&id) {
            return Ok(done.clone());
        }
        let Node::Map { pairs, .. } = &self.nodes[id] else {
            return Err(Fail::Invalid);
        };
        let tag_of = |k: usize| self.scalar_tag(k);
        let is = |k: usize, s: &str| tag_of(k).as_deref() == Some(&format!("{CORE}{s}"));
        if !self.flat_busy.borrow_mut().insert(id) {
            return Ok(Rc::new(
                pairs
                    .iter()
                    .filter(|(k, _)| !is(*k, "merge"))
                    .map(|&(k, v)| (k, v, is(k, "value")))
                    .collect(),
            ));
        }
        let res = (|| {
            let (mut merge, mut rest) = (Vec::new(), Vec::new());
            for &(k, v) in pairs {
                if is(k, "merge") {
                    match &self.nodes[self.deref(v)] {
                        Node::Map { .. } => merge.extend(self.flatten(v)?.iter().copied()),
                        Node::Seq { items, .. } => {
                            let mut sub = Vec::new();
                            for &i in items {
                                if !matches!(self.nodes[self.deref(i)], Node::Map { .. }) {
                                    return Err(Fail::Invalid);
                                }
                                sub.push(self.flatten(i)?);
                            }
                            for s in sub.into_iter().rev() {
                                merge.extend(s.iter().copied());
                            }
                        }
                        _ => return Err(Fail::Invalid),
                    }
                } else {
                    rest.push((k, v, is(k, "value")));
                }
            }
            merge.extend(rest);
            Ok(Rc::new(merge))
        })();
        self.flat_busy.borrow_mut().remove(&id);
        let res = res?;
        self.flat_memo.borrow_mut().insert(id, res.clone());
        Ok(res)
    }

    fn deref(&self, mut id: usize) -> usize {
        while let Node::Alias(t) = self.nodes[id] {
            id = t;
        }
        id
    }
}

/// PyYAML Reader: only printable characters are accepted.
fn printable(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' '..='~' | '\u{85}' | '\u{a0}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..)
}

/// A scalar as saphyr reported it, plus where it sits.
struct Span {
    start: usize,
    end: usize,
    style: ScalarStyle,
    in_flow: bool,
    is_key: bool,
}

/// Char offset just past the closing quote of the quoted scalar opening at `start` (saphyr's
/// reported end also covers trailing blanks, which would hide a tab after the quote).
fn quoted_end(src: &[char], start: usize) -> usize {
    let q = src[start];
    let mut i = start + 1;
    while i < src.len() {
        match src[i] {
            '\\' if q == '"' => i += 2,
            '\'' if q == '\'' && src.get(i + 1) == Some(&'\'') => i += 2,
            c if c == q => return i + 1,
            _ => i += 1,
        }
    }
    src.len()
}

/// Rules of PyYAML's scanner/parser that saphyr (YAML 1.2) does not enforce, checked on the
/// source with saphyr's scalar spans (char offsets). Any hit = invalid frontmatter:
/// - a tab outside quoted scalars, block scalar content and comments;
/// - anchors/aliases are `[0-9A-Za-z_-]+`, may not repeat, and `&name:` is not an anchor;
/// - in flow context (`[...]`, `{...}`) a plain scalar may not contain `?` nor start with
///   `:`, `?`, `|` or `>`; a bare `!` tag may not touch `,`, `]` or `}`;
/// - a simple (non-`?`) key may not span lines and must be under 1024 characters;
/// - content after a `...` document end needs a `---`; `%YAML` must be version 1.x.
fn pyyaml_lexical_ok(src: &[char], scalars: &[Span]) -> bool {
    let mut in_scalar = vec![None; src.len() + 1];
    for sp in scalars {
        let quoted = matches!(
            sp.style,
            ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted
        );
        let end = if quoted && sp.start < src.len() {
            quoted_end(src, sp.start)
        } else {
            sp.end.min(src.len())
        };
        for slot in in_scalar.iter_mut().take(end).skip(sp.start) {
            *slot = Some(sp.style);
        }
        let raw: String = src[sp.start.min(src.len())..end].iter().collect();
        if matches!(sp.style, ScalarStyle::Literal | ScalarStyle::Folded) {
            // the block header line (`|`/`>` plus indicators) may not carry a tab
            // saphyr's span starts at the content (after indentation) or, for an empty block,
            // at the indicator itself
            let s0 = sp.start.min(src.len());
            let at_content = src[..s0]
                .iter()
                .rev()
                .take_while(|c| **c != '\n')
                .all(|c| *c == ' ');
            let upto = if at_content {
                s0
            } else {
                (s0 + 1).min(src.len())
            };
            if let Some(h) = src[..upto].iter().rposition(|c| *c == '|' || *c == '>') {
                if src[h..]
                    .iter()
                    .take_while(|c| **c != '\n')
                    .any(|c| *c == '\t')
                {
                    return false;
                }
            }
        }
        if sp.in_flow
            && sp.style == ScalarStyle::Plain
            && (raw.contains('?') || raw.starts_with([':', '?', '|', '>']))
        {
            return false;
        }
        if sp.is_key {
            let explicit = src[..sp.start]
                .iter()
                .rev()
                .find(|c| !matches!(c, ' ' | '\t'))
                == Some(&'?');
            if !explicit && (raw.contains('\n') || raw.chars().count() > 1024) {
                return false;
            }
        }
    }
    let mut comment = vec![false; src.len()];
    let mut i = 0;
    while i < src.len() {
        if src[i] == '#'
            && in_scalar[i].is_none()
            && (i == 0 || matches!(src[i - 1], ' ' | '\t' | '\n'))
        {
            while i < src.len() && src[i] != '\n' {
                comment[i] = true;
                i += 1;
            }
        }
        i += 1;
    }
    let mut seen = HashSet::new();
    for i in 0..src.len() {
        if comment[i] {
            continue;
        }
        let c = src[i];
        if c == '\t'
            && !matches!(
                in_scalar[i],
                Some(
                    ScalarStyle::SingleQuoted
                        | ScalarStyle::DoubleQuoted
                        | ScalarStyle::Literal
                        | ScalarStyle::Folded
                )
            )
        {
            return false;
        }
        if in_scalar[i].is_some() {
            continue;
        }
        if c == '!' && matches!(src.get(i + 1), Some(',' | ']' | '}')) {
            return false;
        }
        if c == '&' || c == '*' {
            let name: String = src[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == '-' || **c == '_')
                .collect();
            let next = src
                .get(i + 1 + name.chars().count())
                .copied()
                .unwrap_or('\n');
            let ends = matches!(
                next,
                ' ' | '\t' | '\n' | '\r' | '?' | ',' | ']' | '}' | '%' | '@' | '`'
            ) || (c == '*' && next == ':'); // `*a: v` is an alias key; `&a:` is rejected
            if name.is_empty() || !ends {
                return false;
            }
            if c == '&' && !seen.insert(name) {
                return false; // found duplicate anchor
            }
        }
    }
    // document markers and directives (line-level)
    let text: String = src.iter().collect();
    let mut after_end = false;
    let mut seen_content = false;
    for line in text.lines() {
        let content = !line.trim().is_empty() && !line.trim_start().starts_with('#');
        if !seen_content && (line == "..." || line.starts_with("... ")) {
            return false; // '...' before the first document
        }
        seen_content |= content;
        if after_end && content && !line.starts_with("---") {
            return false; // expected '<document start>'
        }
        if line == "..." || line.starts_with("... ") {
            after_end = true;
        } else if content {
            after_end = false;
        }
        let mut words = line.split_whitespace();
        if words.next() == Some("%YAML")
            && words.next().and_then(|v| v.split('.').next()) != Some("1")
        {
            return false;
        }
    }
    true
}

/// `yaml.load(source, StrictLoader)`: Ok(None) for an empty stream.
fn load(source: &str) -> Result<Option<Val>, Fail> {
    if !source.chars().all(printable) {
        return Err(Fail::Invalid);
    }
    let src = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut parser = Parser::new_from_str(src);
    let mut nodes: Vec<Node> = Vec::new();
    let mut anchors: HashMap<usize, usize> = HashMap::new();
    let mut stack: Vec<(usize, Option<usize>)> = Vec::new(); // (node, pending map key)
    let mut roots = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut flow: Vec<bool> = Vec::new(); // per open collection: flow style ([ or {)
    let tag_str = |t: &Option<std::borrow::Cow<'_, saphyr_parser::Tag>>| {
        t.as_ref().map(|t| format!("{}{}", t.handle, t.suffix))
    };
    while let Some(ev) = parser.next_event() {
        let (ev, span) = ev.map_err(|_| Fail::Invalid)?;
        if let Event::Scalar(_, st, ..) = &ev {
            let is_key =
                matches!(stack.last(), Some((p, None)) if matches!(nodes[*p], Node::Map { .. }));
            spans.push(Span {
                start: span.start.index(),
                end: span.end.index(),
                style: *st,
                in_flow: flow.iter().any(|f| *f),
                is_key,
            });
        }
        if matches!(ev, Event::SequenceStart(..) | Event::MappingStart(..)) {
            flow.push(matches!(chars.get(span.start.index()), Some('[' | '{')));
        }
        if matches!(ev, Event::SequenceEnd | Event::MappingEnd) {
            flow.pop();
        }
        let is_seq = matches!(ev, Event::SequenceStart(..));
        let new = match ev {
            Event::StreamEnd => break,
            Event::Nothing | Event::StreamStart | Event::DocumentStart(_) | Event::DocumentEnd => {
                continue
            }
            Event::Alias(a) => Node::Alias(*anchors.get(&a).ok_or(Fail::Invalid)?),
            Event::Scalar(v, style, anchor, tag) => {
                let id = nodes.len();
                if anchor > 0 {
                    anchors.insert(anchor, id);
                }
                // PyYAML: a block scalar with no content lines is "" (clipped) or one break per
                // blank line (kept, `|+`); saphyr adds the header's line break
                let mut value = v.into_owned();
                if matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
                    && value.chars().all(|c| c == '\n')
                {
                    let header = chars[..(span.start.index() + 1).min(chars.len())]
                        .iter()
                        .rposition(|c| *c == '|' || *c == '>')
                        .map(|i| {
                            chars[i..]
                                .iter()
                                .take_while(|c| **c != '\n')
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    if header.contains('+') {
                        // kept: one break per line after the header line (saphyr reports at
                        // least one; its span starts at the indicator when there is no content)
                        let raw = &chars[span.start.index().min(chars.len())
                            ..span.end.index().min(chars.len())];
                        let breaks = raw.iter().filter(|c| **c == '\n').count();
                        let header_break = usize::from(matches!(raw.first(), Some('|' | '>')));
                        value = "\n".repeat(breaks.saturating_sub(header_break));
                    } else {
                        value.clear();
                    }
                }
                Node::Scalar {
                    value,
                    plain: style == ScalarStyle::Plain,
                    tag: tag_str(&tag),
                }
            }
            Event::SequenceStart(anchor, tag) | Event::MappingStart(anchor, tag) => {
                let id = nodes.len();
                if anchor > 0 {
                    anchors.insert(anchor, id);
                }
                let t = tag_str(&tag);
                let node = if is_seq {
                    Node::Seq {
                        items: Vec::new(),
                        tag: t,
                    }
                } else {
                    Node::Map {
                        pairs: Vec::new(),
                        tag: t,
                    }
                };
                nodes.push(node);
                attach(&mut nodes, &mut stack, &mut roots, id);
                stack.push((id, None));
                // PyYAML composes recursively and the checker turns RecursionError into
                // invalid frontmatter at ~490 levels (489 block, 490 flow, 491 flow seq on
                // CPython 3.14). Stricter-or-equal: reject beyond MAX_DEPTH collections.
                if stack.len() > MAX_DEPTH {
                    return Err(Fail::Invalid);
                }
                continue;
            }
            Event::SequenceEnd | Event::MappingEnd => {
                stack.pop();
                continue;
            }
        };
        let id = nodes.len();
        nodes.push(new);
        attach(&mut nodes, &mut stack, &mut roots, id);
    }
    if !pyyaml_lexical_ok(&chars, &spans) {
        return Err(Fail::Invalid);
    }
    match roots.len() {
        0 => Ok(None),
        1 => {
            let res = Resolvers::new();
            let mut l = Loader {
                nodes,
                res: &res,
                done: HashMap::new(),
                in_progress: HashSet::new(),
                flat_memo: Default::default(),
                flat_busy: Default::default(),
            };
            l.construct(roots[0], &mut Vec::new()).map(Some)
        }
        _ => Err(Fail::Invalid), // expected a single document in the stream
    }
}

fn attach(
    nodes: &mut [Node],
    stack: &mut [(usize, Option<usize>)],
    roots: &mut Vec<usize>,
    id: usize,
) {
    let Some((parent, pending)) = stack.last_mut() else {
        roots.push(id);
        return;
    };
    match &mut nodes[*parent] {
        Node::Seq { items, .. } => items.push(id),
        Node::Map { pairs, .. } => match pending.take() {
            None => *pending = Some(id),
            Some(k) => pairs.push((k, id)),
        },
        _ => unreachable!(),
    }
}

// ---------- the checker ----------

const ALLOWED_TOP: [&str; 6] = [
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed-tools",
];

pub(crate) fn parse_frontmatter(text: &str) -> Result<Vec<(String, Val)>, &'static str> {
    let lines = py_splitlines(text);
    if lines.first() != Some(&"---") {
        return Err("SKILL.md must start with YAML frontmatter (---)");
    }
    let Some(end) = lines
        .iter()
        .skip(1)
        .position(|l| *l == "---")
        .map(|i| i + 1)
    else {
        return Err("YAML frontmatter has no closing ---");
    };
    let source = lines[1..end].join("\n") + "\n";
    match load(&source) {
        Ok(Some(Val::Map(m))) => Ok((*m).clone()),
        Ok(_) => Err("frontmatter must be a mapping"),
        Err(_) => {
            Err("invalid YAML frontmatter (syntax, key, duplicate, recursion or unsupported tag)")
        }
    }
}

/// Names both values and the fix: agents otherwise moved or nested a folder the user asked for
/// to satisfy this error. Kept identical to dao-skill-state/scripts/check_skill.py.
fn mismatch_message(name: &str, folder: &str) -> String {
    let alnum = Regex::new(r"^[\p{L}\p{N}]+$").unwrap();
    let valid = (1..=64).contains(&folder.chars().count())
        && folder == folder.to_lowercase()
        && folder.split('-').all(|p| alnum.is_match(p));
    if valid {
        format!(
            "name '{name}' must match the skill directory name '{folder}': set name to '{folder}' \
             or move the skill to a folder named '{name}'; keep a folder the user asked for and \
             report any mismatch"
        )
    } else {
        format!(
            "name '{name}' must match the skill directory name '{folder}', which is not a valid skill \
             name: keep a folder the user asked for, keep a valid name and report the mismatch"
        )
    }
}

/// Regular, non-symlink files directly in `folder`; empty when it is missing, a link or unreadable.
fn regular_files(folder: &Path) -> Vec<PathBuf> {
    match fs::symlink_metadata(folder) {
        Ok(m) if m.is_dir() => {}
        _ => return Vec::new(),
    }
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
        .collect()
}

/// 4+ distinct ratios in SKILL.md and references/*.md, and no file in scripts/: agents copied a
/// skill's wrong contrast table while a skill with a script passed (eval H, run 40471493176).
/// Same rule as `_quotes_unchecked_values` in check_skill.py.
fn quotes_unchecked_values(root: &Path, text: &str) -> bool {
    let ratio = Regex::new(r"(?-u:\b)[0-9]+(?:\.[0-9]+)?:1(?-u:\b)").unwrap();
    let mut texts = vec![text.to_string()];
    for f in regular_files(&root.join("references")) {
        let small = fs::metadata(&f).is_ok_and(|m| m.len() <= 1024 * 1024);
        if f.extension().is_some_and(|x| x == "md") && small {
            if let Ok(Ok(t)) = fs::read(&f).map(String::from_utf8) {
                texts.push(t);
            }
        }
    }
    let found: HashSet<&str> = texts
        .iter()
        .flat_map(|t| ratio.find_iter(t).map(|m| m.as_str()))
        .collect();
    found.len() >= 4 && regular_files(&root.join("scripts")).is_empty()
}

pub fn check_skill(arg: &str) -> CheckResult {
    let path = py_path_str(arg);
    let mut r = CheckResult {
        path: path.clone(),
        errors: Vec::new(),
        warnings: Vec::new(),
        status: String::new(),
    };
    let uncheckable = |mut r: CheckResult, kind: &str, msg: String| {
        r.status = "uncheckable".into();
        r.errors.push(issue(kind, msg));
        r
    };
    let root = match py_realpath(Path::new(&path)) {
        Ok(p) => p,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return uncheckable(
                r,
                "uncheckable",
                format!("missing {}", py_join(&path, "SKILL.md")),
            );
        }
        Err(_) => {
            return uncheckable(
                r,
                "uncheckable",
                "cannot resolve/read SKILL.md as UTF-8".into(),
            )
        }
    };
    let skill_file = root.join("SKILL.md");
    match fs::metadata(&skill_file) {
        Ok(m) if m.is_file() && m.len() <= 1024 * 1024 => {}
        _ => {
            return uncheckable(
                r,
                "resource",
                "SKILL.md must be a regular file no larger than this checker's 1 MiB limit".into(),
            )
        }
    }
    let text = match fs::read(&skill_file).map(String::from_utf8) {
        Ok(Ok(t)) => t.replace("\r\n", "\n").replace('\r', "\n"),
        _ => {
            return uncheckable(
                r,
                "uncheckable",
                "cannot resolve/read SKILL.md as UTF-8".into(),
            )
        }
    };
    let fm = match parse_frontmatter(&text) {
        Ok(m) => m,
        Err(msg) => {
            r.status = "invalid".into();
            r.errors.push(issue("frontmatter", msg));
            return r;
        }
    };
    let get = |k: &str| fm.iter().find(|(key, _)| key == k).map(|(_, v)| v);
    let mut unknown: Vec<&str> = fm
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| !ALLOWED_TOP.contains(k))
        .collect();
    unknown.sort();
    if !unknown.is_empty() {
        r.warnings.push(issue(
            "unknown-field",
            format!("unrecognized optional fields: {}", unknown.join(", ")),
        ));
    }
    let dir_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let alnum = Regex::new(r"^[\p{L}\p{N}]+$").unwrap();
    match get("name") {
        Some(Val::Str(name)) if (1..=64).contains(&name.chars().count()) => {
            if *name != dir_name {
                r.errors
                    .push(issue("name", mismatch_message(name, &dir_name)));
            } else if *name != name.to_lowercase() {
                r.errors
                    .push(issue("name", "name must not contain uppercase characters"));
            } else if !name.split('-').all(|p| alnum.is_match(p)) {
                r.errors.push(issue(
                    "name",
                    "name segments must be non-empty alphanumeric strings separated by hyphens",
                ));
            }
        }
        _ => r
            .errors
            .push(issue("name", "name must be a string of 1..64 characters")),
    }
    if let Some(Val::Str(name)) = get("name") {
        if !Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
            .unwrap()
            .is_match(name)
        {
            r.warnings.push(issue(
                "portability",
                "name is not ASCII lowercase portable form; Unicode names are accepted",
            ));
        }
    }
    match get("description") {
        Some(Val::Str(d)) if !py_strip(d).is_empty() && (1..=1024).contains(&d.chars().count()) => {
            // words that say when a skill applies; descriptions under 8 words are too short to judge
            let trigger =
                Regex::new(r"(?i)\bwhen(ever)?\b|\buse[ds]?\b|\btrigger|\binvoke|时|用于|使用")
                    .unwrap();
            if d.split(py_space).filter(|w| !w.is_empty()).count() >= 8 && !trigger.is_match(d) {
                r.warnings.push(issue("trigger", "description says what the skill does but not when to use it; hosts choose skills from the description alone, so name the requests or situations that should load it (for example \"Use when ...\")"));
            }
        }
        _ => r.errors.push(issue(
            "description",
            "description must be a string of 1..1024 characters",
        )),
    }
    for (field, limit) in [
        ("license", None),
        ("compatibility", Some(500)),
        ("allowed-tools", None),
    ] {
        let Some(v) = get(field) else { continue };
        let ok = matches!(v, Val::Str(s) if !py_strip(s).is_empty() && limit.is_none_or(|l| s.chars().count() <= l));
        if !ok {
            let suffix = match limit {
                Some(l) => format!(" string of 1..{l} characters"),
                None => " non-empty string".into(),
            };
            r.errors
                .push(issue(field, format!("{field} must be a{suffix}")));
        }
    }
    if let Some(m) = get("metadata") {
        match m {
            Val::Map(items) => {
                if items.iter().any(|(_, v)| !matches!(v, Val::Str(_))) {
                    r.errors.push(issue(
                        "metadata",
                        "metadata keys and values must all be strings",
                    ));
                }
            }
            _ => r.errors.push(issue(
                "metadata",
                "metadata must be a string:string mapping",
            )),
        }
    }
    if quotes_unchecked_values(&root, &text) {
        r.warnings.push(issue("unchecked-values", "the skill quotes several ratios (N:1) but has no file in scripts/; agents copy quoted values even when they are wrong, so add a script under scripts/ that computes them and say when to run it"));
    }
    r.status = if r.errors.is_empty() {
        "valid"
    } else {
        "invalid"
    }
    .into();
    r
}

/// Python `json.dumps(result, ensure_ascii=False, indent=2)` for this result shape.
pub fn to_json(r: &CheckResult) -> String {
    serde_json::to_string_pretty(r).unwrap()
}

pub fn exit_code(r: &CheckResult) -> u8 {
    match r.status.as_str() {
        "uncheckable" => 2,
        "valid" => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm(src: &str) -> Result<Option<Val>, Fail> {
        load(src)
    }

    #[test]
    fn description_without_a_trigger_warns() {
        let d = std::env::temp_dir().join(format!("dao-check-trig-{}", std::process::id()));
        for (text, warns) in [
            ("Write and review web pages so they pass review with readable contrast.", true),
            ("Write and review web pages with readable contrast. Use when an agent changes a page.", false),
            ("检查网页的对比度、字号和手机布局，在修改任何网页时使用这个技能。", false),
            ("short", false),
        ] {
            let dir = d.join("pages");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("SKILL.md"), format!("---\nname: pages\ndescription: {text}\n---\n")).unwrap();
            let r = check_skill(&dir.to_string_lossy());
            let hit = r.warnings.iter().any(|w| w.kind == "trigger" && w.message.contains("when to use it"));
            assert_eq!(hit, warns, "{text}");
            assert_eq!(r.status, "valid");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn quoted_ratios_without_a_script_warn() {
        let d = std::env::temp_dir().join(format!("dao-check-ratio-{}", std::process::id()));
        let dir = d.join("pages");
        fs::create_dir_all(dir.join("references")).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: pages\ndescription: short\n---\n",
        )
        .unwrap();
        fs::write(
            dir.join("references/contrast.md"),
            "#111827 16.7:1, #4b5563 10.9:1, #6b7280 7.2:1, minimum 4.5:1\n",
        )
        .unwrap();
        let hit = |p: &Path| {
            check_skill(&p.to_string_lossy())
                .warnings
                .iter()
                .any(|w| w.kind == "unchecked-values" && w.message.contains("scripts/"))
        };
        assert!(hit(&dir));
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join("scripts/contrast.py"), "print()\n").unwrap();
        assert!(!hit(&dir));
        let rules = d.join("rules");
        fs::create_dir_all(&rules).unwrap();
        fs::write(
            rules.join("SKILL.md"),
            "---\nname: rules\ndescription: short\n---\nBody text 4.5:1, large text 3:1, AAA 7:1.\n",
        )
        .unwrap();
        assert!(!hit(&rules));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn script_not_named_in_skill_md_warns() {
        let d = std::env::temp_dir().join(format!("dao-check-script-{}", std::process::id()));
        let dir = d.join("pages");
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: pages\ndescription: short\n---\n",
        )
        .unwrap();
        fs::write(dir.join("scripts/contrast.py"), "print()\n").unwrap();
        let hit = |p: &Path| {
            check_skill(&p.to_string_lossy())
                .warnings
                .iter()
                .any(|w| w.kind == "unnamed-script" && w.message.contains("contrast.py"))
        };
        assert!(hit(&dir));
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: pages\ndescription: short\n---\nRun `python3 scripts/contrast.py FG BG` before writing colours.\n",
        )
        .unwrap();
        assert!(!hit(&dir));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn yaml_1_1_implicit_types_like_pyyaml() {
        let res = Resolvers::new();
        for (v, t) in [
            ("yes", "bool"),
            ("Off", "bool"),
            ("y", "str"),
            ("", "null"),
            ("~", "null"),
            ("NULL", "null"),
            ("0o17", "str"),
            ("017", "int"),
            ("08", "str"),
            ("1_000", "int"),
            ("0x1F", "int"),
            ("190:20:30", "int"),
            ("1.5", "float"),
            ("1e5", "str"),
            ("1.0e+5", "float"),
            (".inf", "float"),
            ("2026-10-13", "timestamp"),
            ("2026-1-3 1:02:03", "timestamp"),
            ("<<", "merge"),
            ("=", "value"),
            ("none", "str"),
        ] {
            assert_eq!(res.implicit(v), t, "{v:?}");
        }
    }

    #[test]
    fn name_mismatch_message_matches_python() {
        assert_eq!(
            mismatch_message("clean-csv", "new-skill"),
            "name 'clean-csv' must match the skill directory name 'new-skill': set name to \
             'new-skill' or move the skill to a folder named 'clean-csv'; keep a folder the user \
             asked for and report any mismatch"
        );
        assert_eq!(
            mismatch_message("my-skill", "My_Skill"),
            "name 'my-skill' must match the skill directory name 'My_Skill', which is not a valid \
             skill name: keep a folder the user asked for, keep a valid name and report the mismatch"
        );
    }

    #[test]
    fn strict_mapping_rules() {
        assert!(matches!(fm("a: 1\n"), Ok(Some(Val::Map(_)))));
        assert_eq!(fm("a: 1\na: 2\n"), Err(Fail::Invalid), "duplicate");
        assert_eq!(fm("12: v\n"), Err(Fail::Invalid), "non-string key");
        assert_eq!(fm("? [a, b]\n: v\n"), Err(Fail::Invalid), "sequence key");
        assert_eq!(
            fm("a: !!python/object/apply:os.system ['id']\n"),
            Err(Fail::Invalid)
        );
        assert_eq!(
            fm("<<: {a: 1}\n"),
            Err(Fail::Invalid),
            "merge has no constructor"
        );
        assert_eq!(fm("a: =\n"), Err(Fail::Invalid), "value tag");
        assert_eq!(
            fm("a: &x {b: *x}\n"),
            Err(Fail::Invalid),
            "recursive mapping"
        );
        assert!(
            fm("a: &x [*x]\n").is_ok(),
            "recursive sequence is constructible"
        );
        assert!(
            fm("a: &x {b: [*x]}\n").is_ok(),
            "through a sequence it is deferred"
        );
        assert_eq!(fm("a: 2026-02-30\n"), Err(Fail::Invalid), "invalid date");
        assert_eq!(fm("a: !!int '0b'\n"), Err(Fail::Invalid));
        assert_eq!(fm("a: !!bool maybe\n"), Err(Fail::Crash));
        assert_eq!(fm("a: 1\n---\nb: 2\n"), Err(Fail::Invalid), "two documents");
        assert_eq!(fm("\n"), Ok(None));
        assert_eq!(fm("a: \u{1}\n"), Err(Fail::Invalid), "non-printable");
        assert!(
            matches!(fm("a: !!str 12\n"), Ok(Some(Val::Map(m))) if m[0].1 == Val::Str("12".into()))
        );
    }

    fn b64(s: &str) -> Vec<u8> {
        let val = |c: u8| match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            _ => 63,
        };
        let s: Vec<u8> = s.bytes().filter(|c| *c != b'=').collect();
        let mut out = Vec::new();
        for ch in s.chunks(4) {
            let n = ch
                .iter()
                .enumerate()
                .fold(0u32, |acc, (i, c)| acc | (val(*c) as u32) << (18 - 6 * i));
            out.extend_from_slice(&n.to_be_bytes()[1..ch.len()]);
        }
        out
    }

    /// M0 → M1 gate: every frozen Python result (tests/fixtures/check_skill.json, produced by
    /// tools/check_skill_diff.py --freeze) is reproduced byte-for-byte, except the documented
    /// divergences: Python tracebacks (reported here as invalid frontmatter) and KNOWN below.
    #[test]
    fn matches_frozen_python_baseline() {
        // Documented divergences, all "Rust stricter, never looser":
        // - v76m, r-sqcont: saphyr (YAML 1.2) rejects a quoted continuation line at or below
        //   the parent's indent; PyYAML accepts it.
        // - r-depth-{flow,seq}-{480,489,490}: saphyr's flow nesting limit is 256; PyYAML's
        //   recursion limit allows ~490. No real frontmatter nests that deep.
        // - r-aliaskey: `*a: v` — YAML 1.2 lets alias names contain ':', so saphyr reads an
        //   undefined alias `a:`; PyYAML ends the alias at ':' and makes it a key.
        const KNOWN: [&str; 9] = [
            "r-aliaskey",
            "v76m",
            "r-sqcont",
            "r-depth-flow-480",
            "r-depth-seq-480",
            "r-depth-flow-489",
            "r-depth-seq-489",
            "r-depth-flow-490",
            "r-depth-seq-490",
        ];
        let raw = include_str!("../tests/fixtures/check_skill.json");
        let cases: Vec<serde_json::Value> = serde_json::from_str(raw).unwrap();
        let root = std::env::temp_dir().join(format!("dao-check-fx-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut bad = Vec::new();
        for (i, c) in cases.iter().enumerate() {
            let dir = root
                .join(format!("{i:05}"))
                .join(c["dir"].as_str().unwrap());
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                b64(c["skill_md_b64"].as_str().unwrap()),
            )
            .unwrap();
            let path = dir.to_string_lossy().into_owned();
            let r = check_skill(&path);
            let (got, code) = (to_json(&r) + "\n", exit_code(&r));
            let ok = match c["python_exit"].as_u64() {
                Some(e) => {
                    got == c["python_stdout"].as_str().unwrap().replace("{DIR}", &path)
                        && code as u64 == e
                }
                None => code == 1 && got.contains("invalid YAML frontmatter"), // PY_CRASH
            };
            if !ok {
                bad.push(c["dir"].as_str().unwrap().to_string());
            }
        }
        let _ = fs::remove_dir_all(&root);
        let mut known = KNOWN.to_vec();
        known.sort_unstable();
        bad.sort_unstable();
        assert_eq!(
            bad, known,
            "unexplained differences from the Python baseline"
        );
    }

    #[test]
    fn python_string_and_path_semantics() {
        // a path through a regular file is "missing" (CPython 3.14 strict realpath)
        let d = std::env::temp_dir().join(format!("dao-check-pf-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("SKILL.md"), "---\nname: x\ndescription: y\n---\n").unwrap();
        let through = format!("{}/SKILL.md/..", d.display());
        assert_eq!(exit_code(&check_skill(&through)), 2);
        let _ = fs::remove_dir_all(&d);
        assert_eq!(py_splitlines("a\u{2028}b\u{1c}c\n"), vec!["a", "b", "c"]);
        assert_eq!(py_strip("\u{1f} x \u{3000}"), "x");
        for (a, b) in [
            ("", "."),
            (".", "."),
            ("./a/", "a"),
            ("a//b/./c", "a/b/c"),
            ("//x", "//x"),
            ("///x", "/x"),
            ("../a", "../a"),
        ] {
            assert_eq!(py_path_str(a), b, "{a:?}");
        }
        assert_eq!(py_join(".", "SKILL.md"), "SKILL.md");
    }
}
