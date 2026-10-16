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
const DETAIL_LIMIT: usize = 16 * 1024;
const READ_LIMIT: usize = 4096;

#[derive(Default)]
struct PipeCapture {
    bytes: Vec<u8>,
    truncated: bool,
    reads: Vec<(usize, f64)>,
    timing_truncated: bool,
    first_s: Option<f64>,
    last_s: Option<f64>,
}

fn drain(
    mut pipe: impl Read + Send + 'static,
    limit: usize,
    start: Instant,
) -> mpsc::Receiver<PipeCapture> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut kept = PipeCapture::default();
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let at = start.elapsed().as_secs_f64();
                    kept.first_s.get_or_insert(at);
                    kept.last_s = Some(at);
                    let take = n.min(limit.saturating_sub(kept.bytes.len()));
                    kept.bytes.extend_from_slice(&buf[..take]);
                    kept.truncated |= take != n;
                    if take > 0 {
                        if kept.reads.len() < READ_LIMIT {
                            kept.reads.push((kept.bytes.len(), at));
                        } else {
                            kept.timing_truncated = true;
                        }
                    }
                }
                Err(_) => {
                    kept.truncated = true;
                    break;
                }
            }
        }
        let _ = tx.send(kept);
    });
    rx
}

fn capture(mut command: Command, seconds: u64) -> (PipeCapture, Value, Value) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            return (
                PipeCapture {
                    truncated: true,
                    ..PipeCapture::default()
                },
                json!("spawn error"),
                json!({"spawn_failed":true}),
            );
        }
    };
    let start = Instant::now();
    let stdout = drain(child.stdout.take().unwrap(), OUTPUT_LIMIT, start);
    let stderr = drain(child.stderr.take().unwrap(), DETAIL_LIMIT, start);
    let rc = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().map_or(json!("signal"), |n| json!(n)),
            Ok(None) if start.elapsed() < Duration::from_secs(seconds) => {
                thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                break json!("timeout");
            }
        }
    };
    let stopped_s = start.elapsed().as_secs_f64();
    // Also reap background descendants that kept a pipe open after the CLI exited.
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.wait();
    let missing = || PipeCapture {
        truncated: true,
        ..PipeCapture::default()
    };
    let output = stdout
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_else(|_| missing());
    let errors = stderr
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_else(|_| missing());
    // A byte cap can bisect a credential; never publish the retained prefix in that case.
    let stderr_truncated = errors.truncated || std::str::from_utf8(&errors.bytes).is_err();
    let stderr_text = if stderr_truncated {
        "stderr omitted: incomplete capture".to_string()
    } else {
        redact(&String::from_utf8_lossy(&errors.bytes))
    };
    let observation = json!({
        "clock":"monotonic seconds since spawn; pipe receipt and exit/deadline observation, not tool execution time",
        "stdout_first_s":output.first_s,"stdout_last_s":output.last_s,
        "stdout_timing_truncated":output.timing_truncated,"process_stopped_s":stopped_s,
        "stderr":stderr_text,"stderr_truncated":stderr_truncated
    });
    (output, rc, observation)
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

/// Refuse credential-bearing artifact bytes before anything reaches the upload directory.
pub(super) fn contains_secret(bytes: &[u8]) -> bool {
    env::vars().any(|(key, value)| {
        (key.starts_with("ANTHROPIC_") || key.starts_with("CLAUDE_CODE_"))
            && ["TOKEN", "KEY", "SECRET"]
                .iter()
                .any(|part| key.contains(part))
            && !value.is_empty()
            && bytes
                .windows(value.len())
                .any(|window| window == value.as_bytes())
    })
}

