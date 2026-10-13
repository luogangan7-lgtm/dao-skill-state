//! M3 gate: replays tests/fixtures/check_assessment.json, frozen from the Python CLI
//! (dao-skill-state/scripts/check_assessment.py) by `tools/diff_cli.py --corpus assess --freeze`,
//! against the `dao assess` binary. Each case runs in its own directory with the frozen argv;
//! stdout bytes (sha256), exit code and stderr class must match, except the KNOWN divergences.

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::process::Command;

/// Documented divergences (all exit codes 0/1/2 on the Rust side, never a crash):
/// - help (`-h`, `--help`, `--he`, `-hx`, `-h x`, `-hh`, help after a file): same text, but the
///   usage line names `dao assess` instead of `check_assessment.py`.
/// - nesting depth: Rust stops at 100_000 levels with "maximum recursion depth exceeded while
///   decoding a JSON array|object ..."; CPython 3.14 stops at its C stack (~116k levels on this
///   macOS arm64 build, fewer on 8 MiB stacks) with "Stack overflow (used N kB) ...".
///   d-arr-100001 / d-arr-116000 / d-obj-100001 parse in Python and are input errors in Rust.
/// - PY_CRASH: Python raises ValueError (traceback, exit 1) when str(int) exceeds 4300 digits:
///   the expanded-plan message (n-reps-4300-nines) and a token sum (n-tokens-4300-nines).
///   Rust prints the full integer.
const KNOWN: [&str; 16] = [
    "c-help",
    "c-help-hh",
    "c-help-hspace",
    "c-help-hx",
    "c-help-late",
    "c-help-long",
    "c-help-prefix",
    "d-arr-100001",
    "d-arr-116000",
    "d-arr-300000",
    "d-arr-3000000",
    "d-obj-100001",
    "d-obj-200000",
    "n-reps-4300-nines",
    "n-tokens-4300-nines",
    "t-recursion",
];

fn b64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let digits: Vec<u8> = s.bytes().filter(|&c| c != b'=').map(val).collect();
    let mut out = Vec::new();
    for chunk in digits.chunks(4) {
        let n = chunk.iter().fold(0u32, |acc, &d| acc << 6 | d as u32) << (6 * (4 - chunk.len()));
        out.extend(&n.to_be_bytes()[1..chunk.len()]);
    }
    out
}

fn stderr_class(err: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(err);
    if text.is_empty() {
        "empty"
    } else if text.contains("Traceback (most recent call last)") {
        "traceback"
    } else if text.contains("panicked") {
        "panic"
    } else if text.starts_with("usage:") {
        "usage"
    } else {
        "other"
    }
}

#[test]
fn matches_frozen_python_baseline() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/check_assessment.json")).unwrap();
    assert!(cases.len() > 1500, "fixture shrank: {} cases", cases.len());
    let root = std::env::temp_dir().join(format!("dao-assess-fx-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (mut bad, mut panics) = (Vec::new(), Vec::new());
    for (i, c) in cases.iter().enumerate() {
        let name = c["name"].as_str().unwrap();
        let dir = root.join(format!("{i:05}"));
        fs::create_dir_all(&dir).unwrap();
        for d in c["dirs"].as_array().unwrap() {
            fs::create_dir_all(dir.join(d.as_str().unwrap())).unwrap();
        }
        for (rel, parts) in c["files"].as_object().unwrap() {
            let mut bytes = Vec::new();
            for p in parts.as_array().unwrap() {
                let chunk = match p["t"].as_str() {
                    Some(t) => t.as_bytes().to_vec(),
                    None => b64(p["b"].as_str().unwrap()),
                };
                bytes.extend(chunk.repeat(p["n"].as_u64().unwrap() as usize));
            }
            let path = dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        for f in c["fifos"].as_array().unwrap() {
            let ok = Command::new("mkfifo")
                .arg(dir.join(f.as_str().unwrap()))
                .status();
            assert!(ok.unwrap().success());
        }
        let args: Vec<OsString> = c["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| match a.as_str() {
                Some(s) => s.into(),
                None => OsString::from_vec(b64(a["b"].as_str().unwrap())),
            })
            .collect();
        let out = Command::new(env!("CARGO_BIN_EXE_dao"))
            .arg("assess")
            .args(&args)
            .current_dir(&dir)
            .output()
            .unwrap();
        let class = stderr_class(&out.stderr);
        if class == "panic" || out.status.code().is_none() {
            panics.push(name.to_string());
        }
        let same = format!("{:x}", Sha256::digest(&out.stdout)) == c["python_stdout_sha256"]
            && out.status.code().map(i64::from) == c["python_exit"].as_i64()
            && class == c["python_stderr"];
        if !same {
            bad.push(name.to_string());
        }
        let _ = fs::remove_dir_all(&dir);
    }
    let _ = fs::remove_dir_all(&root);
    assert!(panics.is_empty(), "dao assess crashed on: {panics:?}");
    let mut known: Vec<String> = KNOWN.iter().map(|s| s.to_string()).collect();
    known.sort();
    bad.sort();
    assert_eq!(
        bad, known,
        "unexplained differences from the Python baseline"
    );
}
