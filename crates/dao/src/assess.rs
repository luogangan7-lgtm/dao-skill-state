//! assess — Rust port of `dao-skill-state/scripts/check_assessment.py` (dao-assessment/v1
//! validator and counter; bookkeeping only, evidence strings are never opened).
//!
//! Depends on: check (Python `str.isspace`, `pathlib` str, month lengths); regex (Unicode
//! categories for `repr`). Called by: `main` (`dao assess FILE`).
//! Differential gate: tests/fixtures/check_assessment.json, frozen from the Python CLI by
//! tools/diff_cli.py and replayed against the binary by tests/assess_baseline.rs.
//! Invariants:
//! - Same stdout bytes and exit codes as the Python CLI (0 summary, 1 invalid, 2 input error or
//!   usage), same error order and messages, same admission math (counts, coverage, pairing,
//!   `math.fsum`, null reasons). No semantics were changed in the port.
//! - JSON is read like CPython's `json.loads` (C scanner) with the checker's hooks: strings are
//!   code-point sequences (lone surrogates kept), ints are arbitrary precision with CPython's
//!   4300-digit limit, NaN/Infinity rejected, duplicate keys rejected when their object closes,
//!   same messages and character positions.
//! - Never recurses on input: iterative parser into an arena (no recursive drop either).
//!
//! Documented divergences (see tests/assess_baseline.rs KNOWN): nesting deeper than
//! MAX_DEPTH (Python's limit is its C stack, its message carries the stack size); integers
//! over 4300 digits in output (Python crashes in `str(int)`, Rust prints them); `-h` help text
//! names `dao assess` instead of `check_assessment.py`.

use crate::check::{days_in_month, py_path_str, py_space};
use regex::Regex;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::OnceLock;

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ARMS: usize = 8;
const MAX_PLANNED_ROWS: &str = "100000";
/// CPython 3.14 stops at its C stack (~116k levels on macOS arm64, less on 8 MiB stacks).
const MAX_DEPTH: usize = 100_000;
const MAX_INT_DIGITS: usize = 4300; // sys.int_info.default_max_str_digits
const KINDS: [&str; 3] = ["positive", "boundary", "nontrigger"];
const STATUSES: [&str; 5] = ["PASS", "FAIL", "UNKNOWN", "NOT_RUN", "INVALID"];
const TRUST: [&str; 5] = ["PASS", "CONDITIONAL", "FAIL", "UNKNOWN", "N/A"];
const COST_FIELDS: [&str; 4] = [
    "seconds",
    "input_tokens",
    "output_tokens",
    "cached_input_tokens",
];
const ROOT_FIELDS: [&str; 7] = [
    "schema",
    "identity",
    "arms",
    "repetitions",
    "cases",
    "isolation",
    "runs",
];
const IDENTITY_FIELDS: [&str; 7] = [
    "skill",
    "skill_sha256",
    "task_set_sha256",
    "model",
    "host",
    "date",
    "rubric",
];
const RUN_FIELDS: [&str; 7] = [
    "case_id",
    "arm",
    "repetition",
    "status",
    "trust",
    "evidence",
    "cost",
];
/// int(sys.float_info.max)
const DBL_MAX: &str = "179769313486231570814527423731704356798070567525844996598917476803157260780028538760589558632766878171540458953514382464234321326889464182768467546703537516986049910576551282076245490090389328944075868508455133942304583236903222948165808559332123348274797826204144723168738177180919299881250404026184124858368";

#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(not(target_os = "linux"))]
const O_NONBLOCK: i32 = 0x0004; // macOS / BSD

/// A Python `str`: code points, lone surrogates allowed.
type S = Vec<u32>;

fn u(s: &str) -> S {
    s.chars().map(|c| c as u32).collect()
}

// ---------- Python ints (non-negative magnitudes as decimal strings) ----------

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Int {
    neg: bool,
    mag: String, // no leading zeros; "0" is never negative
}

impl Int {
    fn positive(&self) -> bool {
        !self.neg && self.mag != "0"
    }
}

