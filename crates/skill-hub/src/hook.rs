//! `skill-hub hook` — prompt-time skill pre-selection for Claude Code and Codex.
//!
//! Role: run as a `UserPromptSubmit` command hook. Reads the hook JSON from stdin, retrieves
//! the top-K skills lexically, optionally asks the Jev decision model (TypeSafe API) to pick
//! one or the no-skill option, and prints `hookSpecificOutput.additionalContext` naming the chosen skill.
//! Depends on: crate::{load, search} (registry + lexical ranking); serde_json; the system
//! `curl` binary for the optional Jev call (no HTTP crate).
//! Called by: the host's hook runner; `main` routes `skill-hub hook` here.
//! Invariants:
//! - Never blocks or fails the prompt: every error path prints nothing to stdout and the
//!   process exits 0 (Codex treats exit code 2 as "block this prompt").
//! - The API key is passed to curl on stdin (`-q -K -`: user curlrc ignored), never in argv or a file.
//! - Only the prompt (cut to its first and last ~1,250 characters, see `bound_prompt`) and
//!   candidate names/descriptions leave the machine, and only when `TYPESAFE_API_KEY` is set.
//!   Without it, retrieval is fully local.
//! - Jev may only answer with a retrieved candidate or the no-skill sentinel; anything else
//!   falls back to the lexical rule.

use crate::load;
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
/// Abstain option offered to Jev. Spaces and parentheses are not valid in skill names; if a
/// candidate id still equals it, `sentinel` extends it until it is unique for that request.
const NONE_BASE: &str = "(no skill)";

fn sentinel(cands: &[Candidate]) -> String {
    let mut s = NONE_BASE.to_string();
    while cands.iter().any(|c| c.id == s) {
        s.push('*');
    }
    s
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub description: String,
    /// BM25 score (0 when only dense recall found the skill).
    pub score: f64,
    /// Cosine similarity when dense recall ran.
    pub dense: Option<f64>,
}

/// A local cross-encoder that scores (prompt, candidate) pairs; the best is kept if it clears
/// `tau`, otherwise the hook abstains. D7 (held-out 40 cases, same RRF top-10): bge-reranker-v2-m3
/// with tau 0.02 chose right on 31/40 against 24/40 for the lexical rule, mostly by abstaining
/// on chit-chat (10/12 vs 5/12); ~1.2 s per prompt on CPU.
/// Scores for (query, texts), in text order.
pub type Scorer<'a> = dyn Fn(&str, &[String]) -> Result<Vec<f32>, String> + 'a;

pub struct Rerank<'a> {
    pub score: &'a Scorer<'a>,
    pub tau: f32,
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

/// Absolute score floor for a registry of `n` skills. BM25 IDF grows with corpus size, so the
/// floor (calibrated at ~2,000+ skills) shrinks in proportion to the largest possible IDF.
fn floor_for(n: usize) -> f64 {
    let max_idf = |n: f64| ((n - 1.0 + 0.5) / 1.5 + 1.0).ln();
    FLOOR * (max_idf(n.max(1) as f64) / max_idf(2000.0)).min(1.0)
}

/// Lexical rule used without Jev or when Jev fails: of the top 3 (in fused order when dense
/// recall ran), keep those whose BM25 score clears the absolute and relative floor. Dense
/// recall changes which skills reach the top 3, not the floor: a cosine keep rule made no
/// measured difference in the D5 review, so there is none.
pub fn lexical(cands: &[Candidate], n_skills: usize) -> Vec<Candidate> {
    let top = cands.iter().map(|c| c.score).fold(0.0, f64::max);
    cands
        .iter()
        .take(3)
        .filter(|c| c.score >= floor_for(n_skills).max(REL * top))
        .cloned()
        .collect()
}

/// At most `PROMPT_CHARS` characters: the head and the tail of a long prompt, so a request
/// written after a long paste (logs, code) is still seen by retrieval and by Jev.
pub fn bound_prompt(prompt: &str) -> String {
    const GAP: &str = "\n…\n";
    let n = prompt.chars().count();
    if n <= PROMPT_CHARS {
        return prompt.to_string();
    }
    let keep = PROMPT_CHARS - GAP.chars().count();
    let head: String = prompt.chars().take(keep / 2).collect();
    let tail: String = prompt.chars().skip(n - (keep - keep / 2)).collect();
    format!("{head}{GAP}{tail}")
}

