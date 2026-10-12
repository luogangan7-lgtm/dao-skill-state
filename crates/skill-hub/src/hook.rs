//! `skill-hub hook` — prompt-time skill pre-selection for Claude Code and Codex.
//!
//! Role: run as a `UserPromptSubmit` command hook. Reads the hook JSON from stdin, retrieves
//! the top-K skills lexically, optionally asks the Jev decision model (TypeSafe API) to pick
//! one or "none", and prints `hookSpecificOutput.additionalContext` naming the chosen skill.
//! Depends on: crate::{load, search} (registry + lexical ranking); serde_json; the system
//! `curl` binary for the optional Jev call (no HTTP crate).
//! Called by: the host's hook runner; `main` routes `skill-hub hook` here.
//! Invariants:
//! - Never blocks or fails the prompt: every error path prints nothing to stdout and the
//!   process exits 0 (Codex treats exit code 2 as "block this prompt").
//! - The API key is passed to curl on stdin (`-K -`), never in argv or a file.
//! - Only the prompt (truncated to `PROMPT_CHARS`) and candidate names/descriptions leave the
//!   machine, and only when `TYPESAFE_API_KEY` is set. Without it, retrieval is fully local.
//! - Jev may only answer with a retrieved candidate or "none"; anything else falls back.

use crate::{load, search};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

pub const DEFAULT_K: usize = 10;
const FLOOR: f64 = 6.0;
const REL: f64 = 0.5;
const PROMPT_CHARS: usize = 2500;
const DESC_CHARS: usize = 400;
const JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";
const JEV_TIMEOUT_SECS: u32 = 4;
const NONE: &str = "none";

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub description: String,
    pub score: f64,
}

/// How to reach Jev; absent means local lexical selection only.
pub struct Jev {
    pub key: String,
    pub model: String,
    pub curl: String,
}