fn mag_cmp(a: &str, b: &str) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn mag_add(a: &str, b: &str) -> String {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let (mut i, mut j, mut carry) = (a.len(), b.len(), 0u8);
    while i > 0 || j > 0 || carry > 0 {
        let mut s = carry;
        if i > 0 {
            i -= 1;
            s += a[i] - b'0';
        }
        if j > 0 {
            j -= 1;
            s += b[j] - b'0';
        }
        out.push(b'0' + s % 10);
        carry = s / 10;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

fn mag_mul(a: &str, m: u128) -> String {
    if m == 0 || a == "0" {
        return "0".into();
    }
    let mut out = Vec::new();
    let mut carry = 0u128;
    for d in a.bytes().rev() {
        let p = (d - b'0') as u128 * m + carry;
        out.push(b'0' + (p % 10) as u8);
        carry = p / 10;
    }
    while carry > 0 {
        out.push(b'0' + (carry % 10) as u8);
        carry /= 10;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

// ---------- JSON values (arena) and CPython's json scanner ----------

enum Val {
    Null,
    Bool,
    Int(Int),
    Float(f64),
    Str(S),
    Arr(Vec<usize>),
    Obj(Vec<(S, usize)>),
}

enum PErr {
    Json(&'static str, usize),
    Value(String),
    Deep(&'static str),
}

fn ws(c: &[char], mut i: usize) -> usize {
    while i < c.len() && matches!(c[i], ' ' | '\t' | '\n' | '\r') {
        i += 1;
    }
    i
}

fn lit(c: &[char], i: usize, s: &str) -> bool {
    c.len() >= i + s.len() && s.chars().zip(&c[i..]).all(|(a, b)| a == *b)
}

fn hex4(c: &[char]) -> Option<u32> {
    c.iter()
        .try_fold(0u32, |acc, ch| Some(acc << 4 | ch.to_digit(16)?))
}

/// `scanstring_unicode`: `end` is just past the opening quote.
fn scanstring(c: &[char], mut end: usize) -> Result<(S, usize), PErr> {
    let begin = end - 1;
    let len = c.len();
    let mut out = S::new();
    loop {
        let mut next = end;
        while next < len && c[next] != '"' && c[next] != '\\' {
            if (c[next] as u32) <= 0x1f {
                return Err(PErr::Json("Invalid control character at", next));
            }
            next += 1;
        }
        if next == len {
            return Err(PErr::Json("Unterminated string starting at", begin));
        }
        out.extend(c[end..next].iter().map(|&ch| ch as u32));
        let quote = c[next] == '"';
        next += 1;
        if quote {
            return Ok((out, next));
        }
        if next == len {
            return Err(PErr::Json("Unterminated string starting at", begin));
        }
        if c[next] != 'u' {
            end = next + 1;
            out.push(match c[next] {
                '"' => 0x22,
                '\\' => 0x5c,
                '/' => 0x2f,
                'b' => 0x08,
                'f' => 0x0c,
                'n' => 0x0a,
                'r' => 0x0d,
                't' => 0x09,
                _ => return Err(PErr::Json("Invalid \\escape", end - 2)),
            });
            continue;
        }
        next += 1;
        end = next + 4;
        if end >= len {
            return Err(PErr::Json("Invalid \\uXXXX escape", next - 1));
        }
        let mut cp = hex4(&c[next..end]).ok_or(PErr::Json("Invalid \\uXXXX escape", end - 5))?;
        if (0xd800..0xdc00).contains(&cp) && end + 6 < len && c[end] == '\\' && c[end + 1] == 'u' {
            end += 6;
            let lo = hex4(&c[end - 4..end]).ok_or(PErr::Json("Invalid \\uXXXX escape", end - 5))?;
            if (0xdc00..0xe000).contains(&lo) {
                cp = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
            } else {
                end -= 6;
            }
        }
        out.push(cp);
    }
}

/// `_match_number_unicode`; `Err(Json("Expecting value", start))` stands for StopIteration.
fn number(c: &[char], start: usize) -> Result<(Val, usize), PErr> {
    let digit = |i: usize| c.get(i).is_some_and(|ch| ch.is_ascii_digit());
    let stop = || PErr::Json("Expecting value", start);
    let end_idx = c.len() - 1;
    let mut idx = start;
    if c[idx] == '-' {
        idx += 1;
        if idx > end_idx {
            return Err(stop());
        }
    }
    match c[idx] {
        '1'..='9' => {
            idx += 1;
            while digit(idx) {
                idx += 1;
            }
        }
        '0' => idx += 1,
        _ => return Err(stop()),
    }
    let mut is_float = false;
    if idx < end_idx && c[idx] == '.' && digit(idx + 1) {
        is_float = true;
        idx += 2;
        while digit(idx) {
            idx += 1;
        }
    }
    if idx < end_idx && matches!(c[idx], 'e' | 'E') {
        let e_start = idx;
        idx += 1;
        if idx < end_idx && matches!(c[idx], '-' | '+') {
            idx += 1;
        }
        while digit(idx) {
            idx += 1;
        }
        if digit(idx - 1) {
            is_float = true;
        } else {
            idx = e_start;
        }
    }
    let text: String = c[start..idx].iter().collect();
    if is_float {
        return Ok((Val::Float(parse_float(&text)), idx));
    }
    let (neg, mag) = match text.strip_prefix('-') {
        Some(m) => (m != "0", m.to_string()),
        None => (false, text),
    };
    if mag.len() > MAX_INT_DIGITS {
        return Err(PErr::Value(format!(
            "Exceeds the limit ({MAX_INT_DIGITS} digits) for integer string conversion: value has {} digits; use sys.set_int_max_str_digits() to increase the limit",
            mag.len()
        )));
    }
    Ok((Val::Int(Int { neg, mag }), idx))
}

/// `float(text)` for a JSON float literal. Rust's parser saturates the exponent near 65536, so
/// the digits and exponent are first folded into at most 801 significant digits (a sticky `1`
/// stands for a dropped nonzero tail; f64 needs at most 768) and a small exponent, with
/// out-of-range magnitudes decided here as inf or 0.
fn parse_float(text: &str) -> f64 {
    let (neg, t) = text.strip_prefix('-').map_or((false, text), |t| (true, t));
    let (mant, exp) = t.split_once(['e', 'E']).unwrap_or((t, "0"));
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let (eneg, edigits) = match exp.as_bytes()[0] {
        b'-' => (true, &exp[1..]),
        b'+' => (false, &exp[1..]),
        _ => (false, exp),
    };
    let edigits = edigits.trim_start_matches('0');
    // |exponent| beyond 10^17 is decided by its sign alone: mantissas are under 10^7 digits
    let e: i64 = if edigits.len() > 17 {
        1 << 60
    } else {
        edigits.parse().unwrap_or(0)
    };
    let mut e = if eneg { -e } else { e } - frac.len() as i64;
    let all = format!("{int}{frac}");
    let trimmed = all.trim_end_matches('0');
    e += (all.len() - trimmed.len()) as i64;
    let mut digits = trimmed.trim_start_matches('0').to_string();
    let sign = if neg { -1.0 } else { 1.0 };
    if digits.is_empty() || digits.len() as i64 + e < -330 {
        return sign * 0.0;
    }
    if digits.len() as i64 + e > 310 {
        return sign * f64::INFINITY;
    }
    if digits.len() > 801 {
        e += digits.len() as i64 - 801;
        digits.truncate(800);
        digits.push('1');
    }
    sign * format!("{digits}e{e}").parse::<f64>().unwrap()
}

/// `"key"` + ws + `:` + ws; returns the key and the index of the value.
fn key_colon(c: &[char], i: usize) -> Result<(S, usize), PErr> {
    if c.get(i) != Some(&'"') {
        return Err(PErr::Json(
            "Expecting property name enclosed in double quotes",
            i,
        ));
    }
    let (k, j) = scanstring(c, i + 1)?;
    let j = ws(c, j);
    if c.get(j) != Some(&':') {
        return Err(PErr::Json("Expecting ':' delimiter", j));
    }
    Ok((k, ws(c, j + 1)))
}

const NON_FINITE: [&str; 3] = ["NaN", "Infinity", "-Infinity"];

/// `json.loads(text, object_pairs_hook=_unique_object, parse_constant=<raise>)`.
fn parse(c: &[char]) -> Result<(Vec<Val>, usize), PErr> {
    enum Frame {
        Arr(Vec<usize>),
        Obj(Vec<(S, usize)>, S),
    }
    if c.first() == Some(&'\u{feff}') {
        return Err(PErr::Json(
            "Unexpected UTF-8 BOM (decode using utf-8-sig)",
            0,
        ));
    }
    let mut vals: Vec<Val> = Vec::new();
    let mut push = |v: Val| {
        vals.push(v);
        vals.len() - 1
    };
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = ws(c, 0);
    let root;
    'value: loop {
        let mut v = match c.get(i).copied() {
            None => return Err(PErr::Json("Expecting value", i)),
            Some('"') => {
                let (s, j) = scanstring(c, i + 1)?;
                i = j;
                push(Val::Str(s))
            }
            Some(open @ ('{' | '[')) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(PErr::Deep(if open == '{' { "object" } else { "array" }));
                }
                i = ws(c, i + 1);
                if open == '[' {
                    if c.get(i) != Some(&']') {
                        stack.push(Frame::Arr(Vec::new()));
                        continue 'value;
                    }
                    i += 1;
                    push(Val::Arr(Vec::new()))
                } else {
                    if c.get(i) != Some(&'}') {
                        let (k, j) = key_colon(c, i)?;
                        i = j;
                        stack.push(Frame::Obj(Vec::new(), k));
                        continue 'value;
                    }
                    i += 1;
                    push(Val::Obj(Vec::new()))
                }
            }
            Some('n') if lit(c, i, "null") => {
                i += 4;
                push(Val::Null)
            }
            Some('t') if lit(c, i, "true") => {
                i += 4;
                push(Val::Bool)
            }
            Some('f') if lit(c, i, "false") => {
                i += 5;
                push(Val::Bool)
            }
            Some(_) if NON_FINITE.iter().any(|k| lit(c, i, k)) => {
                let name = NON_FINITE.iter().find(|k| lit(c, i, k)).unwrap();
                return Err(PErr::Value(format!("non-finite JSON number {name}")));
            }
            Some(_) => {
                let (val, j) = number(c, i)?;
                i = j;
                push(val)
            }
        };
        loop {
            match stack.last_mut() {
                None => {
                    root = v;
                    break 'value;
                }
                Some(Frame::Arr(items)) => {
                    items.push(v);
                    i = ws(c, i);
                    if c.get(i) == Some(&']') {
                        i += 1;
                        let Some(Frame::Arr(items)) = stack.pop() else {
                            unreachable!()
                        };
                        v = push(Val::Arr(items));
                        continue;
                    }
                    if c.get(i) != Some(&',') {
                        return Err(PErr::Json("Expecting ',' delimiter", i));
                    }
                    let comma = i;
                    i = ws(c, i + 1);
                    if c.get(i) == Some(&']') {
                        return Err(PErr::Json(
                            "Illegal trailing comma before end of array",
                            comma,
                        ));
                    }
                    continue 'value;
                }
                Some(Frame::Obj(pairs, key)) => {
                    pairs.push((std::mem::take(key), v));
                    i = ws(c, i);
                    if c.get(i) == Some(&'}') {
                        i += 1;
                        let Some(Frame::Obj(pairs, _)) = stack.pop() else {
                            unreachable!()
                        };
                        let mut seen = HashSet::new();
                        if let Some((k, _)) = pairs.iter().find(|(k, _)| !seen.insert(k)) {
                            return Err(PErr::Value(format!(
                                "duplicate object key: {}",
                                py_repr(k)
                            )));
                        }
                        v = push(Val::Obj(pairs));
                        continue;
                    }
                    if c.get(i) != Some(&',') {
                        return Err(PErr::Json("Expecting ',' delimiter", i));
                    }
                    let comma = i;
                    i = ws(c, i + 1);
                    if c.get(i) == Some(&'}') {
                        return Err(PErr::Json(
                            "Illegal trailing comma before end of object",
                            comma,
                        ));
                    }
                    let (k, j) = key_colon(c, i)?;
                    *key = k;
                    i = j;
                    continue 'value;
                }
            }
        }
    }
    let end = ws(c, i);
    if end != c.len() {
        return Err(PErr::Json("Extra data", end));
    }
    Ok((vals, root))
}

// ---------- Python str helpers ----------

/// `str.isprintable()` for one non-ASCII code point.
fn printable(cp: u32) -> bool {
    static NP: OnceLock<Regex> = OnceLock::new();
    let re = NP.get_or_init(|| Regex::new(r"[\p{C}\p{Z}]").unwrap());
    char::from_u32(cp).is_some_and(|ch| !re.is_match(ch.encode_utf8(&mut [0; 4])))
}

/// `repr(str)`.
fn py_repr(s: &[u32]) -> String {
    let q = if s.contains(&0x27) && !s.contains(&0x22) {
        0x22
    } else {
        0x27
    };
    let mut out = String::new();
    out.push(q as u8 as char);
    for &c in s {
        match c {
            _ if c == q || c == 0x5c => {
                out.push('\\');
                out.push(c as u8 as char);
            }
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0..0x20 | 0x7f => out.push_str(&format!("\\x{c:02x}")),
            0x20..0x7f => out.push(c as u8 as char),
            _ if printable(c) => out.push(char::from_u32(c).unwrap()),
            0x80..=0xff => out.push_str(&format!("\\x{c:02x}")),
            0x100..=0xffff => out.push_str(&format!("\\u{c:04x}")),
            _ => out.push_str(&format!("\\U{c:08x}")),
        }
    }
    out.push(q as u8 as char);
    out
}

/// `str(UnicodeDecodeError)` for `bytes.decode("utf-8")`.
fn utf8_error(raw: &[u8], e: std::str::Utf8Error) -> String {
    let start = e.valid_up_to();
    let (end, reason) = match e.error_len() {
        None => (raw.len(), "unexpected end of data"),
        Some(1) if !(0xc2..=0xf4).contains(&raw[start]) => (start + 1, "invalid start byte"),
        Some(n) => (start + n, "invalid continuation byte"),
    };
    if end - start == 1 {
        format!(
            "'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            raw[start]
        )
    } else {
        format!(
            "'utf-8' codec can't decode bytes in position {start}-{}: {reason}",
            end - 1
        )
    }
}

/// `str(float)` (shortest round-trip, Python's exponent thresholds). Rust's shortest form breaks
/// a tie between two equally near shortest strings upward; repr takes the correctly rounded
/// (half-even) one, which exact formatting at the same digit count gives.
fn py_float(x: f64) -> String {
    let shortest = format!("{x:e}");
    let n = shortest
        .split_once('e')
        .unwrap()
        .0
        .matches(|c: char| c.is_ascii_digit())
        .count();
    // half-even at the shortest length matches Python's tie rule, but at a power of two the
    // rounding interval is uneven and that string may not read back: keep Rust's then
    let exact = format!("{x:.*e}", n - 1);
    let s = if exact.parse::<f64>() == Ok(x) {
        exact
    } else {
        shortest
    };
    let (mant, exp) = s.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let (sign, mant) = match mant.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mant),
    };
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let decpt = exp + 1;
    let body = if decpt <= -4 || decpt > 16 {
        let m = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    } else if decpt <= 0 {
        format!("0.{}{digits}", "0".repeat((-decpt) as usize))
    } else if decpt as usize >= digits.len() {
        format!("{digits}{}.0", "0".repeat(decpt as usize - digits.len()))
    } else {
        let p = decpt as usize;
        format!("{}.{}", &digits[..p], &digits[p..])
    };
    format!("{sign}{body}")
}

/// `math.fsum` (CPython's msum with half-even correction); None on overflow.
fn fsum(xs: impl Iterator<Item = f64>) -> Option<f64> {
    let mut p: Vec<f64> = Vec::new();
    for mut x in xs {
        let mut i = 0;
        for j in 0..p.len() {
            let mut y = p[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let lo = y - (hi - x);
            if lo != 0.0 {
                p[i] = lo;
                i += 1;
            }
            x = hi;
        }
        p.truncate(i);
        if x != 0.0 {
            if !x.is_finite() {
                return None; // inputs are finite: "intermediate overflow in fsum"
            }
            p.push(x);
        }
    }
    let mut n = p.len();
    let mut hi = 0.0;
    if n > 0 {
        n -= 1;
        hi = p[n];
        let mut lo = 0.0;
        while n > 0 {
            let x = hi;
            n -= 1;
            let y = p[n];
            hi = x + y;
            lo = y - (hi - x);
            if lo != 0.0 {
                break;
            }
        }
        if n > 0 && ((lo < 0.0 && p[n - 1] < 0.0) || (lo > 0.0 && p[n - 1] > 0.0)) {
            let y = lo * 2.0;
            let x = hi + y;
            if y == x - hi {
                hi = x;
            }
        }
    }
    Some(hi)
}

// ---------- output: json.dumps ----------

enum J {
    Null,
    Bool(bool),
    Num(String),
    Str(S),
    Arr(Vec<J>),
    Obj(Vec<(S, J)>),
}

fn js(s: &str) -> J {
    J::Str(u(s))
}

fn obj(pairs: Vec<(&str, J)>) -> J {
    J::Obj(pairs.into_iter().map(|(k, v)| (u(k), v)).collect())
}

fn float_or_null(x: Option<f64>) -> J {
    x.map_or(J::Null, |x| J::Num(py_float(x)))
}

/// `ensure_ascii=True` string encoding.
fn esc(s: &[u32], out: &mut String) {
    out.push('"');
    for &c in s {
        match c {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x20..=0x7e => out.push(c as u8 as char),
            0..0x10000 => out.push_str(&format!("\\u{c:04x}")),
            _ => {
                let v = c - 0x10000;
                out.push_str(&format!(
                    "\\u{:04x}\\u{:04x}",
                    0xd800 | (v >> 10),
                    0xdc00 | (v & 0x3ff)
                ));
            }
        }
    }
    out.push('"');
}

/// `json.dumps(j, indent=indent)`; without indent the separators are ", " and ": ".
fn dump(j: &J, indent: Option<usize>, level: usize, out: &mut String) {
    let nl = |out: &mut String, lv: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(n * lv));
        }
    };
    let sep = if indent.is_some() { "," } else { ", " };
    match j {
        J::Null => out.push_str("null"),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Num(n) => out.push_str(n),
        J::Str(s) => esc(s, out),
        J::Arr(items) if items.is_empty() => out.push_str("[]"),
        J::Obj(items) if items.is_empty() => out.push_str("{}"),
        J::Arr(items) => {
            out.push('[');
            for (k, v) in items.iter().enumerate() {
                if k > 0 {
                    out.push_str(sep);
                }
                nl(out, level + 1);
                dump(v, indent, level + 1, out);
            }
            nl(out, level);
            out.push(']');
        }
        J::Obj(items) => {
            out.push('{');
            for (k, (key, v)) in items.iter().enumerate() {
                if k > 0 {
                    out.push_str(sep);
                }
                nl(out, level + 1);
                esc(key, out);
                out.push_str(": ");
                dump(v, indent, level + 1, out);
            }
            nl(out, level);
            out.push('}');
        }
    }
}