pub fn jev_body(prompt: &str, cands: &[Candidate], model: &str) -> String {
    let mut criteria = serde_json::Map::new();
    let none = sentinel(cands);
    for c in cands {
        let d: String = c.description.chars().take(DESC_CHARS).collect();
        criteria.insert(
            c.id.clone(),
            Value::String(if d.is_empty() { c.id.clone() } else { d }),
        );
    }
    criteria.insert(
        none.clone(),
        Value::String(
            "No listed skill clearly fits this request; answer directly without a skill.".into(),
        ),
    );
    let state = bound_prompt(prompt);
    let instructions = format!(
        "Which skill should an AI agent load to handle this request? \
         Choose {none} if no listed skill clearly fits."
    );
    json!({"model": model, "state": state, "questions": {"skill": {"type": "choice",
        "instructions": instructions, "criteria": criteria}}})
    .to_string()
}

/// `Ok(Some(id))` for a candidate, `Ok(None)` for the no-skill sentinel, `Err` otherwise.
pub fn parse_jev(out: &str, cands: &[Candidate]) -> Result<Option<String>, String> {
    let v: Value = serde_json::from_str(out).map_err(|e| format!("bad Jev response: {e}"))?;
    let ans = &v["answers"]["skill"];
    let label = ["answer", "label", "choice"]
        .iter()
        .find_map(|k| ans[*k].as_str())
        .ok_or("Jev response has no choice")?;
    if label == sentinel(cands) {
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
        // -q must come first: skip ~/.curlrc so its own `url` entries can't receive the key
        .args(["-q", "-K", "-"])
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
pub fn decide(
    cands: &[Candidate],
    prompt: &str,
    jev: Option<&Jev>,
    rerank: Option<&Rerank>,
    n_skills: usize,
) -> Decision {
    if cands.is_empty() {
        return Decision::Nothing("no-match");
    }
    // the local reranker comes first: it never sends the prompt off the machine
    if let Some(r) = rerank {
        let docs: Vec<String> = cands
            .iter()
            .map(|c| {
                let d: String = c.description.chars().take(DESC_CHARS).collect();
                format!("{}: {d}", c.id)
            })
            .collect();
        match (r.score)(prompt, &docs) {
            Ok(s) if s.len() == cands.len() => {
                let best = (0..s.len()).max_by(|a, b| s[*a].total_cmp(&s[*b])).unwrap();
                return if s[best] >= r.tau {
                    Decision::Skills(vec![cands[best].clone()], "rerank")
                } else {
                    Decision::Nothing("rerank-none")
                };
            }
            Ok(_) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "skill-hub hook: reranker returned the wrong number of scores"
                );
            }
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "skill-hub hook: {e}; reranker skipped");
            }
        }
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
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "skill-hub hook: {e}; using lexical ranking"
                );
            }
        }
    }
    let keep = lexical(cands, n_skills);
    if keep.is_empty() {
        Decision::Nothing("below-floor")
    } else {
        Decision::Skills(
            keep,
            if jev.is_some() || rerank.is_some() {
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
        "skill-hub retrieved these skills as possibly relevant to the request \
         (load one with `skill-hub get <id>`, or by name if it is installed; ignore them if none fits):\n{}",
        lines.join("\n")
    )
}

/// Entry point for `skill-hub hook`. Returns what to print on stdout (possibly nothing).
pub fn run(registry: Option<String>, top_k: usize) -> Option<String> {
    let t0 = Instant::now();
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).ok()?;
    // bounded before retrieval too: pasted logs/code must not stall the prompt
    let prompt = bound_prompt(serde_json::from_str::<Value>(&input).ok()?["prompt"].as_str()?);
    let path = registry.or_else(|| std::env::var("SKILL_HUB_REGISTRY").ok())?;
    let reg = load(&path)
        .map_err(|e| {
            let _ = writeln!(std::io::stderr(), "skill-hub hook: {e}");
        })
        .ok()?;
    let cands: Vec<Candidate> = crate::ranked(&reg, &path, &prompt, top_k)
        .into_iter()
        .map(|h| Candidate {
            id: h.skill.id.clone(),
            description: h.skill.description.clone(),
            score: h.lexical,
            dense: h.dense.map(f64::from),
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
    let rerank_fn = |q: &str, docs: &[String]| {
        let ms = std::env::var("SKILL_HUB_RERANK_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000);
        crate::dense::rerank(
            q,
            docs,
            Instant::now() + std::time::Duration::from_millis(ms),
        )
    };
    let rerank = std::env::var("SKILL_HUB_RERANK").is_ok().then(|| Rerank {
        score: &rerank_fn,
        tau: std::env::var("SKILL_HUB_RERANK_TAU")
            .ok()
            // a probability threshold: nan, negative or > 1 would mean "never" or "always"
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|t| t.is_finite() && (0.0..=1.0).contains(t))
            .unwrap_or(0.02),
    });
    let decision = decide(
        &cands,
        &prompt,
        jev.as_ref(),
        rerank.as_ref(),
        reg.skills.len(),
    );
    if let Ok(log) = std::env::var("SKILL_HUB_HOOK_LOG") {
        // decisions only; the prompt text is never logged
        let (via, picked) = match &decision {
            Decision::Skills(s, via) => (*via, s.iter().map(|c| c.id.clone()).collect()),
            Decision::Nothing(via) => (*via, Vec::new()),
        };
        let line = json!({"via": via, "picked": picked,
                          "dense": cands.iter().any(|c| c.dense.is_some()),
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
            dense: None,
        }
    }

    #[test]
    fn floor_scales_with_registry_size_and_sentinel_is_not_a_name() {
        assert!((floor_for(5000) - FLOOR).abs() < 1e-9);
        assert!(floor_for(1) < 0.3);
        // a one-skill registry: an exact single-term match must still be shown
        let root =
            std::env::temp_dir().join(format!("skill-hub-hook-small-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("netlify")).unwrap();
        std::fs::write(
            root.join("netlify/SKILL.md"),
            "---\nname: netlify-deploy\ndescription: Deploy sites\n---\n",
        )
        .unwrap();
        let roots = crate::normalize_roots(vec![crate::Root {
            label: "t".into(),
            path: root.to_string_lossy().into(),
        }])
        .unwrap();
        let reg = crate::Registry {
            schema: crate::SCHEMA,
            skills: crate::scan(&roots).unwrap().skills,
            roots,
        };
        let cands: Vec<Candidate> = crate::search(&reg, "netlify", 10)
            .into_iter()
            .map(|(score, s)| Candidate {
                id: s.id.clone(),
                description: s.description.clone(),
                score,
                dense: None,
            })
            .collect();
        assert_eq!(lexical(&cands, reg.skills.len()).len(), 1);
        let _ = std::fs::remove_dir_all(root);
        // a skill literally named "none" is an ordinary candidate
        let c = [cand("none", 1.0)];
        assert_eq!(
            parse_jev(r#"{"answers":{"skill":{"answer":"none"}}}"#, &c),
            Ok(Some("none".into()))
        );
    }

    #[test]
    fn long_prompts_keep_head_and_tail() {
        let long = format!(
            "{}please deploy this site to Netlify.",
            "server log line 42 ok\n".repeat(400)
        );
        let b = bound_prompt(&long);
        assert_eq!(b.chars().count(), PROMPT_CHARS);
        assert!(b.starts_with("server log") && b.ends_with("to Netlify."));
        assert_eq!(bound_prompt(&b), b); // idempotent
        assert_eq!(bound_prompt("short"), "short");
    }

    #[test]
    fn lexical_rule_keeps_top3_above_floors() {
        let c = [
            cand("a", 20.0),
            cand("b", 11.0),
            cand("c", 9.0),
            cand("d", 8.0),
        ];
        let ids: Vec<_> = lexical(&c, 3000).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert!(lexical(&[cand("x", 5.0)], 3000).is_empty());
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
        assert!(crit["a"].is_string() && crit["(no skill)"].is_string());
        assert_eq!(
            body["state"].as_str().unwrap().chars().count(),
            PROMPT_CHARS
        );
    }

    #[test]
    fn parse_jev_accepts_only_candidates_or_none() {
        let c = [cand("a", 1.0)];
        assert_eq!(
            parse_jev(r#"{"answers":{"skill":{"answer":"a"}}}"#, &c),
            Ok(Some("a".into()))
        );
        assert_eq!(
            parse_jev(r#"{"answers":{"skill":{"label":"(no skill)"}}}"#, &c),
            Ok(None)
        );
        assert!(parse_jev(r#"{"answers":{"skill":{"answer":"evil"}}}"#, &c).is_err());
        // a skill that happens to use the sentinel text stays selectable
        let c = [cand("(no skill)", 1.0)];
        assert_eq!(sentinel(&c), "(no skill)*");
        assert_eq!(
            parse_jev(r#"{"answers":{"skill":{"answer":"(no skill)"}}}"#, &c),
            Ok(Some("(no skill)".into()))
        );
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
    fn rerank_keeps_the_best_above_tau_abstains_below_and_falls_back_on_error() {
        let c = vec![cand("a", 20.0), cand("b", 15.0), cand("c", 1.0)];
        let fixed = |s: Vec<f32>| move |_: &str, _: &[String]| Ok(s.clone());
        let pick = fixed(vec![0.1, 0.9, 0.0]);
        let r = Rerank {
            score: &pick,
            tau: 0.02,
        };
        match decide(&c, "do b", None, Some(&r), 3000) {
            Decision::Skills(s, via) => {
                assert_eq!((s[0].id.as_str(), s.len(), via), ("b", 1, "rerank"))
            }
            d => panic!("{d:?}"),
        }
        let low = fixed(vec![0.01, 0.005, 0.0]);
        let r = Rerank {
            score: &low,
            tau: 0.02,
        };
        assert!(matches!(
            decide(&c, "hi", None, Some(&r), 3000),
            Decision::Nothing("rerank-none")
        ));
        // a reranker that fails or miscounts leaves the lexical rule in charge
        let broken = |_: &str, _: &[String]| -> Result<Vec<f32>, String> { Err("down".into()) };
        let short = fixed(vec![0.5]);
        for f in [
            &broken as &dyn Fn(&str, &[String]) -> Result<Vec<f32>, String>,
            &short,
        ] {
            let r = Rerank {
                score: f,
                tau: 0.02,
            };
            assert!(matches!(
                decide(&c, "do a", None, Some(&r), 3000),
                Decision::Skills(_, "lexical-fallback")
            ));
        }
        // the reranker sees "id: description" for every candidate, in order
        let seen = std::cell::RefCell::new(Vec::new());
        let spy = |_: &str, d: &[String]| {
            *seen.borrow_mut() = d.to_vec();
            Ok(vec![0.0; d.len()])
        };
        let r = Rerank {
            score: &spy,
            tau: 0.02,
        };
        decide(&c, "x", None, Some(&r), 3000);
        assert_eq!(seen.borrow().len(), 3);
        assert!(seen.borrow()[1].starts_with("b: "));
    }

    #[test]
    fn the_prompt_reaches_jev_only_when_the_local_reranker_fails() {
        let (curl, dir) = fake_curl(
            "rerank-first",
            r#"echo '{"answers":{"skill":{"answer":"a"}}}'"#,
        );
        let jev = Jev {
            key: "k".into(),
            model: "m".into(),
            curl,
        };
        let c = [cand("a", 20.0), cand("b", 15.0)];
        let ok = |_: &str, d: &[String]| Ok(vec![0.9; d.len()]);
        let r = Rerank {
            score: &ok,
            tau: 0.02,
        };
        assert!(matches!(
            decide(&c, "secret", Some(&jev), Some(&r), 3000),
            Decision::Skills(_, "rerank")
        ));
        assert!(
            !dir.join("argv").exists(),
            "jev was called although the reranker answered"
        );
        let down = |_: &str, _: &[String]| -> Result<Vec<f32>, String> { Err("down".into()) };
        let r = Rerank {
            score: &down,
            tau: 0.02,
        };
        assert!(matches!(
            decide(&c, "secret", Some(&jev), Some(&r), 3000),
            Decision::Skills(_, "jev")
        ));
        assert!(
            dir.join("argv").exists(),
            "jev is the fallback when the reranker fails"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

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
            decide(&c, "do b", Some(&jev), None, 3000),
            Decision::Skills(vec![cand("b", 10.0)], "jev")
        );
        let argv = std::fs::read_to_string(dir.join("argv")).unwrap();
        let stdin = std::fs::read_to_string(dir.join("stdin")).unwrap();
        assert!(!argv.contains("sk-secret-123") && argv.starts_with("-q -K -"));
        assert!(
            stdin.contains("Authorization: Bearer sk-secret-123") && stdin.contains("max-time")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn jev_none_injects_nothing_and_failure_falls_back() {
        let (curl, d1) = fake_curl(
            "none",
            r#"echo '{"answers":{"skill":{"answer":"(no skill)"}}}'"#,
        );
        let jev = Jev {
            key: "k".into(),
            model: "m".into(),
            curl,
        };
        let c = [cand("a", 20.0)];
        assert_eq!(
            decide(&c, "hi", Some(&jev), None, 3000),
            Decision::Nothing("jev-none")
        );
        let (curl, d2) = fake_curl("fail", "exit 22");
        let jev = Jev {
            key: "k".into(),
            model: "m".into(),
            curl,
        };
        assert_eq!(
            decide(&c, "hi", Some(&jev), None, 3000),
            Decision::Skills(vec![cand("a", 20.0)], "lexical-fallback")
        );
        assert_eq!(
            decide(&[], "hi", Some(&jev), None, 3000),
            Decision::Nothing("no-match")
        );
        let _ = (std::fs::remove_dir_all(d1), std::fs::remove_dir_all(d2));
    }

    #[test]
    fn curl_config_values_are_quoted_and_reject_control_chars() {
        assert_eq!(quoted(r#"a"b\c"#).unwrap(), r#""a\"b\\c""#);
        assert!(quoted("line\nbreak").is_err());
    }
}
