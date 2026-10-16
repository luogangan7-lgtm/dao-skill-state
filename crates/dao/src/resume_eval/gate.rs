//! Strict recovery acceptance; depends on fixture specs, hashes/valid_skill and serde_json.
//! Called by resume run/check; no model or Python execution. Missing evidence never means PASS.
//! Probe classification observes known tool inputs, not a shell security sandbox or isolation.

use super::{fixture, hashes, valid_skill};
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

fn need(ok: bool, message: impl Into<String>) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn keys(value: &Value, expected: &[&str]) -> bool {
    value
        .as_object()
        .map(|v| v.len() == expected.len() && expected.iter().all(|k| v.contains_key(*k)))
        .unwrap_or(false)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn regular(path: &Path) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    need(
        meta.file_type().is_file() && meta.len() <= 64 * 1024 * 1024,
        format!("not a bounded regular file: {}", path.display()),
    )?;
    fs::read(path).map_err(|e| e.to_string())
}

// Only tokenization of the observed command shapes: quoting, comments and separators.
// Here-doc bodies are stdin data; retain headers so `cat <<EOF | python` is still execution.
fn shell_words(command: &str) -> Vec<String> {
    let heredoc = Regex::new(r#"<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?"#).unwrap();
    let mut source = String::new();
    let mut end: Option<String> = None;
    for line in command.lines() {
        if let Some(delimiter) = &end {
            if line.trim() == delimiter {
                end = None;
            }
            continue;
        }
        if let Some(c) = heredoc.captures(line) {
            end = Some(c[1].to_string());
        }
        source.push_str(line);
        source.push('\n');
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quote, mut escape, mut comment) = (None, false, false);
    for ch in source.chars() {
        if comment {
            if ch != '\n' {
                continue;
            }
            comment = false;
        }
        if escape {
            word.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                word.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch == '#' && word.is_empty() {
            comment = true;
        } else if ch.is_whitespace() || ";&|<>".contains(ch) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if ch == '\n' || ";&|<>".contains(ch) {
                words.push(ch.to_string());
            }
        } else {
            word.push(ch);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn forbidden(path: &str) -> bool {
    path.contains("/tmp/dao-resume-store-")
        || path.contains("$DAO_RESUME_STORE")
        || path.contains("${DAO_RESUME_STORE}")
        || Regex::new(r"/tmp/warehouse(/|$)").unwrap().is_match(path)
        || path.contains("$WAREHOUSE_DIR")
        || path.contains("${WAREHOUSE_DIR}")
        || path.contains("site-packages/warehouse.py")
        || path.contains("dist-packages/warehouse.py")
        || path.contains("evals/agent/")
        || path.ends_with("evals/agent")
        || path.contains("crates/dao/src/resume_eval/")
        || path.ends_with("crates/dao/src/resume_eval")
        || Regex::new(r"/tmp/(resume|make)-[A-Za-z0-9_]{8}(/|$)")
            .unwrap()
            .is_match(path)
}

fn shell_probe(command: &str) -> bool {
    let words = shell_words(command);
    let python = Regex::new(r"^python([0-9.]*)?$").unwrap();
    let mut segment: Vec<&str> = Vec::new();
    let mut redirection = false;
    for word in words.iter().map(String::as_str).chain(std::iter::once(";")) {
        if word == "<" || word == ">" {
            redirection = true;
            continue;
        }
        if redirection {
            if forbidden(word) {
                return true;
            }
            redirection = false;
        }
        if !matches!(word, ";" | "&" | "|" | "\n") {
            segment.push(word);
            continue;
        }
        if let Some(first) = segment.first() {
            let exe = Path::new(first)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(first);
            // Any actual Python execution violates this task's language boundary; written code does not.
            if python.is_match(exe) || exe.ends_with(".py") {
                return true;
            }
            let mut operands = segment
                .iter()
                .skip(1)
                .copied()
                .filter(|p| !p.starts_with('-'));
            // For the observed simple search forms the first operand is a pattern/program, not a file.
            if matches!(exe, "grep" | "rg" | "sed" | "awk") && !segment.contains(&"--files") {
                operands.next();
            }
            if matches!(
                exe,
                "cat"
                    | "ls"
                    | "cd"
                    | "find"
                    | "sed"
                    | "head"
                    | "tail"
                    | "cp"
                    | "mv"
                    | "rm"
                    | "mkdir"
                    | "touch"
                    | "readlink"
                    | "stat"
                    | "grep"
                    | "rg"
                    | "awk"
                    | "wc"
                    | "sort"
            ) && operands.any(forbidden)
            {
                return true;
            }
            if exe == "for"
                && words.iter().any(|w| w == "find")
                && segment.iter().any(|p| forbidden(p))
            {
                return true;
            }
            if exe == "find"
                && segment
                    .iter()
                    .any(|p| matches!(*p, "/" | "/tmp" | "/opt" | "/usr"))
                && segment.contains(&"warehouse.py")
            {
                return true;
            }
        }
        segment.clear();
    }
    false
}

/// Count observed backend/oracle/language violations, including the maker; never execute inputs.
pub fn probed(run: &Value) -> u64 {
    let Some(trace) = run["tool_trace"].as_array() else {
        return 1;
    };
    trace
        .iter()
        .filter(|event| {
            let Some(input) = event["input"].as_object() else {
                return true;
            };
            match event["name"].as_str() {
                Some("Bash") => input
                    .get("command")
                    .and_then(Value::as_str)
                    .map(shell_probe)
                    .unwrap_or(true),
                Some("Read" | "Write" | "Edit" | "Glob" | "Grep") => {
                    let path = input
                        .get("file_path")
                        .or_else(|| input.get("path"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let pattern = match event["name"].as_str() {
                        Some("Glob") => "pattern",
                        Some("Grep") => "glob",
                        _ => "",
                    };
                    forbidden(&format!(
                        "{}/{}",
                        path.trim_end_matches('/'),
                        input.get(pattern).and_then(Value::as_str).unwrap_or("")
                    ))
                }
                _ => false,
            }
        })
        .count() as u64
}

fn loaded(run: &Value, name: &str) -> bool {
    let call = |s: &str| s == name || s == format!("read:{name}");
    let reported = run["skill_calls"]
        .as_array()
        .map(|a| {
            !a.is_empty()
                && a.iter().all(Value::is_string)
                && a.iter().filter_map(Value::as_str).any(call)
        })
        .unwrap_or(false);
    reported
        && run["tool_trace"]
            .as_array()
            .map(|a| {
                a.iter().any(|e| {
                    (e["name"] == "Skill"
                        && (e["input"]["skill"].as_str() == Some(name)
                            || e["input"]["command"].as_str() == Some(name)))
                        || (e["name"] == "Read"
                            && e["input"]["file_path"]
                                .as_str()
                                .map(|p| p.contains(&format!("/.claude/skills/{name}/")))
                                .unwrap_or(false))
                })
            })
            .unwrap_or(false)
}

fn numbers(value: &Value) -> Result<Vec<u64>, String> {
    value
        .as_array()
        .ok_or("missing step list")?
        .iter()
        .map(|v| {
            v.as_u64()
                .filter(|n| (1..=8).contains(n))
                .ok_or_else(|| "malformed step".into())
        })
        .collect()
}

fn audit(events: &Value) -> Result<(Vec<u64>, Vec<u64>), String> {
    let mut loads = Vec::new();
    let mut attempts = Vec::new();
    for event in events.as_array().ok_or("missing audit events")? {
        let n = event["n"]
            .as_u64()
            .filter(|n| (1..=8).contains(n))
            .ok_or("invalid audit step")?;
        match event["kind"].as_str() {
            Some("attempt") => {
                need(keys(event, &["kind", "n"]), "attempt fields")?;
                attempts.push(n);
            }
            Some("load") => {
                need(
                    keys(event, &["kind", "n"])
                        && attempts.iter().filter(|x| **x == n).count()
                            > loads.iter().filter(|x| **x == n).count(),
                    "load without attempt",
                )?;
                loads.push(n);
            }
            Some("query") => need(
                keys(event, &["kind", "n", "loaded"])
                    && event["loaded"].as_bool() == Some(loads.contains(&n)),
                "query contradicts authoritative commits",
            )?,
            _ => return Err("malformed audit event".into()),
        }
    }
    Ok((loads, attempts))
}

/// Accept exactly the selected make:dao matrix and immutable authoritative event prefixes.
pub fn validate_rows(rows: &[Value], tasks: &[String], reps: u64) -> Result<(), String> {
    need(
        reps > 0 && !tasks.is_empty() && !rows.is_empty(),
        "empty matrix",
    )?;
    let selected: BTreeSet<_> = tasks.iter().cloned().collect();
    need(selected.len() == tasks.len(), "duplicate tasks")?;
    for task in tasks {
        fixture::spec(task)?;
    }
    let expected = tasks
        .len()
        .checked_mul(usize::try_from(reps).map_err(|_| "reps too large")?)
        .ok_or("matrix too large")?;
    need(rows.len() == expected, "missing or extra rows")?;
    let slug = Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").unwrap();
    let mut seen = BTreeSet::new();
    for row in rows {
        let task = row["task"].as_str().ok_or("missing task")?;
        let rep = row["rep"].as_u64().ok_or("invalid repetition")?;
        need(
            selected.contains(task)
                && rep < reps
                && row["arm"] == "make:dao"
                && seen.insert((task, rep)),
            "extra or duplicate row",
        )?;
        for field in ["pass", "made", "clean_stop", "s2_loaded_runbook"] {
            need(
                row[field].as_bool() == Some(true),
                format!("{task}/{rep}: {field}"),
            )?;
        }
        need(row["tampered"].as_bool() == Some(false), "tampered fixture")?;
        for field in ["reruns", "probes"] {
            need(
                row[field].as_u64() == Some(0),
                format!("nonzero or invalid {field}"),
            )?;
        }
        let skill = row["skill_name"]
            .as_str()
            .ok_or("missing generated skill name")?;
        need(
            skill.len() <= 64 && slug.is_match(skill),
            "invalid generated skill name",
        )?;
        for phase in ["make", "s1", "s2"] {
            let run = &row[phase];
            need(
                run["rc"].as_i64() == Some(0)
                    && run
                        .get("error")
                        .map(|v| v.is_null() || v.as_str() == Some(""))
                        .unwrap_or(false)
                    && run
                        .get("is_error")
                        .map(|v| v.is_null() || v.as_bool() == Some(false))
                        .unwrap_or(false),
                format!("{phase}: model failed"),
            )?;
            need(
                run["turns"].as_u64().map(|n| n > 0).unwrap_or(false)
                    && run["subtype"] == "success"
                    && run["errors"].as_array().map(Vec::is_empty) == Some(true)
                    && run["diagnostics_truncated"].as_bool() == Some(false),
                format!("{phase}: failed or incomplete result diagnostics"),
            )?;
            need(
                run["tool_trace"]
                    .as_array()
                    .map(|v| !v.is_empty())
                    .unwrap_or(false)
                    && run["trace_truncated"].as_bool() == Some(false),
                format!("{phase}: incomplete tool trace"),
            )?;
            need(
                loaded(
                    run,
                    if phase == "make" {
                        "dao-skill-state"
                    } else {
                        skill
                    },
                ),
                format!("{phase}: missing actual skill load"),
            )?;
            need(
                probed(run) == 0,
                format!("{phase}: observed backend/oracle/Python probe"),
            )?;
        }
        let spec = fixture::spec(task)?;
        let (first, first_attempts) = audit(&row["events_after_s1"])?;
        let (final_loads, attempts) = audit(&row["events"])?;
        need(
            first == spec.first_loads
                && first == numbers(&row["after_session1"])?
                && first_attempts == (1..=5).collect::<Vec<_>>(),
            "false first-session stop",
        )?;
        need(
            final_loads == spec.final_loads
                && final_loads == numbers(&row["ran"])?
                && attempts == spec.attempts,
            "wrong final execution boundary",
        )?;
        let prefix = row["events_after_s1"]
            .as_array()
            .ok_or("missing first audit")?;
        let final_events = row["events"].as_array().ok_or("missing final audit")?;
        need(final_events.starts_with(prefix), "changed audit prefix")?;
        if spec.no_query {
            need(
                !final_events.iter().any(|e| e["kind"] == "query"),
                "forbidden query",
            )?;
        }
        if let Some(answer) = spec.query {
            let mut confirmed = false;
            for event in &final_events[prefix.len()..] {
                if !confirmed && event["kind"] == "query" && event["n"].as_u64() == Some(5) {
                    need(
                        event["loaded"].as_bool() == Some(answer),
                        "wrong reconciliation answer",
                    )?;
                    confirmed = true;
                }
                if event["kind"] == "attempt" || event["kind"] == "load" {
                    need(confirmed, "action before reconciliation")?;
                }
            }
            need(confirmed, "missing reconciliation query")?;
        }
    }
    Ok(())
}

const SOURCES: [&str; 4] = ["mod.rs", "runner.rs", "fixture.rs", "gate.rs"];

fn env(name: &str) -> Result<Option<String>, String> {
    std::env::var_os(name)
        .map(|v| v.into_string().map_err(|_| format!("non-Unicode {name}")))
        .transpose()
}

fn current(repo: &Path, tasks: &[String], reps: u64, exe: &Path) -> Result<Value, String> {
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .map_err(|e| e.to_string())?;
    need(head.status.success(), "cannot read git HEAD")?;
    let sha = String::from_utf8(head.stdout)
        .map_err(|e| e.to_string())?
        .trim()
        .to_owned();
    if let Some(github) = env("GITHUB_SHA")? {
        need(github == sha, "GITHUB_SHA differs from HEAD")?;
    }
    let mut sources = BTreeMap::new();
    for name in SOURCES {
        let path = format!("crates/dao/src/resume_eval/{name}");
        sources.insert(path.clone(), digest(&regular(&repo.join(path))?));
    }
    Ok(json!({"schema":"dao-resume-producer/2-rust", "sha":sha,
        "authoring_sha256":digest(&regular(&repo.join("dao-skill-state/references/authoring.md"))?),
        "state_contract_sha256":digest(&regular(&repo.join("dao-skill-state/references/state-contract.md"))?),
        "binary_sha256":digest(&fs::read(exe).map_err(|e| e.to_string())?), "fixtures":sources, "tasks":tasks, "arms":["make:dao"], "reps":reps}))
}

fn producer_matches(producer: &Value, expected: &Value) -> Result<(), String> {
    need(
        keys(
            producer,
            &[
                "schema",
                "sha",
                "authoring_sha256",
                "state_contract_sha256",
                "binary_sha256",
                "fixtures",
                "tasks",
                "arms",
                "reps",
                "budgets",
                "github_run_id",
                "github_run_attempt",
            ],
        ),
        "invalid producer schema",
    )?;
    for (field, value) in expected.as_object().ok_or("invalid expected producer")? {
        need(
            &producer[field] == value,
            format!("stale or mismatched producer {field}"),
        )?;
    }
    need(
        producer["budgets"]
            == json!({"make":{"max_turns":40,"timeout_s":300},"s1":{"max_turns":45,"timeout_s":180},"s2":{"max_turns":45,"timeout_s":180}}),
        "invalid strict phase budgets",
    )?;
    for field in ["github_run_id", "github_run_attempt"] {
        need(
            producer[field].is_null() || producer[field].is_string(),
            "invalid producer run identity",
        )?;
        if let Some(value) = env(&field.to_ascii_uppercase())? {
            need(
                producer[field].as_str() == Some(value.as_str()),
                format!("producer {field} mismatch"),
            )?;
        }
    }
    Ok(())
}

fn artifacts(out: &Path, rows: &[Value], producer_hash: &str) -> Result<(), String> {
    need(
        !fs::symlink_metadata(out.join("created"))
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink(),
        "created directory symlink",
    )?;
    for row in rows {
        need(
            row["producer_sha256"].as_str() == Some(producer_hash),
            "row producer hash mismatch",
        )?;
        let name = format!(
            "{}-dao-{}",
            row["task"].as_str().ok_or("task")?,
            row["rep"].as_u64().ok_or("rep")?
        );
        let path = out.join("created").join(name);
        let manifest = hashes(&path)?;
        need(
            !manifest.is_empty()
                && serde_json::to_value(&manifest).map_err(|e| e.to_string())?
                    == row["skill_hashes"],
            "generated skill hash mismatch",
        )?;
        need(
            !manifest
                .keys()
                .any(|p| p.to_ascii_lowercase().ends_with(".py")),
            "generated Python helper forbidden",
        )?;
        need(
            valid_skill(&path)? == row["skill_name"].as_str().ok_or("missing skill name")?,
            "generated skill name mismatch",
        )?;
    }
    Ok(())
}

/// Verify producer, exact matrix and created artifacts; caller alone writes the returned receipt.
pub fn check(out: &Path, repo: &Path, tasks: &[String], reps: u64) -> Result<Value, String> {
    let source = regular(&out.join("resume.jsonl"))?;
    let text = std::str::from_utf8(&source).map_err(|e| e.to_string())?;
    let rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    validate_rows(&rows, tasks, reps)?;
    let producer_bytes = regular(&out.join("resume-producer.json"))?;
    let producer: Value = serde_json::from_slice(&producer_bytes).map_err(|e| e.to_string())?;
    let expected = current(
        repo,
        tasks,
        reps,
        &std::env::current_exe().map_err(|e| e.to_string())?,
    )?;
    producer_matches(&producer, &expected)?;
    let producer_hash = digest(&producer_bytes);
    artifacts(out, &rows, &producer_hash)?;
    let mut receipt = producer;
    receipt["schema"] = json!("dao-resume-gate/2-rust");
    receipt["pass"] = json!(true);
    receipt["rows"] = json!(rows.len());
    receipt["producer_sha256"] = json!(producer_hash);
    receipt["results_sha256"] = json!(digest(&source));
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::slice::from_ref;

    fn phase(name: &str) -> Value {
        json!({"rc":0,"error":null,"is_error":false,"turns":2,"subtype":"success","errors":[],"diagnostics_truncated":false,"trace_truncated":false,"skill_calls":[name],"tool_trace":[{"name":"Skill","input":{"skill":name}}],"check":"PASS","final":"Stopped."})
    }
    fn row(task: &str, rep: u64) -> Value {
        let spec = fixture::spec(task).unwrap();
        let mut events = Vec::new();
        for n in 1..=5 {
            events.push(json!({"kind":"attempt","n":n}));
            if spec.first_loads.contains(&n) {
                events.push(json!({"kind":"load","n":n}));
            }
        }
        let first = events.clone();
        if let Some(loaded) = spec.query {
            events.push(json!({"kind":"query","n":5,"loaded":loaded}));
        }
        for n in spec.attempts.iter().skip(5) {
            events.push(json!({"kind":"attempt","n":n}));
            if spec.final_loads.contains(n) {
                events.push(json!({"kind":"load","n":n}));
            }
        }
        json!({"task":task,"arm":"make:dao","rep":rep,"pass":true,"made":true,"clean_stop":true,"s2_loaded_runbook":true,"tampered":false,"reruns":0,"probes":0,"skill_name":"backfill-runbook","after_session1":spec.first_loads,"ran":spec.final_loads,"events_after_s1":first,"events":events,"make":phase("dao-skill-state"),"s1":phase("backfill-runbook"),"s2":phase("backfill-runbook")})
    }
    fn task() -> String {
        fixture::TASKS[0].to_string()
    }
    fn validate(row: Value) -> Result<(), String> {
        validate_rows(&[row], &[task()], 1)
    }
    fn bash(command: &str) -> Value {
        json!({"tool_trace":[{"name":"Bash","input":{"command":command}}]})
    }

    #[test]
    fn full_selected_matrix_and_missing_duplicate_extra() {
        let tasks: Vec<_> = fixture::TASKS.iter().map(|s| s.to_string()).collect();
        let rows: Vec<_> = tasks
            .iter()
            .flat_map(|t| (0..2).map(move |r| row(t, r)))
            .collect();
        assert!(validate_rows(&rows, &tasks, 2).is_ok());
        assert!(validate_rows(&rows[..rows.len() - 1], &tasks, 2).is_err());
        let mut duplicate = rows.clone();
        duplicate[1] = duplicate[0].clone();
        assert!(validate_rows(&duplicate, &tasks, 2).is_err());
        assert!(validate_rows(&rows, &[], 2).is_err());
        assert!(validate_rows(&[], &tasks, 2).is_err());
        assert!(validate_rows(&rows, &[task(), task()], 2).is_err());
        let mut extra = row(&task(), 0);
        extra["rep"] = json!(2);
        assert!(validate(extra).is_err());
    }

    #[test]
    fn result_diagnostics_and_exact_actual_loads_are_required() {
        for p in ["make", "s1", "s2"] {
            for (field, value) in [
                ("rc", json!(1)),
                ("rc", json!(false)),
                ("error", json!("timeout")),
                ("is_error", json!(true)),
                ("turns", json!(0)),
                ("turns", json!(true)),
                ("subtype", json!("error_max_turns")),
                ("errors", json!(["failed"])),
                ("errors", Value::Null),
                ("diagnostics_truncated", json!(true)),
                ("trace_truncated", json!(true)),
                ("tool_trace", json!([])),
                ("skill_calls", json!([])),
            ] {
                let mut r = row(&task(), 0);
                r[p][field] = value;
                assert!(validate(r).is_err(), "{p}.{field}");
            }
            let mut r = row(&task(), 0);
            r[p]["skill_calls"] = json!(["prefix-backfill-runbook-suffix"]);
            assert!(validate(r).is_err());
            let mut r = row(&task(), 0);
            r[p]["tool_trace"][0]["input"]["skill"] = json!("unrelated");
            assert!(validate(r).is_err());
        }
        for field in ["pass", "made", "clean_stop", "s2_loaded_runbook"] {
            let mut r = row(&task(), 0);
            r[field] = json!(false);
            assert!(validate(r).is_err());
        }
        for (field, value) in [
            ("tampered", json!(true)),
            ("reruns", json!(1)),
            ("probes", json!(1)),
            ("rep", json!(true)),
            ("ran", json!([true])),
        ] {
            let mut r = row(&task(), 0);
            r[field] = value;
            assert!(validate(r).is_err());
        }
    }

    #[test]
    fn authoritative_audit_order_and_query_policy() {
        let tasks: Vec<_> = fixture::TASKS.iter().map(|s| s.to_string()).collect();
        for t in tasks {
            let good = row(&t, 0);
            assert!(validate_rows(from_ref(&good), from_ref(&t), 1).is_ok());
            for key in ["events_after_s1", "events"] {
                let mut r = good.clone();
                r[key][0]["n"] = json!(true);
                assert!(validate_rows(&[r], from_ref(&t), 1).is_err());
            }
            let mut r = good.clone();
            r["events"][0]["kind"] = json!("load");
            assert!(validate_rows(&[r], from_ref(&t), 1).is_err());
            if fixture::spec(&t).unwrap().query.is_some() {
                let mut r = good.clone();
                let start = r["events_after_s1"].as_array().unwrap().len();
                let wrong = !r["events"][start]["loaded"].as_bool().unwrap();
                r["events"][start]["loaded"] = json!(wrong);
                assert!(validate_rows(&[r], from_ref(&t), 1).is_err());
                let mut r = good.clone();
                let e = r["events"].as_array_mut().unwrap();
                e.remove(start);
                assert!(validate_rows(&[r], from_ref(&t), 1).is_err());
                if good["events"].as_array().unwrap().len() > start + 1 {
                    let mut r = good.clone();
                    r["events"].as_array_mut().unwrap().swap(start, start + 1);
                    assert!(validate_rows(&[r], from_ref(&t), 1).is_err());
                }
            }
            if fixture::spec(&t).unwrap().no_query {
                let mut r = good.clone();
                r["events"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"kind":"query","n":5,"loaded":false}));
                assert!(validate_rows(&[r], from_ref(&t), 1).is_err());
            }
            if fixture::spec(&t).unwrap().query == Some(false)
                && fixture::spec(&t).unwrap().final_loads.contains(&5)
            {
                let mut r = good.clone();
                r["events"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"kind":"query","n":5,"loaded":true}));
                assert!(validate_rows(&[r], from_ref(&t), 1).is_ok());
            }
        }
    }

    #[test]
    fn observed_boundaries_do_not_confuse_literals_with_execution() {
        for command in [
            "cat /tmp/dao-resume-store-abc/id.log",
            "cat \"$DAO_RESUME_STORE/id.log\"",
            "ls /tmp/warehouse",
            "cd /repo/crates/dao/src/resume_eval && cat gate.rs",
            "cat /tmp/resume-89t6zs8r/prompt.md",
            "python3 -c 'print(1)'",
            "cat <<'PY' | python3\nimport warehouse\nPY",
            "python3 - <<'PY'\nprint('mock')\nPY",
            "printf '%s' harmless > /tmp/dao-resume-store-a/id.log",
        ] {
            assert_eq!(probed(&bash(command)), 1, "{command}");
        }
        for command in ["# do not read /tmp/dao-resume-store-a\nsh status.sh 5","printf '%s\\n' '/tmp/dao-resume-store-a'","printf '%s\\n' find / warehouse.py","cat /tmp/trial/warehouse.txt /tmp/warehouse.txt","cat > /tmp/trial/mock.py <<'PY'\nimport warehouse\nSTORE='/tmp/warehouse'\npython3 forbidden\nPY","cat runbook.md status.sh step.sh","rg -n '/tmp/warehouse' runbook.md","cat .claude/skills/dao-skill-state/references/state-contract.md"] { assert_eq!(probed(&bash(command)),0,"{command}"); }
        let written = json!({"tool_trace":[{"name":"Write","input":{"file_path":"/tmp/trial/mock.py","content":"python3; /tmp/dao-resume-store-a; /repo/crates/dao/src/resume_eval/gate.rs"}}]});
        assert_eq!(probed(&written), 0);
        for name in ["Read", "Write", "Edit", "Glob", "Grep"] {
            let r = json!({"tool_trace":[{"name":name,"input":{"path":"/repo/crates/dao/src/resume_eval","file_path":"/repo/crates/dao/src/resume_eval/gate.rs"}}]});
            assert_eq!(probed(&r), 1);
        }
        assert_eq!(
            probed(&json!({"tool_trace":[{"name":"Read","input":null}]})),
            1
        );
        for phase in ["make", "s1", "s2"] {
            let mut r = row(&task(), 0);
            r[phase]["tool_trace"]
                .as_array_mut()
                .unwrap()
                .push(bash("cat /tmp/dao-resume-store-a/id.log")["tool_trace"][0].clone());
            assert_eq!(r["probes"], 0);
            assert!(validate(r).unwrap_err().contains("probe"));
        }
    }

    #[test]
    fn producer_identity_and_hash_drift_are_red() {
        let expected = json!({"schema":"dao-resume-producer/2-rust","sha":"head","authoring_sha256":"author","state_contract_sha256":"state","binary_sha256":"binary","fixtures":{"gate.rs":"fixture"},"tasks":[task()],"arms":["make:dao"],"reps":1});
        let mut producer = expected.clone();
        producer["budgets"] = json!({"make":{"max_turns":40,"timeout_s":300},"s1":{"max_turns":45,"timeout_s":180},"s2":{"max_turns":45,"timeout_s":180}});
        for field in ["github_run_id", "github_run_attempt"] {
            producer[field] = std::env::var(field.to_ascii_uppercase())
                .map(Value::String)
                .unwrap_or(Value::Null);
        }
        assert!(producer_matches(&producer, &expected).is_ok());
        for field in [
            "schema",
            "sha",
            "authoring_sha256",
            "state_contract_sha256",
            "binary_sha256",
            "fixtures",
            "tasks",
            "arms",
            "reps",
        ] {
            let mut bad = producer.clone();
            bad[field] = json!("changed");
            assert!(producer_matches(&bad, &expected).is_err(), "{field}");
        }
        for value in [json!(0), json!(true), json!(-1), json!(40.0)] {
            let mut bad = producer.clone();
            bad["budgets"]["make"]["max_turns"] = value;
            assert!(producer_matches(&bad, &expected).is_err());
        }
        let mut bad = producer.clone();
        bad["extra"] = json!(true);
        assert!(producer_matches(&bad, &expected).is_err());
        let mut bad = producer.clone();
        bad["github_run_id"] = json!(3);
        assert!(producer_matches(&bad, &expected).is_err());
    }

    #[test]
    fn retained_artifacts_require_producer_hash_and_rust_structure() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let out = std::env::temp_dir().join(format!(
            "dao-gate-test-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let dir = out.join("created").join(format!("{}-dao-0", task()));
        fs::create_dir_all(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(out.clone());
        for malformed in ["", "\n", "{bad JSON}\n", "true\n"] {
            fs::write(out.join("resume.jsonl"), malformed).unwrap();
            assert!(check(&out, Path::new("."), &[task()], 1).is_err());
        }
        fs::write(dir.join("SKILL.md"),"---\nname: backfill-runbook\ndescription: Resume the authorized backfill workflow with durable state.\n---\nUse the runbook.\n").unwrap();
        let mut r = row(&task(), 0);
        r["producer_sha256"] = json!("producer");
        r["skill_hashes"] = serde_json::to_value(hashes(&dir).unwrap()).unwrap();
        assert!(artifacts(&out, from_ref(&r), "producer").is_ok());
        assert!(
            artifacts(&out, from_ref(&r), "producer-with-new-whitespace")
                .unwrap_err()
                .contains("producer hash")
        );
        fs::write(dir.join("SKILL.md"), "tampered").unwrap();
        assert!(artifacts(&out, from_ref(&r), "producer")
            .unwrap_err()
            .contains("skill hash"));
        r["skill_hashes"] = serde_json::to_value(hashes(&dir).unwrap()).unwrap();
        assert!(artifacts(&out, from_ref(&r), "producer").is_err());
        fs::write(dir.join("SKILL.md"),"---\nname: backfill-runbook\ndescription: Resume authorized work.\n---\nUse the runbook.\n").unwrap();
        fs::write(dir.join("helper.py"), "print('not allowed')\n").unwrap();
        r["skill_hashes"] = serde_json::to_value(hashes(&dir).unwrap()).unwrap();
        assert!(artifacts(&out, from_ref(&r), "producer")
            .unwrap_err()
            .contains("Python"));
        #[cfg(unix)]
        {
            fs::remove_file(dir.join("helper.py")).unwrap();
            std::os::unix::fs::symlink(out.join("external"), dir.join("outside")).unwrap();
            assert!(artifacts(&out, &[r], "producer").is_err());
        }
    }
}