// ---------- validate ----------

struct Doc {
    v: Vec<Val>,
}

type Errors = Vec<(String, S)>;

fn err(e: &mut Errors, path: &str, msg: &str) {
    e.push((path.to_string(), u(msg)));
}

impl Doc {
    fn obj(&self, i: usize) -> Option<&[(S, usize)]> {
        match &self.v[i] {
            Val::Obj(p) => Some(p),
            _ => None,
        }
    }
    fn arr(&self, i: usize) -> Option<&[usize]> {
        match &self.v[i] {
            Val::Arr(a) => Some(a),
            _ => None,
        }
    }
    fn str(&self, i: usize) -> Option<&[u32]> {
        match &self.v[i] {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
    /// `type(x) is int` (bool excluded).
    fn int(&self, i: usize) -> Option<&Int> {
        match &self.v[i] {
            Val::Int(n) => Some(n),
            _ => None,
        }
    }
    fn find(&self, o: usize, k: &str) -> Option<usize> {
        let k = u(k);
        self.obj(o)?.iter().find(|(key, _)| *key == k).map(|p| p.1)
    }
    fn get(&self, o: usize, k: &str) -> usize {
        self.find(o, k).unwrap()
    }
    fn str_in(&self, i: usize, options: &[&str]) -> bool {
        self.str(i)
            .is_some_and(|s| options.iter().any(|o| u(o) == s))
    }

    /// `_object`: exact field set; returns whether it matched.
    fn object(&self, i: usize, path: &str, fields: &[&str], e: &mut Errors) -> bool {
        let Some(pairs) = self.obj(i) else {
            err(e, path, "must be an object");
            return false;
        };
        let keys: HashSet<&[u32]> = pairs.iter().map(|(k, _)| k.as_slice()).collect();
        let want: Vec<S> = fields.iter().map(|f| u(f)).collect();
        let mut missing: Vec<&str> = fields
            .iter()
            .zip(&want)
            .filter(|(_, w)| !keys.contains(w.as_slice()))
            .map(|(f, _)| *f)
            .collect();
        let mut unknown: Vec<&[u32]> = keys
            .into_iter()
            .filter(|k| !want.iter().any(|w| w == k))
            .collect();
        missing.sort_unstable();
        unknown.sort_unstable();
        if !missing.is_empty() {
            err(e, path, &format!("missing fields: {}", missing.join(", ")));
        }
        if !unknown.is_empty() {
            let mut msg = u("unknown fields: ");
            for (k, key) in unknown.iter().enumerate() {
                if k > 0 {
                    msg.extend(u(", "));
                }
                msg.extend_from_slice(key);
            }
            e.push((path.to_string(), msg));
        }
        missing.is_empty() && unknown.is_empty()
    }

    /// `_text`: a str with at least one non-whitespace character.
    fn text(&self, i: usize, path: &str, e: &mut Errors) -> bool {
        let ok = self
            .str(i)
            .is_some_and(|s| s.iter().any(|&c| !char::from_u32(c).is_some_and(py_space)));
        if !ok {
            err(e, path, "must be a nonempty string");
        }
        ok
    }
}

fn hex64(s: &[u32]) -> bool {
    s.len() == 64
        && s.iter()
            .all(|&c| char::from_u32(c).is_some_and(|c| c.is_ascii_hexdigit()))
}

/// DATE regex + `date.fromisoformat` (which only accepts ASCII digits).
fn iso_date(s: &[u32]) -> bool {
    let Ok(t) = String::from_utf16(&s.iter().map(|&c| c as u16).collect::<Vec<_>>()) else {
        return false;
    };
    let b = t.as_bytes();
    if s.iter().any(|&c| c > 0x7f) || b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    if !b
        .iter()
        .enumerate()
        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return false;
    }
    let (y, m, d): (u32, u32, u32) = (
        t[..4].parse().unwrap(),
        t[5..7].parse().unwrap(),
        t[8..].parse().unwrap(),
    );
    y >= 1 && (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)
}

fn validate(d: &Doc, root: usize) -> Errors {
    let mut e = Errors::new();
    if d.obj(root).is_none() {
        err(&mut e, "$", "must be an object");
        return e;
    }
    d.object(root, "$", &ROOT_FIELDS, &mut e);
    if ROOT_FIELDS.iter().any(|f| d.find(root, f).is_none()) {
        return e;
    }
    let g = |f| d.get(root, f);
    if d.str(g("schema")) != Some(u("dao-assessment/v1").as_slice()) {
        err(&mut e, "$.schema", "must equal dao-assessment/v1");
    }

    let identity = g("identity");
    if d.object(identity, "$.identity", &IDENTITY_FIELDS, &mut e) {
        for f in ["skill", "model", "host", "rubric"] {
            d.text(d.get(identity, f), &format!("$.identity.{f}"), &mut e);
        }
        for f in ["skill_sha256", "task_set_sha256"] {
            if !d.str(d.get(identity, f)).is_some_and(hex64) {
                err(
                    &mut e,
                    &format!("$.identity.{f}"),
                    "must be a 64-character SHA-256 hex string",
                );
            }
        }
        if !d.str(d.get(identity, "date")).is_some_and(iso_date) {
            err(
                &mut e,
                "$.identity.date",
                "must be a valid ISO date YYYY-MM-DD",
            );
        }
    }

    let mut arm_set: HashSet<&[u32]> = HashSet::new();
    let arms: &[usize] = match d.arr(g("arms")) {
        None => {
            err(&mut e, "$.arms", "must be an array");
            &[]
        }
        Some(arms) => {
            for (i, &a) in arms.iter().enumerate() {
                d.text(a, &format!("$.arms[{i}]"), &mut e);
            }
            if arms.is_empty() {
                err(&mut e, "$.arms", "must contain at least one arm");
            } else if arms.len() > MAX_ARMS {
                err(
                    &mut e,
                    "$.arms",
                    &format!("must contain at most {MAX_ARMS} arms"),
                );
            }
            arm_set = arms.iter().filter_map(|&a| d.str(a)).collect();
            if arms.len() != arm_set.len() {
                err(&mut e, "$.arms", "contains duplicate arms");
            }
            arms
        }
    };

    let reps = d.int(g("repetitions"));
    if !reps.is_some_and(Int::positive) {
        err(
            &mut e,
            "$.repetitions",
            "must be a positive integer (boolean is not allowed)",
        );
    }

    let cases: &[usize] = match d.arr(g("cases")) {
        None => {
            err(&mut e, "$.cases", "must be an array");
            &[]
        }
        Some(c) => {
            if c.is_empty() {
                err(&mut e, "$.cases", "must contain at least one case");
            }
            c
        }
    };
    if let Some(r) = reps.filter(|r| r.positive()) {
        let planned = mag_mul(&r.mag, arms.len() as u128 * cases.len() as u128);
        if mag_cmp(&planned, MAX_PLANNED_ROWS) == Ordering::Greater {
            err(
                &mut e,
                "$",
                &format!("expanded plan has {planned} rows; maximum is {MAX_PLANNED_ROWS}"),
            );
        }
    }
    let mut case_ids: HashSet<&[u32]> = HashSet::new();
    for (i, &c) in cases.iter().enumerate() {
        let path = format!("$.cases[{i}]");
        if !d.object(c, &path, &["id", "kind"], &mut e) {
            continue;
        }
        let id = d.get(c, "id");
        if d.text(id, &format!("{path}.id"), &mut e) && !case_ids.insert(d.str(id).unwrap()) {
            err(&mut e, &format!("{path}.id"), "duplicate case id");
        }
        if !d.str_in(d.get(c, "kind"), &KINDS) {
            err(
                &mut e,
                &format!("{path}.kind"),
                "must be one of positive, boundary, nontrigger",
            );
        }
    }

    let iso = g("isolation");
    if d.object(iso, "$.isolation", &["status", "evidence"], &mut e) {
        if !d.str_in(d.get(iso, "status"), &["PASS", "FAIL", "UNKNOWN"]) {
            err(
                &mut e,
                "$.isolation.status",
                "must be PASS, FAIL, or UNKNOWN",
            );
        }
        d.text(d.get(iso, "evidence"), "$.isolation.evidence", &mut e);
    }

    let runs: &[usize] = d.arr(g("runs")).unwrap_or_else(|| {
        err(&mut e, "$.runs", "must be an array");
        &[]
    });
    let mut run_keys: HashSet<(&[u32], &[u32], &Int)> = HashSet::new();
    for (i, &r) in runs.iter().enumerate() {
        let path = format!("$.runs[{i}]");
        if !d.object(r, &path, &RUN_FIELDS, &mut e) {
            continue;
        }
        let (cid, arm) = (d.get(r, "case_id"), d.get(r, "arm"));
        let case_ok = d.text(cid, &format!("{path}.case_id"), &mut e);
        let arm_ok = d.text(arm, &format!("{path}.arm"), &mut e);
        if case_ok && !case_ids.contains(d.str(cid).unwrap()) {
            err(&mut e, &format!("{path}.case_id"), "unknown case id");
        }
        if arm_ok && !arm_set.contains(d.str(arm).unwrap()) {
            err(&mut e, &format!("{path}.arm"), "unknown arm");
        }
        let rep = d.int(d.get(r, "repetition"));
        let rep_ok = match (rep, reps) {
            (Some(x), Some(n)) => {
                x.positive() && !n.neg && mag_cmp(&x.mag, &n.mag) != Ordering::Greater
            }
            _ => false,
        };
        if !rep_ok {
            err(
                &mut e,
                &format!("{path}.repetition"),
                "must be an integer from 1 through repetitions",
            );
        }
        if case_ok
            && arm_ok
            && rep_ok
            && !run_keys.insert((d.str(cid).unwrap(), d.str(arm).unwrap(), rep.unwrap()))
        {
            err(&mut e, &path, "duplicate case_id/arm/repetition run");
        }
        if !d.str_in(d.get(r, "status"), &STATUSES) {
            err(
                &mut e,
                &format!("{path}.status"),
                "must be one of PASS, FAIL, UNKNOWN, NOT_RUN, INVALID",
            );
        }
        if !d.str_in(d.get(r, "trust"), &TRUST) {
            err(
                &mut e,
                &format!("{path}.trust"),
                "must be one of PASS, CONDITIONAL, FAIL, UNKNOWN, N/A",
            );
        }
        d.text(d.get(r, "evidence"), &format!("{path}.evidence"), &mut e);
        let cost = d.get(r, "cost");
        if matches!(d.v[cost], Val::Null)
            || !d.object(cost, &format!("{path}.cost"), &COST_FIELDS, &mut e)
        {
            continue;
        }
        for f in COST_FIELDS {
            let integer = f != "seconds";
            let ok = match &d.v[d.get(cost, f)] {
                Val::Null => continue,
                Val::Int(n) => !n.neg && (integer || mag_cmp(&n.mag, DBL_MAX) != Ordering::Greater),
                Val::Float(x) => !integer && *x >= 0.0 && x.is_finite(),
                _ => false,
            };
            if !ok {
                let kind = if integer {
                    "nonnegative integer or null"
                } else {
                    "finite nonnegative number or null"
                };
                err(
                    &mut e,
                    &format!("{path}.cost.{f}"),
                    &format!("must be a {kind} (boolean is not allowed)"),
                );
            }
        }
    }
    e
}

// ---------- summarize (valid input only) ----------

struct Row {
    kind: usize,
    status: usize,
    trust: usize,
    cost: Option<usize>,
}

fn index_of(d: &Doc, i: usize, options: &[&str]) -> usize {
    let s = d.str(i).unwrap();
    options.iter().position(|o| u(o) == s).unwrap()
}

fn counter(d: &Doc, rows: &[&Row]) -> J {
    let planned = rows.len();
    let mut counts = [0usize; 5];
    for r in rows {
        counts[r.status] += 1;
    }
    let assessed = counts[0] + counts[1];
    let mut costs = Vec::new();
    for f in COST_FIELDS {
        let values: Vec<usize> = rows
            .iter()
            .filter_map(|r| r.cost)
            .map(|c| d.get(c, f))
            .filter(|&v| !matches!(d.v[v], Val::Null))
            .collect();
        let mut total = J::Null;
        let mut reason = Some(if planned == 0 {
            "no planned rows"
        } else {
            "missing planned values"
        });
        if planned > 0 && values.len() == planned {
            reason = None;
            if f != "seconds" {
                let sum = values
                    .iter()
                    .fold("0".to_string(), |acc, &v| match &d.v[v] {
                        Val::Int(n) => mag_add(&acc, &n.mag),
                        _ => unreachable!(),
                    });
                total = J::Num(sum);
            } else {
                let xs = values.iter().map(|&v| match &d.v[v] {
                    Val::Int(n) => n.mag.parse::<f64>().unwrap(),
                    Val::Float(x) => *x,
                    _ => unreachable!(),
                });
                match fsum(xs).filter(|x| x.is_finite()) {
                    Some(x) => total = J::Num(py_float(x)),
                    None => reason = Some("sum is non-finite"),
                }
            }
        }
        costs.push((
            f,
            obj(vec![
                ("sum", total),
                ("known_values", J::Num(values.len().to_string())),
                ("unavailable_reason", reason.map_or(J::Null, js)),
            ]),
        ));
    }
    let ratio = |a: usize, b: usize| float_or_null((b > 0).then(|| a as f64 / b as f64));
    obj(vec![
        ("planned", J::Num(planned.to_string())),
        (
            "counts",
            obj(STATUSES
                .iter()
                .zip(counts)
                .map(|(s, n)| (*s, J::Num(n.to_string())))
                .collect()),
        ),
        ("coverage", ratio(assessed, planned)),
        ("conditional_pass", ratio(counts[0], assessed)),
        ("cost", obj(costs)),
    ])
}

fn summarize(d: &Doc, root: usize) -> J {
    let g = |f| d.get(root, f);
    let arms: Vec<&[u32]> = d
        .arr(g("arms"))
        .unwrap()
        .iter()
        .map(|&a| d.str(a).unwrap())
        .collect();
    let cases = d.arr(g("cases")).unwrap();
    let reps: usize = d.int(g("repetitions")).unwrap().mag.parse().unwrap(); // ≤ 100000 once valid
    let mut by_key: HashMap<(&[u32], &[u32], usize), usize> = HashMap::new();
    for &r in d.arr(g("runs")).unwrap() {
        let rep = d.int(d.get(r, "repetition")).unwrap().mag.parse().unwrap();
        by_key.insert(
            (
                d.str(d.get(r, "case_id")).unwrap(),
                d.str(d.get(r, "arm")).unwrap(),
                rep,
            ),
            r,
        );
    }
    // rows in (case, arm, repetition) order: index = (case * arms + arm) * reps + rep - 1
    let mut rows = Vec::with_capacity(cases.len() * arms.len() * reps);
    for &c in cases {
        let id = d.str(d.get(c, "id")).unwrap();
        let kind = index_of(d, d.get(c, "kind"), &KINDS);
        for arm in &arms {
            for rep in 1..=reps {
                rows.push(match by_key.get(&(id, *arm, rep)) {
                    Some(&r) => Row {
                        kind,
                        status: index_of(d, d.get(r, "status"), &STATUSES),
                        trust: index_of(d, d.get(r, "trust"), &TRUST),
                        cost: Some(d.get(r, "cost")).filter(|&c| !matches!(d.v[c], Val::Null)),
                    },
                    None => Row {
                        kind,
                        status: 3,
                        trust: 3,
                        cost: None,
                    },
                });
            }
        }
    }
    let mut arm_out = Vec::new();
    for (ai, arm) in arms.iter().enumerate() {
        let arm_rows: Vec<&Row> = rows
            .iter()
            .enumerate()
            .filter(|(i, _)| (i / reps) % arms.len() == ai)
            .map(|(_, r)| r)
            .collect();
        let by_kind = (0..KINDS.len())
            .map(|k| {
                let kr: Vec<&Row> = arm_rows.iter().copied().filter(|r| r.kind == k).collect();
                (KINDS[k], counter(d, &kr))
            })
            .collect();
        arm_out.push((
            arm.to_vec(),
            obj(vec![
                ("overall", counter(d, &arm_rows)),
                ("by_kind", obj(by_kind)),
            ]),
        ));
    }
    let any = !rows.is_empty();
    let data_complete = any && rows.iter().all(|r| r.status <= 1);
    let trust_clear = any && rows.iter().all(|r| r.trust == 0 || r.trust == 4);
    let identity = d
        .obj(g("identity"))
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), J::Str(d.str(*v).unwrap().to_vec())))
        .collect();
    let mut out = vec![
        ("schema", js("dao-assessment-summary/v1")),
        ("identity", J::Obj(identity)),
        ("data_complete", J::Bool(data_complete)),
        ("trust_clear", J::Bool(trust_clear)),
        (
            "gate",
            js(if data_complete && trust_clear {
                "COMPLETE"
            } else {
                "INCOMPLETE_OR_BLOCKED"
            }),
        ),
        (
            "all_tasks_passed",
            J::Bool(any && rows.iter().all(|r| r.status == 0)),
        ),
        ("arms", J::Obj(arm_out)),
    ];
    if arms.len() == 2 {
        let mut pairs = Vec::new();
        for ci in 0..cases.len() {
            for rep in 0..reps {
                let (a, b) = (
                    &rows[(ci * 2) * reps + rep],
                    &rows[(ci * 2 + 1) * reps + rep],
                );
                if a.status <= 1 && b.status <= 1 {
                    pairs.push((a.status, b.status));
                }
            }
        }
        let planned = cases.len() * reps;
        let wins = pairs.iter().filter(|p| **p == (1, 0)).count();
        let losses = pairs.iter().filter(|p| **p == (0, 1)).count();
        let full = pairs.len() == planned;
        let isolation_pass = d.str_in(d.get(g("isolation"), "status"), &["PASS"]);
        let reason = if planned == 0 {
            Some("requires at least one planned pair")
        } else if !full {
            Some("requires full paired PASS/FAIL coverage")
        } else if !isolation_pass {
            Some("requires isolation PASS")
        } else {
            None
        };
        let delta = (full && isolation_pass && planned > 0).then(|| {
            let b = pairs.iter().filter(|p| p.1 == 0).count() as f64;
            let a = pairs.iter().filter(|p| p.0 == 0).count() as f64;
            (b - a) / planned as f64 * 100.0
        });
        let n = |x: usize| J::Num(x.to_string());
        out.push((
            "pairwise",
            obj(vec![
                ("reference_arm", J::Str(arms[0].to_vec())),
                ("comparison_arm", J::Str(arms[1].to_vec())),
                ("distinct_cases", n(cases.len())),
                ("planned_pairs", n(planned)),
                ("comparable_pairs", n(pairs.len())),
                ("wins", n(wins)),
                ("ties", n(pairs.len() - wins - losses)),
                ("losses", n(losses)),
                ("delta_pp", float_or_null(delta)),
                ("delta_unavailable_reason", reason.map_or(J::Null, js)),
            ]),
        ));
    }
    obj(out)
}

