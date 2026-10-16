//! Claude subprocess custody for the CI-only resume evaluation.
//! Depends on std process/threads and serde_json; called only by resume_eval.
//! Invariants: fresh HOME, environment allowlist, bounded redacted evidence, concurrent pipe
//! draining, a deadline and process-group cleanup. This is observation, not a security sandbox.

use serde_json::{json, Value};
use std::env;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const TRACE_LIMIT: usize = 256 * 1024;

fn drain(mut pipe: impl Read + Send + 'static, limit: usize) -> mpsc::Receiver<(Vec<u8>, bool)> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut truncated = false;
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let take = n.min(limit.saturating_sub(kept.len()));
                    kept.extend_from_slice(&buf[..take]);
                    truncated |= take != n;
                }
                Err(_) => { truncated = true; break; }
            }
        }
        let _ = tx.send((kept, truncated));
    });
    rx
}

fn capture(mut command: Command, seconds: u64) -> (Vec<u8>, Value, bool) {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return (vec![], json!("spawn error"), true),
    };
    let stdout = drain(child.stdout.take().unwrap(), OUTPUT_LIMIT);
    let stderr = drain(child.stderr.take().unwrap(), 0); // Drain, never retain raw stderr.
    let start = Instant::now();
    let rc = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().map_or(json!("signal"), |n| json!(n)),
            Ok(None) if start.elapsed() < Duration::from_secs(seconds) => {
                thread::sleep(Duration::from_millis(50));
            }
            _ => { let _ = child.kill(); break json!("timeout"); }
        }
    };
    // Also reap background descendants that kept a pipe open after the CLI exited.
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = child.wait();
    let result = stdout.recv_timeout(Duration::from_secs(2)).unwrap_or((vec![], true));
    let _ = stderr.recv_timeout(Duration::from_secs(2));
    (result.0, rc, result.1)
}

pub(super) fn redact(text: &str) -> String {
    let mut result = text.to_string();
    for (key, value) in env::vars() {
        if (key.starts_with("ANTHROPIC_") || key.starts_with("CLAUDE_CODE_")) && value.len() >= 8 {
            result = result.replace(&value, "[REDACTED]");
        }
    }
    result
}

/// Parses only Claude's documented stream-json event/result fields. Missing/truncated evidence
/// stays explicit; a written output cannot turn a failed CLI result into a clean phase.
fn evidence(bytes: &[u8], rc: Value, mut truncated: bool, elapsed: f64) -> Value {
    truncated |= std::str::from_utf8(bytes).is_err();
    let mut result = Value::Null;
    let mut trace = Vec::new();
    let mut calls = Vec::new();
    let mut trace_bytes = 0;
    for line in String::from_utf8_lossy(bytes).lines().filter(|line| !line.trim().is_empty()) {
        let event: Value = match serde_json::from_str(line) {
            Ok(event) => event,
            Err(_) => { truncated = true; continue; }
        };
        if event["type"] == "result" { result = event.clone(); }
        if let Some(content) = event["message"]["content"].as_array() {
            for item in content.iter().filter(|item| item["type"] == "tool_use") {
                let entry: Value = serde_json::from_str(&redact(&json!({
                    "name": item["name"], "input": item.get("input").cloned().unwrap_or(json!({}))
                }).to_string())).unwrap();
                trace_bytes += entry.to_string().len();
                if trace_bytes <= TRACE_LIMIT { trace.push(entry.clone()); } else { truncated = true; }
                if entry["name"] == "Skill" {
                    calls.push(entry["input"]["skill"].as_str().or(entry["input"]["command"].as_str()).unwrap_or("?").to_string());
                } else if entry["name"] == "Read" {
                    if let Some(path) = entry["input"]["file_path"].as_str() {
                        if let Some((_, suffix)) = path.split_once("/.claude/skills/") {
                            calls.push(format!("read:{}", suffix.split('/').next().unwrap_or("?")));
                        }
                    }
                }
            }
        }
    }
    let diagnostics = redact(&json!({"subtype":result["subtype"], "errors":result.get("errors").cloned().unwrap_or(json!([]))}).to_string());
    let diagnostics_truncated = diagnostics.len() > 16 * 1024;
    let diagnostic: Value = if diagnostics_truncated {
        json!({"subtype":redact(result["subtype"].as_str().unwrap_or("unknown")),"errors":["diagnostics exceeded limit"]})
    } else { serde_json::from_str(&diagnostics).unwrap() };
    let final_text: String = redact(result["result"].as_str().unwrap_or("")).chars().take(600).collect();
    let error = if rc != json!(0) || result.is_null() || result["is_error"] == true {
        Some(format!("CLI failed: rc={rc}, result_present={}", !result.is_null()))
    } else { None };
    json!({"rc":rc,"is_error":result["is_error"],"turns":result["num_turns"],
        "skill_calls":calls,"tool_trace":trace,"trace_truncated":truncated,
        "subtype":diagnostic["subtype"],"errors":diagnostic["errors"],
        "diagnostics_truncated":diagnostics_truncated,"error":error,"final":final_text,
        "check":null,"duration_s":elapsed,"input_tokens":result["usage"]["input_tokens"],
        "output_tokens":result["usage"]["output_tokens"],"cache_read":result["usage"]["cache_read_input_tokens"],
        "cache_write":result["usage"]["cache_creation_input_tokens"]})
}