#[derive(Debug, PartialEq)]
pub enum Decision {
    /// Skills to put in front of the model, and which stage chose them.
    Skills(Vec<Candidate>, &'static str),
    Nothing(&'static str),
}

/// Lexical rule used without Jev or when Jev fails: top-3 above an absolute and relative floor.
pub fn lexical(cands: &[Candidate]) -> Vec<Candidate> {
    let top = cands.first().map_or(0.0, |c| c.score);
    cands
        .iter()
        .take(3)
        .filter(|c| c.score >= FLOOR.max(REL * top))
        .cloned()
        .collect()
}

pub fn jev_body(prompt: &str, cands: &[Candidate], model: &str) -> String {
    let mut criteria = serde_json::Map::new();
    for c in cands {
        let d: String = c.description.chars().take(DESC_CHARS).collect();
        criteria.insert(
            c.id.clone(),
            Value::String(if d.is_empty() { c.id.clone() } else { d }),
        );
    }
    criteria.insert(
        NONE.into(),
        Value::String(
            "No listed skill clearly fits this request; answer directly without a skill.".into(),
        ),
    );
    let state: String = prompt.chars().take(PROMPT_CHARS).collect();
    json!({"model": model, "state": state, "questions": {"skill": {"type": "choice",
        "instructions": "Which skill should an AI agent load to handle this request? \
                         Choose 'none' if no listed skill clearly fits.",
        "criteria": criteria}}})
    .to_string()
}

/// `Ok(Some(id))` for a candidate, `Ok(None)` for "none", `Err` for anything unexpected.
pub fn parse_jev(out: &str, cands: &[Candidate]) -> Result<Option<String>, String> {
    let v: Value = serde_json::from_str(out).map_err(|e| format!("bad Jev response: {e}"))?;
    let ans = &v["answers"]["skill"];
    let label = ["answer", "label", "choice"]
        .iter()
        .find_map(|k| ans[*k].as_str())
        .ok_or("Jev response has no choice")?;
    if label == NONE {
        return Ok(None);
    }
    if cands.iter().any(|c| c.id == label) {
        return Ok(Some(label.to_string()));
    }
    Err(format!("Jev chose an unknown option: {label}"))
}

/// curl config syntax: a double-quoted string with `\` and `"` escaped.
fn quoted(s: &str) -> Result<String, String> {
    if s.chars().any(|c| c.is_control()) {
        return Err("control character in curl config value".into());
    }
    Ok(format!(
        "\"{}\"",
        s.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn call_jev(jev: &Jev, body: &str) -> Result<String, String> {
    let cfg = format!(
        "url = {}\nheader = {}\nheader = \"Content-Type: application/json\"\n\
         data-binary = {}\nmax-time = {JEV_TIMEOUT_SECS}\nsilent\nshow-error\nfail\n",
        quoted(JEV_URL)?,
        quoted(&format!("Authorization: Bearer {}", jev.key))?,
        quoted(body)?,
    );
    let mut child = Command::new(&jev.curl)
        .args(["-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", jev.curl))?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(cfg.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("Jev call failed ({})", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Choose what to show the model. Jev failures fall back to the lexical rule.
pub fn decide(cands: &[Candidate], prompt: &str, jev: Option<&Jev>) -> Decision {
    if cands.is_empty() {
        return Decision::Nothing("no-match");
    }
    if let Some(jev) = jev {
        let result = call_jev(jev, &jev_body(prompt, cands, &jev.model))
            .and_then(|out| parse_jev(&out, cands));
        match result {
            Ok(Some(id)) => {
                let c = cands.iter().find(|c| c.id == id).unwrap().clone();
                return Decision::Skills(vec![c], "jev");
            }
            Ok(None) => return Decision::Nothing("jev-none"),
            Err(e) => eprintln!("skill-hub hook: {e}; using lexical ranking"),
        }
    }
    let keep = lexical(cands);
    if keep.is_empty() {
        Decision::Nothing("below-floor")
    } else {
        Decision::Skills(
            keep,
            if jev.is_some() {
                "lexical-fallback"
            } else {
                "lexical"
            },
        )
    }
}

pub fn context(skills: &[Candidate]) -> String {
    let lines: Vec<String> = skills
        .iter()
        .map(|c| {
            format!(
                "- {}: {}",
                c.id,
                c.description.chars().take(300).collect::<String>()
            )
        })
        .collect();
    format!(
        "Skills matched to this request by skill-hub:\n{}\nIf one fits, load it by name (or `skill-hub get <id>`) \
         and follow it; otherwise ignore this note.",
        lines.join("\n")
    )
}

/// Entry point for `skill-hub hook`. Returns what to print on stdout (possibly nothing).
pub fn run(registry: Option<String>, top_k: usize) -> Option<String> {
    let t0 = Instant::now();
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).ok()?;
    let prompt = serde_json::from_str::<Value>(&input).ok()?["prompt"]
        .as_str()?
        .to_string();
    let path = registry.or_else(|| std::env::var("SKILL_HUB_REGISTRY").ok())?;
    let reg = load(&path)
        .map_err(|e| eprintln!("skill-hub hook: {e}"))
        .ok()?;
    let cands: Vec<Candidate> = search(&reg, &prompt, top_k)
        .into_iter()
        .map(|(score, s)| Candidate {
            id: s.id.clone(),
            description: s.description.clone(),
            score,
        })
        .collect();
    let jev = std::env::var("TYPESAFE_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .map(|key| Jev {
            key,
            model: std::env::var("SKILL_HUB_JEV_MODEL").unwrap_or_else(|_| "jev-latest".into()),
            curl: std::env::var("SKILL_HUB_CURL").unwrap_or_else(|_| "curl".into()),
        });
    let decision = decide(&cands, &prompt, jev.as_ref());
    if let Ok(log) = std::env::var("SKILL_HUB_HOOK_LOG") {
        // decisions only; the prompt text is never logged
        let (via, picked) = match &decision {
            Decision::Skills(s, via) => (*via, s.iter().map(|c| c.id.clone()).collect()),
            Decision::Nothing(via) => (*via, Vec::new()),
        };
        let line = json!({"via": via, "picked": picked,
                          "candidates": cands.iter().take(5).map(|c| &c.id).collect::<Vec<_>>(),
                          "ms": t0.elapsed().as_millis() as u64});
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let _ = writeln!(f, "{line}");
        }
    }
    match decision {
        Decision::Skills(s, _) => Some(
            json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": context(&s)}})
                .to_string(),
        ),
        Decision::Nothing(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, score: f64) -> Candidate {
        Candidate {
            id: id.into(),
            description: format!("{id} does things"),
            score,
        }
    }

    #[test]
    fn lexical_rule_keeps_top3_above_floors() {
        let c = [
            cand("a", 20.0),
            cand("b", 11.0),
            cand("c", 9.0),
            cand("d", 8.0),
        ];
        let ids: Vec<_> = lexical(&c).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert!(lexical(&[cand("x", 5.0)]).is_empty());
    }

    #[test]
    fn jev_body_has_candidates_none_and_truncated_prompt() {
        let body: Value = serde_json::from_str(&jev_body(
            &"x".repeat(3000),
            &[cand("a", 1.0)],
            "jev-latest",
        ))
        .unwrap();
        let crit = &body["questions"]["skill"]["criteria"];
        assert!(crit["a"].is_string() && crit[NONE].is_string());
        assert_eq!(body["state"].as_str().unwrap().len(), PROMPT_CHARS);
    }

    #[test]
    fn parse_jev_accepts_only_candidates_or_none() {
        let c = [cand("a", 1.0)];
        assert_eq!(
            parse_jev(r#"{"answers":{"skill":{"answer":"a"}}}"#, &c),
            Ok(Some("a".into()))
        );
        assert_eq!(
            parse_jev(r#"{"answers":{"skill":{"label":"none"}}}"#, &c),
            Ok(None)
        );
        assert!(parse_jev(r#"{"answers":{"skill":{"answer":"evil"}}}"#, &c).is_err());
        assert!(parse_jev("not json", &c).is_err());
    }

    #[cfg(unix)]
    fn fake_curl(name: &str, script_body: &str) -> (String, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("skill-hub-hook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("curl");
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\necho \"$@\" > \"{d}/argv\"\ncat > \"{d}/stdin\"\n{script_body}\n",
                d = dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        (p.to_string_lossy().into_owned(), dir)
    }

    #[cfg(unix)]
    #[test]
    fn jev_pick_is_used_and_key_stays_out_of_argv() {
        let (curl, dir) = fake_curl("ok", r#"echo '{"answers":{"skill":{"answer":"b"}}}'"#);
        let jev = Jev {
            key: "sk-secret-123".into(),
            model: "jev-latest".into(),
            curl,
        };
        let c = [cand("a", 20.0), cand("b", 10.0)];
        assert_eq!(
            decide(&c, "do b", Some(&jev)),
            Decision::Skills(vec![cand("b", 10.0)], "jev")
        );
        let argv = std::fs::read_to_string(dir.join("argv")).unwrap();
        let stdin = std::fs::read_to_string(dir.join("stdin")).unwrap();
        assert!(!argv.contains("sk-secret-123") && argv.contains("-K"));
        assert!(
            stdin.contains("Authorization: Bearer sk-secret-123") && stdin.contains("max-time")
        );
    }

    #[cfg(unix)]
    #[test]
    fn jev_none_injects_nothing_and_failure_falls_back() {
        let (curl, _) = fake_curl("none", r#"echo '{"answers":{"skill":{"answer":"none"}}}'"#);
        let jev = Jev {
            key: "k".into(),
            model: "m".into(),
            curl,
        };
        let c = [cand("a", 20.0)];
        assert_eq!(decide(&c, "hi", Some(&jev)), Decision::Nothing("jev-none"));
        let (curl, _) = fake_curl("fail", "exit 22");
        let jev = Jev {
            key: "k".into(),
            model: "m".into(),
            curl,
        };
        assert_eq!(
            decide(&c, "hi", Some(&jev)),
            Decision::Skills(vec![cand("a", 20.0)], "lexical-fallback")
        );
        assert_eq!(decide(&[], "hi", Some(&jev)), Decision::Nothing("no-match"));
    }

    #[test]
    fn curl_config_values_are_quoted_and_reject_control_chars() {
        assert_eq!(quoted(r#"a"b\c"#).unwrap(), r#""a\"b\\c""#);
        assert!(quoted("line\nbreak").is_err());
    }
}