// ---------- CLI ----------

fn input_error(msg: S) -> (String, u8) {
    let j = obj(vec![
        ("status", js("input_error")),
        (
            "errors",
            J::Arr(vec![obj(vec![("path", js("$")), ("message", J::Str(msg))])]),
        ),
    ]);
    let mut out = String::new();
    dump(&j, None, 0, &mut out);
    (out, 2)
}

fn read_input(path: &[u8]) -> Result<Vec<u8>, String> {
    // Python's OSError text: "[Errno N] message", plus ": 'path'" for errors from open()
    let errno = |e: &std::io::Error| {
        let text = e.to_string();
        let text = text
            .rsplit_once(" (os error ")
            .map_or(text.as_str(), |t| t.0)
            .to_string();
        format!("[Errno {}] {text}", e.raw_os_error().unwrap_or(0))
    };
    let os_err = |e: std::io::Error| format!("{}: {}", errno(&e), py_repr(&surrogateescape(path)));
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open(OsStr::from_bytes(path))
        .map_err(os_err)?;
    if !f.metadata().map_err(os_err)?.is_file() {
        return Err("input must be a regular file".into());
    }
    let mut raw = Vec::new();
    (&mut f)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut raw)
        // Python's stream.read() error carries no filename, unlike open()'s
        .map_err(|e| errno(&e))?;
    if raw.len() as u64 > MAX_BYTES {
        return Err(format!("input exceeds {MAX_BYTES} bytes"));
    }
    Ok(raw)
}