pub(super) fn run(ws: &Path, home: &Path, prompt: &str, turns: u64, seconds: u64, store: &Path) -> Value {
    let mut command = Command::new("claude");
    command.args(["-p",prompt,"--output-format","stream-json","--verbose","--max-turns",&turns.to_string(),"--dangerously-skip-permissions"])
        .current_dir(ws).env_clear().env("HOME",home).env("DAO_RESUME_STORE",store);
    for (key, value) in env::vars() {
        if matches!(key.as_str(), "PATH"|"LANG"|"LC_ALL"|"TMPDIR"|"NODE_PATH"|"RUSTUP_HOME"|"CARGO_HOME"|"RUSTUP_TOOLCHAIN")
            || key.starts_with("ANTHROPIC_") || key.starts_with("CLAUDE_CODE_") || key == "DISABLE_AUTOUPDATER" {
            command.env(key,value);
        }
    }
    // rustup's shims must still find the installed toolchain under a fresh model HOME.
    if let Ok(original) = env::var("HOME") {
        for (key, folder) in [("RUSTUP_HOME",".rustup"),("CARGO_HOME",".cargo")] {
            if env::var_os(key).is_none() { command.env(key,Path::new(&original).join(folder)); }
        }
    }
    let start = Instant::now();
    let (stdout,rc,truncated) = capture(command,seconds);
    evidence(&stdout,rc,truncated,start.elapsed().as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_result_retains_complete_tool_input_and_diagnostics() {
        let input = format!("prefix {} suffix", "x".repeat(250));
        let stream = format!("{}\n{}\n",json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":input}}]}}),json!({"type":"result","subtype":"error_max_turns","errors":["limit"],"is_error":true,"num_turns":2}));
        let row = evidence(stream.as_bytes(),json!(1),false,0.1);
        assert_eq!(row["tool_trace"][0]["input"]["command"],input);
        assert_eq!(row["subtype"],"error_max_turns");
        assert_eq!(row["errors"],json!(["limit"]));
        assert!(row["error"].is_string());
    }

    #[test]
    fn omitted_or_malformed_evidence_never_claims_complete() {
        let row = evidence(b"not-json\n",json!(0),false,0.1);
        assert_eq!(row["trace_truncated"],true);
        assert!(row["error"].is_string());
        let stream = json!({"message":{"content":[{"type":"tool_use","name":"Write","input":{"content":"x".repeat(TRACE_LIMIT+1)}}]}}).to_string();
        assert_eq!(evidence(stream.as_bytes(),json!(0),false,0.1)["trace_truncated"],true);
    }

    #[cfg(unix)]
    #[test]
    fn capture_drains_both_pipes_and_deadline_reaps_background_children() {
        let mut noisy = Command::new("sh");
        noisy.args(["-c","head -c 150000 /dev/zero; head -c 150000 /dev/zero >&2"]);
        let (output,rc,truncated) = capture(noisy,5);
        assert_eq!(output.len(),150000);
        assert_eq!(rc,json!(0));
        assert!(!truncated);
        let mut hanging = Command::new("sh");
        hanging.args(["-c","sleep 20 & wait"]);
        let start = Instant::now();
        let (_,rc,_) = capture(hanging,1);
        assert_eq!(rc,json!("timeout"));
        assert!(start.elapsed() < Duration::from_secs(6));
    }
}