/// Parses only Claude's documented stream-json event/result fields. Missing/truncated evidence
/// stays explicit; a written output cannot turn a failed CLI result into a clean phase.
fn evidence(
    bytes: &[u8],
    rc: Value,
    mut truncated: bool,
    elapsed: f64,
    reads: &[(usize, f64)],
) -> Value {
    truncated |= std::str::from_utf8(bytes).is_err();
    let mut result = Value::Null;
    let mut trace = Vec::new();
    let mut calls = Vec::new();
    let mut trace_bytes = 0;
    let mut tool_results = Vec::new();
    let mut messages = Vec::new();
    let mut detail_bytes = 0;
    let mut details_truncated = false;
    let mut offset = 0;
    for line in String::from_utf8_lossy(bytes).split_inclusive('\n') {
        offset += line.len();
        if line.trim().is_empty() {
            continue;
        }
        let observed_s = reads
            .iter()
            .find(|(end, _)| *end >= offset)
            .map(|(_, at)| *at);
        let event: Value = match serde_json::from_str(line) {
            Ok(event) => event,
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        if event["type"] == "result" {
            result = event.clone();
        }
        if event["type"] == "assistant" {
            let entry = json!({"message_id":event["message"]["id"],
                "model":event["message"]["model"],"usage":event["message"]["usage"],
                "stop_reason":event["message"]["stop_reason"],"error":event["error"],
                "observed_s":observed_s});
            let text = redact(&entry.to_string());
            detail_bytes += text.len();
            if detail_bytes <= TRACE_LIMIT {
                messages.push(serde_json::from_str::<Value>(&text).unwrap());
            } else {
                details_truncated = true;
            }
        }
        if let Some(content) = event["message"]["content"].as_array() {
            for item in content.iter().filter(|item| item["type"] == "tool_result") {
                let text = redact(&item["content"].to_string());
                let content_truncated = text.len() > DETAIL_LIMIT;
                let value = if content_truncated {
                    json!({"excerpt":text.chars().take(DETAIL_LIMIT / 4).collect::<String>()})
                } else {
                    serde_json::from_str::<Value>(&text).unwrap()
                };
                let entry = json!({"tool_use_id":redact(item["tool_use_id"].as_str().unwrap_or("")),
                    "is_error":item["is_error"],"content":value,
                    "content_truncated":content_truncated,"observed_s":observed_s});
                detail_bytes += entry.to_string().len();
                details_truncated |= content_truncated;
                if detail_bytes <= TRACE_LIMIT {
                    tool_results.push(entry);
                } else {
                    details_truncated = true;
                }
            }
            for item in content.iter().filter(|item| item["type"] == "tool_use") {
                if !item["name"].as_str().is_some_and(|name| !name.is_empty())
                    || !item["input"].is_object()
                {
                    truncated = true;
                }
                let entry: Value = serde_json::from_str(&redact(
                    &json!({
                        "id":item["id"],"name": item["name"], "input": item["input"],
                        "observed_s":observed_s
                    })
                    .to_string(),
                ))
                .unwrap();
                trace_bytes += entry.to_string().len();
                if trace_bytes <= TRACE_LIMIT {
                    trace.push(entry.clone());
                } else {
                    truncated = true;
                }
                if entry["name"] == "Skill" {
                    calls.push(
                        entry["input"]["skill"]
                            .as_str()
                            .or(entry["input"]["command"].as_str())
                            .unwrap_or("?")
                            .to_string(),
                    );
                } else if entry["name"] == "Read" {
                    if let Some(path) = entry["input"]["file_path"].as_str() {
                        if let Some((_, suffix)) = path.split_once("/.claude/skills/") {
                            if let Some((name, "SKILL.md")) = suffix.split_once('/') {
                                if !name.is_empty() && name != "." && name != ".." {
                                    calls.push(format!("read:{name}"));
                                }
                            }
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
    } else {
        serde_json::from_str(&diagnostics).unwrap()
    };
    let final_text: String = redact(result["result"].as_str().unwrap_or(""))
        .chars()
        .take(600)
        .collect();
    let error = if rc != json!(0) || result.is_null() || result["is_error"] == true {
        Some(format!(
            "CLI failed: rc={rc}, result_present={}",
            !result.is_null()
        ))
    } else {
        None
    };
    json!({"rc":rc,"is_error":result["is_error"],"turns":result["num_turns"],
        "skill_calls":calls,"tool_trace":trace,"trace_truncated":truncated,
        "tool_results":tool_results,"message_observations":messages,"details_truncated":details_truncated,
        "cli_duration_ms":result["duration_ms"],"cli_duration_api_ms":result["duration_api_ms"],
        "subtype":diagnostic["subtype"],"errors":diagnostic["errors"],
        "diagnostics_truncated":diagnostics_truncated,"error":error,"final":final_text,
        "check":null,"duration_s":elapsed,"input_tokens":result["usage"]["input_tokens"],
        "output_tokens":result["usage"]["output_tokens"],"cache_read":result["usage"]["cache_read_input_tokens"],
        "cache_write":result["usage"]["cache_creation_input_tokens"]})
}

pub(super) fn run(
    ws: &Path,
    home: &Path,
    prompt: &str,
    turns: u64,
    seconds: u64,
    store: &Path,
) -> Value {
    let mut command = Command::new("claude");
    command
        .args([
            "-p",
            prompt,
            "--output-format",
            "stream-json",
            "--verbose",
            "--max-turns",
            &turns.to_string(),
            "--dangerously-skip-permissions",
        ])
        .current_dir(ws)
        .env_clear()
        .env("HOME", home)
        .env("DAO_RESUME_STORE", store);
    for (key, value) in env::vars() {
        if matches!(
            key.as_str(),
            "PATH"
                | "LANG"
                | "LC_ALL"
                | "TMPDIR"
                | "NODE_PATH"
                | "RUSTUP_HOME"
                | "CARGO_HOME"
                | "RUSTUP_TOOLCHAIN"
        ) || key.starts_with("ANTHROPIC_")
            || key.starts_with("CLAUDE_CODE_")
            || key == "DISABLE_AUTOUPDATER"
        {
            command.env(key, value);
        }
    }
    // rustup's shims must still find the installed toolchain under a fresh model HOME.
    if let Ok(original) = env::var("HOME") {
        for (key, folder) in [("RUSTUP_HOME", ".rustup"), ("CARGO_HOME", ".cargo")] {
            if env::var_os(key).is_none() {
                command.env(key, Path::new(&original).join(folder));
            }
        }
    }
    let start = Instant::now();
    let (stdout, rc, observation) = capture(command, seconds);
    let mut row = evidence(
        &stdout.bytes,
        rc,
        stdout.truncated,
        start.elapsed().as_secs_f64(),
        &stdout.reads,
    );
    row["io_observation"] = observation;
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_retains_correlated_tool_outcomes() {
        let stream = format!(
            "{}\n{}\n",
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"compile-1","name":"Bash","input":{"command":"rustc helper.rs"}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"compile-1","is_error":true,"content":"error[E0382]: borrow of moved value"}]}})
        );
        let row = evidence(stream.as_bytes(), json!("timeout"), false, 300.0, &[]);
        assert_eq!(row["tool_trace"][0]["id"], "compile-1");
        assert_eq!(row["tool_results"][0]["tool_use_id"], "compile-1");
        assert_eq!(row["tool_results"][0]["is_error"], true);
        assert_eq!(
            row["tool_results"][0]["content"],
            "error[E0382]: borrow of moved value"
        );
        assert!(row["error"].is_string());
    }

    #[test]
    fn interleaved_results_keep_ids_receipt_times_and_bounded_details() {
        let first = format!("{}\n", json!({"type":"assistant","message":{"id":"m1","model":"test-model","usage":{"input_tokens":12},"content":[
            {"type":"tool_use","id":"a","name":"Bash","input":{"command":"one"}},
            {"type":"tool_use","id":"b","name":"Bash","input":{"command":"two"}}]}}));
        let second = format!("{}\n", json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"b","content":[{"type":"text","text":"two done"}]},
            {"type":"tool_result","tool_use_id":"a","content":"x".repeat(DETAIL_LIMIT + 1)}]}}));
        let stream = first.clone() + &second;
        let row = evidence(
            stream.as_bytes(),
            json!("timeout"),
            false,
            4.0,
            &[(first.len() / 2, 0.5), (first.len(), 1.0), (stream.len(), 3.0)],
        );
        assert_eq!(row["tool_trace"][0]["observed_s"], 1.0);
        assert_eq!(row["tool_results"][0]["tool_use_id"], "b");
        assert_eq!(row["tool_results"][0]["observed_s"], 3.0);
        assert_eq!(row["tool_results"][0]["content"][0]["text"], "two done");
        assert_eq!(row["tool_results"][1]["tool_use_id"], "a");
        assert_eq!(row["tool_results"][1]["content_truncated"], true);
        assert_eq!(row["message_observations"][0]["usage"]["input_tokens"], 12);
        assert_eq!(row["details_truncated"], true);
        assert!(row.to_string().len() < TRACE_LIMIT);
    }

    #[cfg(unix)]
    #[test]
    fn capture_preserves_partial_events_and_redacts_returned_secrets() {
        const FLAG: &str = "DAO_RESUME_DIAGNOSTIC_TEST_CHILD";
        if env::var_os(FLAG).is_some() {
            let secret = env::var("ANTHROPIC_AUTH_TOKEN").unwrap();
            let mut command = Command::new("sh");
            command.args(["-c", "printf '%s' '{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"x\",\"content\":\"'; printf '%s' \"$ANTHROPIC_AUTH_TOKEN\"; printf '%s\\n' '\"}]}}'; printf '%s' \"$ANTHROPIC_AUTH_TOKEN\" >&2; sleep 20 & wait"]);
            let (output, rc, observation) = capture(command, 1);
            let row = evidence(&output.bytes, rc, output.truncated, 1.0, &output.reads);
            assert_eq!(row["rc"], "timeout");
            assert_eq!(row["tool_results"][0]["content"], "[REDACTED]");
            assert!(!row.to_string().contains(&secret));
            assert_eq!(observation["stderr"], "[REDACTED]");
            assert!(observation["process_stopped_s"].as_f64().unwrap() >= 1.0);
            assert!(row["tool_results"][0]["observed_s"].as_f64().unwrap()
                < observation["process_stopped_s"].as_f64().unwrap());
            return;
        }
        let status = Command::new(env::current_exe().unwrap())
            .args(["--exact", "resume_eval::runner::tests::capture_preserves_partial_events_and_redacts_returned_secrets"])
            .env(FLAG, "1")
            .env("ANTHROPIC_AUTH_TOKEN", "synthetic-ci-secret-abcdef")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn failed_result_retains_complete_tool_input_and_diagnostics() {
        let input = format!("prefix {} suffix", "x".repeat(250));
        let stream = format!(
            "{}\n{}\n",
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":input}}]}}),
            json!({"type":"result","subtype":"error_max_turns","errors":["limit"],"is_error":true,"num_turns":2})
        );
        let row = evidence(stream.as_bytes(), json!(1), false, 0.1, &[]);
        assert_eq!(row["tool_trace"][0]["input"]["command"], input);
        assert_eq!(row["subtype"], "error_max_turns");
        assert_eq!(row["errors"], json!(["limit"]));
        assert!(row["error"].is_string());
    }

    #[test]
    fn omitted_or_malformed_evidence_never_claims_complete() {
        let row = evidence(b"not-json\n", json!(0), false, 0.1, &[]);
        assert_eq!(row["trace_truncated"], true);
        assert!(row["error"].is_string());
        let stream = json!({"message":{"content":[{"type":"tool_use","name":"Write","input":{"content":"x".repeat(TRACE_LIMIT+1)}}]}}).to_string();
        assert_eq!(
            evidence(stream.as_bytes(), json!(0), false, 0.1, &[])["trace_truncated"],
            true
        );
    }

    #[test]
    fn malformed_tool_records_never_claim_complete() {
        for item in [
            json!({"type":"tool_use","input":{}}),
            json!({"type":"tool_use","name":"","input":{}}),
            json!({"type":"tool_use","name":"Bash"}),
            json!({"type":"tool_use","name":"Bash","input":null}),
            json!({"type":"tool_use","name":"Bash","input":"command"}),
        ] {
            let stream = format!(
                "{}\n{}\n",
                json!({"type":"assistant","message":{"content":[item.clone()]}}),
                json!({"type":"result","subtype":"success","errors":[],"is_error":false,"num_turns":1})
            );
            assert_eq!(
                evidence(stream.as_bytes(), json!(0), false, 0.1, &[])["trace_truncated"],
                true,
                "malformed native tool record: {item}"
            );
        }
    }

    #[test]
    fn skill_read_evidence_requires_the_entry_file() {
        for (path, expected) in [
            ("/ws/.claude/skills/target/SKILL.md", json!(["read:target"])),
            ("/ws/.claude/skills/target/helper.rs", json!([])),
            ("/ws/.claude/skills/target/references/SKILL.md", json!([])),
            ("/ws/.claude/skills/target/../SKILL.md", json!([])),
        ] {
            let stream = json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":path}}]}}).to_string();
            assert_eq!(
                evidence(stream.as_bytes(), json!(0), false, 0.1, &[])["skill_calls"],
                expected,
                "{path}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn capture_drains_both_pipes_and_deadline_reaps_background_children() {
        let mut noisy = Command::new("sh");
        noisy.args([
            "-c",
            "head -c 150000 /dev/zero; head -c 150000 /dev/zero >&2",
        ]);
        let (output, rc, observation) = capture(noisy, 5);
        assert_eq!(output.bytes.len(), 150000);
        assert_eq!(rc, json!(0));
        assert!(!output.truncated);
        assert_eq!(observation["stderr_truncated"], true);
        let mut hanging = Command::new("sh");
        hanging.args(["-c", "sleep 20 & wait"]);
        let start = Instant::now();
        let (_, rc, _) = capture(hanging, 1);
        assert_eq!(rc, json!("timeout"));
        assert!(start.elapsed() < Duration::from_secs(6));
    }
}