/// `check_assessment.main([arg])` minus argparse: (stdout without the final newline, exit code).
pub fn assess(arg: &[u8]) -> (String, u8) {
    // pathlib's normalisation only looks at '/' and '.', so run it on a byte-per-char copy
    let latin1: String = arg.iter().map(|&b| b as char).collect();
    let path: Vec<u8> = py_path_str(&latin1).chars().map(|c| c as u8).collect();
    let raw = match read_input(&path) {
        Ok(r) => r,
        Err(m) => return input_error(u(&m)),
    };
    let text = match std::str::from_utf8(&raw) {
        Ok(t) => t,
        Err(e) => return input_error(u(&utf8_error(&raw, e))),
    };
    let chars: Vec<char> = text.chars().collect();
    let (vals, root) = match parse(&chars) {
        Ok(p) => p,
        Err(PErr::Json(msg, pos)) => {
            let before = &chars[..pos];
            let line = before.iter().filter(|c| **c == '\n').count() + 1;
            let col = pos - before.iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1) + 1;
            return input_error(u(&format!("{msg}: line {line} column {col} (char {pos})")));
        }
        Err(PErr::Value(m)) => return input_error(u(&m)),
        Err(PErr::Deep(kind)) => {
            return input_error(u(&format!(
            "maximum recursion depth exceeded while decoding a JSON {kind} from a unicode string"
        )))
        }
    };
    let doc = Doc { v: vals };
    let errors = validate(&doc, root);
    let mut out = String::new();
    if !errors.is_empty() {
        let list = errors
            .into_iter()
            .map(|(p, m)| obj(vec![("path", js(&p)), ("message", J::Str(m))]))
            .collect();
        dump(
            &obj(vec![("status", js("invalid")), ("errors", J::Arr(list))]),
            Some(2),
            0,
            &mut out,
        );
        return (out, 1);
    }
    dump(&summarize(&doc, root), Some(2), 0, &mut out);
    (out, 0)
}

const HELP: &str = "usage: dao assess [-h] input

Validate and count a dao-assessment/v1 JSON file without verifying evidence

positional arguments:
  input       assessment JSON file (maximum 10 MiB)

options:
  -h, --help  show this help message and exit
";

/// argparse's view of one argument before `--`: 'h' help (exits 0 at once), 'e' an error raised
/// at once (`-h=x`, `-h-`, `--he=x`), 'o' an unknown option, 'p' a positional.
fn arg_kind(a: &str) -> char {
    let b = a.as_bytes();
    if b.len() < 2 || b[0] != b'-' {
        return 'p';
    }
    if let Some(long) = a.strip_prefix("--") {
        let name = long.split('=').next().unwrap_or("");
        return match (
            // argparse matches even an empty name (`--=y`) as a prefix of --help
            "help".starts_with(name),
            long.contains('='),
        ) {
            (true, false) => 'h',
            (true, true) => 'e',
            _ if a.contains(' ') => 'p',
            _ => 'o',
        };
    }
    if let Some(rest) = a.strip_prefix("-h") {
        // a chain of short flags: -hh is help twice, -hx queues help before the unknown -x
        let rest = rest.trim_start_matches('h');
        return if rest.starts_with(['=', '-']) {
            'e'
        } else {
            'h'
        };
    }
    static NEG: OnceLock<Regex> = OnceLock::new(); // argparse's matcher; \d is Unicode Nd here too
    if NEG
        .get_or_init(|| Regex::new(r"^-\.?\d").unwrap())
        .is_match(a)
        || a.contains(' ')
    {
        'p'
    } else {
        'o'
    }
}

fn usage_error(why: &str) -> u8 {
    eprintln!("usage: dao assess [-h] input\ndao assess: error: {why}");
    2
}

/// Python's view of a non-UTF-8 argv: each undecodable byte becomes a U+DC80..U+DCFF surrogate.
fn surrogateescape(b: &[u8]) -> S {
    let mut out = S::new();
    for chunk in b.utf8_chunks() {
        out.extend(chunk.valid().chars().map(|c| c as u32));
        out.extend(chunk.invalid().iter().map(|&x| 0xDC00 + x as u32));
    }
    out
}

/// `dao assess [-h] input`; prints like the Python CLI and returns its exit code. Arguments stay
/// bytes (Python takes non-UTF-8 argv through surrogate escapes); options are judged on a lossy
/// copy, which only replaces bytes argparse never looks at.
pub fn main(args: &[OsString]) -> u8 {
    let (mut pos, mut bad, mut rest) = (Vec::new(), Vec::new(), false);
    for raw in args {
        let a = String::from_utf8_lossy(raw.as_bytes()).into_owned();
        if rest {
            pos.push((a, raw));
            continue;
        }
        match (a.as_str(), arg_kind(&a)) {
            ("--", _) => rest = true,
            (_, 'h') => {
                print!("{HELP}");
                return 0;
            }
            (_, 'o') => bad.push(a),
            (_, 'e') => return usage_error("argument -h/--help: ignored explicit argument"),
            _ => pos.push((a, raw)),
        }
    }
    if pos.is_empty() {
        return usage_error("the following arguments are required: input");
    }
    if pos.len() > 1 || !bad.is_empty() {
        let extra: Vec<&str> = bad
            .iter()
            .chain(pos.iter().skip(1).map(|p| &p.0))
            .map(|s| s.as_str())
            .collect();
        return usage_error(&format!("unrecognized arguments: {}", extra.join(" ")));
    }
    let (out, code) = assess(pos[0].1.as_bytes());
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{out}"); // a closed stdout is not worth a panic
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_number_and_string_formatting() {
        for (x, s) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (0.5, "0.5"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (2.0 / 3.0, "0.6666666666666666"),
            (123456789.125, "123456789.125"),
            // shortest-digit ties round half-even like repr, not up
            (1e15 + 0.25, "1000000000000000.2"),
            (2f64.powi(-24), "5.960464477539063e-08"),
            (6.189700196426902e26, "6.189700196426902e+26"),
            (580932385485918.0 + 0.25, "580932385485918.2"),
            (2f64.powi(49) + 0.25, "562949953421312.2"),
            (1394089528363410.2, "1394089528363410.2"),
        ] {
            assert_eq!(py_float(x), s);
        }
        assert_eq!(fsum([1e308, 1e308].into_iter()), None);
        assert_eq!(fsum([0.1, 0.2, 0.3].into_iter()), Some(0.6));
        assert_eq!(fsum([1e100, 1.0, -1e100].into_iter()), Some(1.0));
        assert_eq!(mag_add("999", "1"), "1000");
        assert_eq!(mag_mul("123456789", 1000), "123456789000");
        assert_eq!(py_repr(&u("it's")), "\"it's\"");
        assert_eq!(
            py_repr(&[0x27, 0x22, 0xd800, 0xe9, 0xad, 0x1f600]),
            "'\\'\"\\ud800é\\xad😀'"
        );
    }

    #[test]
    fn long_float_literals_parse_like_python_float() {
        let f = |t: String| match number(&format!("{t} ").chars().collect::<Vec<_>>(), 0) {
            Ok((Val::Float(x), _)) => x,
            _ => panic!("not a float: {t}"),
        };
        let z = "0".repeat(700_000);
        assert_eq!(f(format!("0.{z}1e700401")), f64::INFINITY);
        assert_eq!(f(format!("-0.{z}1e700401")), f64::NEG_INFINITY);
        assert_eq!(f(format!("0.{z}1e700001")), 1.0);
        assert_eq!(f(format!("1{z}e-700000")), 1.0);
        assert_eq!(f(format!("1{z}.5e-700401")), 0.0);
        assert_eq!(f(format!("1.5e{}", "9".repeat(30))), f64::INFINITY);
        assert_eq!(f(format!("1.5e-{}", "9".repeat(30))), 0.0);
        assert_eq!(f(format!("0.{z}e{}", "9".repeat(30))), 0.0);
        assert_eq!(f("0.1".into()), 0.1);
        // more than f64's 768 significant digits: the dropped tail still rounds the result
        assert_eq!(
            f(format!("9007199254740993{}1e-{}", "0".repeat(800), 801)),
            9007199254740994.0
        );
    }

    #[test]
    fn utf8_errors_read_like_cpython() {
        let m = |b: &[u8]| utf8_error(b, std::str::from_utf8(b).unwrap_err());
        assert_eq!(
            m(b"\xff"),
            "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"
        );
        assert_eq!(
            m(b"a\xe2\x82"),
            "'utf-8' codec can't decode bytes in position 1-2: unexpected end of data"
        );
        assert_eq!(
            m(b"\xe2\x82x"),
            "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte"
        );
        assert_eq!(
            m(b"\xed\xa0\x80"),
            "'utf-8' codec can't decode byte 0xed in position 0: invalid continuation byte"
        );
    }
}
