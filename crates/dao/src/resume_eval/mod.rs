//! CI-only, two-session behavior evaluation, separate from the G3 routing evaluation.
//! Owns producer custody and generated skill/workspace artifacts. Depends on fixture, gate,
//! runner and the existing Rust skill checker; called by `dao eval resume` in CI.
//! Invariants: no Python execution, exact source/binary manifests, fresh session HOME, no
//! success on missing/failed evidence, and no change to admission mathematics.

mod fixture;
mod gate;
mod runner;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn temporary(label: &str) -> Result<PathBuf, String> {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = env::temp_dir().join(format!("dao-resume-{label}-{}-{n}", std::process::id()));
    fs::create_dir(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

fn copy_tree(source: &Path, target: &Path, skip_claude: bool) -> Result<(), String> {
    if source.is_symlink() {
        return Err(format!("symlink refused: {}", source.display()));
    }
    fs::create_dir_all(target).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if runner::contains_secret(name.to_string_lossy().as_bytes()) {
            return Err("credential-bearing artifact name refused".into());
        }
        if name == ".git" || name == "__pycache__" || (skip_claude && name == ".claude") {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err(format!("symlink refused: {}", entry.path().display()));
        }
        if kind.is_dir() {
            copy_tree(&entry.path(), &target.join(name), skip_claude)?;
        } else if kind.is_file() {
            if entry.metadata().map_err(|e| e.to_string())?.len() > 32 * 1024 * 1024 {
                return Err("artifact file exceeds 32 MiB".into());
            }
            if runner::contains_secret(&fs::read(entry.path()).map_err(|e| e.to_string())?) {
                return Err("credential-bearing artifact content refused".into());
            }
            fs::copy(entry.path(), target.join(name)).map_err(|e| e.to_string())?;
        } else {
            return Err("non-regular artifact refused".into());
        }
    }
    Ok(())
}

pub(super) fn hashes(root: &Path) -> Result<BTreeMap<String, String>, String> {
    fn visit(
        root: &Path,
        here: &Path,
        output: &mut BTreeMap<String, String>,
    ) -> Result<(), String> {
        if here.is_symlink() {
            return Err("symlink in manifest".into());
        }
        for entry in fs::read_dir(here).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err("symlink in manifest".into());
            }
            if kind.is_dir() {
                visit(root, &entry.path(), output)?;
            } else if kind.is_file() {
                if entry.metadata().map_err(|e| e.to_string())?.len() > 32 * 1024 * 1024 {
                    return Err("manifest file exceeds 32 MiB".into());
                }
                output.insert(
                    entry
                        .path()
                        .strip_prefix(root)
                        .map_err(|e| e.to_string())?
                        .to_string_lossy()
                        .replace('\\', "/"),
                    sha(&fs::read(entry.path()).map_err(|e| e.to_string())?),
                );
            } else {
                return Err("non-regular manifest entry".into());
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result)?;
    Ok(result)
}

pub(super) fn valid_skill(dir: &Path) -> Result<String, String> {
    let manifest = hashes(dir)?;
    if manifest
        .keys()
        .any(|path| path.ends_with(".py") || path.ends_with(".pyc"))
    {
        return Err("this Rust evaluation forbids generated Python helpers".into());
    }
    let text = fs::read_to_string(dir.join("SKILL.md")).map_err(|e| e.to_string())?;
    let regex = regex::Regex::new(r#"(?m)^name:\s*['\"]?([a-z0-9-]{1,64})['\"]?\s*$"#).unwrap();
    let name = regex
        .captures(&text)
        .and_then(|m| m.get(1))
        .ok_or("invalid generated skill name")?
        .as_str()
        .to_string();
    let temp = temporary("lint")?;
    let result = (|| {
        let staged = temp.join(&name);
        copy_tree(dir, &staged, false)?;
        let check = crate::check::check_skill(&staged.to_string_lossy());
        if crate::check::exit_code(&check) != 0 {
            return Err(runner::redact(&crate::check::to_json(&check)));
        }
        Ok(name)
    })();
    let _ = fs::remove_dir_all(temp);
    result
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    crate::admission::durable_write(path, &bytes)
}

fn clean(phase: &Value) -> bool {
    phase["rc"].as_i64() == Some(0)
        && phase["subtype"] == "success"
        && phase["errors"].as_array().is_some_and(|a| a.is_empty())
        && (phase["error"].is_null() || phase["error"] == "")
        && phase["is_error"] != true
        && phase["turns"].as_u64().is_some_and(|n| n > 0)
        && phase["trace_truncated"] == false
        && phase["diagnostics_truncated"] == false
        && phase["tool_trace"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
}

fn loads(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter(|event| event["kind"] == "load")
        .map(|event| event["n"].clone())
        .collect()
}

fn producer(repo: &Path, tasks: &[String], reps: u64, binary: &Path) -> Result<Value, String> {
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .map_err(|e| e.to_string())?;
    if !head.status.success() {
        return Err("cannot bind source HEAD".into());
    }
    let mut fixtures = BTreeMap::new();
    for file in ["mod.rs", "runner.rs", "fixture.rs", "gate.rs"] {
        let path = format!("crates/dao/src/resume_eval/{file}");
        fixtures.insert(
            path.clone(),
            sha(&fs::read(repo.join(path)).map_err(|e| e.to_string())?),
        );
    }
    Ok(
        json!({"schema":"dao-resume-producer/2-rust","sha":String::from_utf8_lossy(&head.stdout).trim(),
        "authoring_sha256":sha(&fs::read(repo.join("dao-skill-state/references/authoring.md")).map_err(|e| e.to_string())?),
        "state_contract_sha256":sha(&fs::read(repo.join("dao-skill-state/references/state-contract.md")).map_err(|e| e.to_string())?),
        "binary_sha256":sha(&fs::read(binary).map_err(|e| e.to_string())?),"fixtures":fixtures,
        "tasks":tasks,"arms":["make:dao"],"reps":reps,
        "budgets":{"make":{"max_turns":40,"timeout_s":300},"s1":{"max_turns":45,"timeout_s":180},"s2":{"max_turns":45,"timeout_s":180}},
        "github_run_id":env::var("GITHUB_RUN_ID").ok(),"github_run_attempt":env::var("GITHUB_RUN_ATTEMPT").ok()}),
    )
}

fn sample(
    repo: &Path,
    out: &Path,
    store: &Path,
    binary: &Path,
    task: &str,
    rep: u64,
    producer_hash: &str,
) -> Value {
    let mut row = json!({"task":task,"arm":"make:dao","rep":rep,"pass":false,"made":false,
        "producer_sha256":producer_hash});
    if let Err(error) = sample_phases(repo, out, store, binary, task, rep, &mut row) {
        row["pass"] = json!(false);
        row["harness_error"] = json!(runner::redact(&error));
    }
    row
}

fn sample_phases(
    repo: &Path,
    out: &Path,
    store: &Path,
    binary: &Path,
    task: &str,
    rep: u64,
    row: &mut Value,
) -> Result<(), String> {
    let tag = format!("{task}-dao-{rep}");
    let maker = temporary("make")?;
    let home = temporary("home")?;
    copy_tree(
        &repo.join("dao-skill-state"),
        &maker.join(".claude/skills/dao-skill-state"),
        false,
    )?;
    let prompt = format!("Use the dao-skill-state skill for this.\n\n{}\n\nWrite the skill into ./new-skill/ (SKILL.md and its required files). Only make the skill; do not perform the real backfill. For this project implement any persistent helper in Rust; short shell wrappers may invoke Rust. Do not run Python. The installed Rust checker is {} check DIR; preserve the requested output folder. Keep synthetic trials inside your own mock inputs and records.",fixture::request(task)?,binary.display());
    let mut make = runner::run(&maker, &home, &prompt, 40, 300, store);
    row["make"] = make.clone();
    row["probes"] = json!(gate::probed(&make));
    let generated = maker.join("new-skill");
    let artifact = out.join("created").join(&tag);
    let valid = valid_skill(&generated);
    if generated.is_dir() {
        copy_tree(&generated, &artifact, false)?;
    }
    if !clean(&make) || valid.is_err() || row["probes"] != json!(0) {
        row["make"]["check"] = json!(valid.err().unwrap_or_else(|| "maker CLI failed".into()));
        return Ok(());
    }
    let skill = valid.unwrap();
    row["made"] = json!(true);
    row["skill_name"] = json!(skill);
    row["skill_hashes"] = json!(hashes(&artifact)?);
    make["check"] = json!("valid Rust-checked skill");
    row["make"] = make;
    let ws1 = temporary("ws")?;
    let id = sha(format!("{tag}:{}", ws1.display()).as_bytes())[..16].to_string();
    let baseline = fixture::setup(task, &ws1, binary, store, &id)?;
    let case_out = out.join("workspaces").join(&tag);
    fs::create_dir_all(&case_out).map_err(|e| e.to_string())?;
    write_json(&case_out.join("fixture-baseline.json"), &baseline)?;
    copy_tree(&artifact, &ws1.join(".claude/skills").join(&skill), false)?;
    let mut s1 = runner::run(
        &ws1,
        &temporary("home")?,
        &format!("Use the {skill} skill.\n\n{}", fixture::first_prompt(task)?),
        45,
        180,
        store,
    );
    row["s1"] = s1.clone();
    let first_check = fixture::verify(task, &ws1, store, &baseline, true);
    s1["check"] = json!(first_check
        .as_ref()
        .err()
        .cloned()
        .unwrap_or_else(|| "pass".into()));
    let first = fixture::events(&ws1, store)?;
    let after = loads(&first);
    row["s1"] = s1.clone();
    row["after_session1"] = json!(after);
    row["events_after_s1"] = json!(first);
    row["clean_stop"] = json!(first_check.is_ok());
    row["tampered"] = json!(first_check.is_err());
    row["probes"] = json!(gate::probed(&row["make"]) + gate::probed(&s1));
    copy_tree(&ws1, &out.join("workspaces").join(&tag).join("s1"), true)?;
    if !clean(&s1) || first_check.is_err() || row["probes"] != json!(0) {
        return Ok(());
    }
    fixture::transition(task, &ws1)?;
    let ws2 = temporary("ws")?;
    copy_tree(&ws1, &ws2, true)?;
    copy_tree(&artifact, &ws2.join(".claude/skills").join(&skill), false)?;
    let mut s2 = runner::run(
        &ws2,
        &temporary("home")?,
        &format!(
            "Use the {skill} skill.\n\n{}",
            fixture::resume_prompt(task)?
        ),
        45,
        180,
        store,
    );
    row["s2"] = s2.clone();
    let final_check = fixture::verify(task, &ws2, store, &baseline, false);
    s2["check"] = json!(final_check
        .as_ref()
        .err()
        .cloned()
        .unwrap_or_else(|| "pass".into()));
    let events = fixture::events(&ws2, store)?;
    let ran = loads(&events);
    let unique = ran
        .iter()
        .filter_map(Value::as_u64)
        .collect::<BTreeSet<_>>()
        .len();
    row["s2"] = s2.clone();
    row["events"] = json!(events);
    row["ran"] = json!(ran);
    row["reruns"] = json!(ran.len() - unique);
    row["s2_loaded_runbook"] = json!(s2["skill_calls"].as_array().is_some_and(|calls| calls
        .iter()
        .any(|call| call == &json!(skill) || call == &json!(format!("read:{skill}")))));
    row["probes"] = json!(gate::probed(&row["make"]) + gate::probed(&s1) + gate::probed(&s2));
    row["pass"] = json!(final_check.is_ok() && clean(&s2) && row["probes"] == json!(0));
    copy_tree(&ws2, &out.join("workspaces").join(&tag).join("s2"), true)?;
    Ok(())
}

fn run(repo: &Path, out: &Path, tasks: &[String], reps: u64) -> Result<(), String> {
    if env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        return Err("model evaluations run in GitHub CI only".into());
    }
    if out.exists() {
        return Err("results directory must be new (no stale rows)".into());
    }
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let binary = env::current_exe().map_err(|e| e.to_string())?;
    let manifest = producer(repo, tasks, reps, &binary)?;
    write_json(&out.join("resume-producer.json"), &manifest)?;
    let digest = sha(&fs::read(out.join("resume-producer.json")).map_err(|e| e.to_string())?);
    let store = temporary("store")?;
    let mut log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out.join("resume.jsonl"))
        .map_err(|e| e.to_string())?;
    'matrix: for task in tasks {
        for rep in 0..reps {
            let row = sample(repo, out, &store, &binary, task, rep, &digest);
            writeln!(log, "{}", runner::redact_json(row.clone())).map_err(|e| e.to_string())?;
            log.flush().map_err(|e| e.to_string())?;
            log.sync_all().map_err(|e| e.to_string())?;
            println!(
                "{}",
                json!({"task":task,"rep":rep,"pass":row["pass"],"made":row["made"],"after_session1":row["after_session1"],"ran":row["ran"],"probes":row["probes"],"harness_error":row["harness_error"]})
            );
            if row["pass"] != true {
                break 'matrix;
            } // Preserve the RED, avoid spending on a doomed matrix.
        }
    }
    copy_tree(&store, &out.join("external"), false)?;
    match gate::check(out, repo, tasks, reps) {
        Ok(receipt) => write_json(&out.join("resume-gate.json"), &receipt),
        Err(error) => {
            write_json(
                &out.join("resume-gate.json"),
                &json!({"pass":false,"error":runner::redact(&error)}),
            )?;
            Err(error)
        }
    }
}

pub(super) fn main(args: &[String]) -> Result<ExitCode, String> {
    let (sub, rest) = args
        .split_first()
        .ok_or("resume needs run, check or system")?;
    if sub == "system" {
        return fixture::system(rest).map(|code| ExitCode::from(code as u8));
    }
    if !matches!(sub.as_str(), "run" | "check") {
        return Err("unknown resume subcommand".into());
    }
    let mut repo = env::current_dir().map_err(|e| e.to_string())?;
    let mut out = None;
    let mut tasks: Vec<String> = fixture::TASKS.iter().map(|task| task.to_string()).collect();
    let mut reps = 2;
    let mut args = rest.iter();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--repo" => repo = PathBuf::from(value),
            "--out" => out = Some(PathBuf::from(value)),
            "--tasks" => tasks = value.split(',').map(str::to_string).collect(),
            "--reps" => reps = value.parse::<u64>().map_err(|e| e.to_string())?,
            _ => return Err(format!("unknown resume argument: {flag}")),
        }
    }
    if reps == 0
        || reps > 10
        || tasks.is_empty()
        || tasks.len() != tasks.iter().collect::<BTreeSet<_>>().len()
        || tasks
            .iter()
            .any(|task| !fixture::TASKS.contains(&task.as_str()))
    {
        return Err("invalid resume matrix".into());
    }
    let repo = fs::canonicalize(repo).map_err(|e| e.to_string())?;
    let out = out.ok_or("resume requires --out DIR")?;
    if sub == "run" {
        run(&repo, &out, &tasks, reps)?;
    } else {
        match gate::check(&out, &repo, &tasks, reps) {
            Ok(receipt) => write_json(&out.join("resume-gate.json"), &receipt)?,
            Err(error) => {
                write_json(
                    &out.join("resume-gate.json"),
                    &json!({"pass":false,"error":runner::redact(&error)}),
                )?;
                return Err(error);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn secret_artifacts_are_refused_and_partial_maker_evidence_survives() {
        const FLAG: &str = "DAO_RESUME_ARTIFACT_TEST_CHILD";
        if env::var_os(FLAG).is_some() {
            let root = temporary("artifact-test").unwrap();
            let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let row = sample(
                &repo,
                &root,
                &root,
                &env::current_exe().unwrap(),
                fixture::TASKS[0],
                0,
                "producer",
            );
            assert_eq!(row["pass"], false);
            assert!(row["harness_error"]
                .as_str()
                .unwrap()
                .contains("credential-bearing"));
            assert_eq!(row["make"]["subtype"], "success");
            assert_eq!(row["make"]["tool_trace"][0]["name"], "Skill");
            assert!(!root
                .join("created")
                .join(format!("{}-dao-0", fixture::TASKS[0]))
                .join("debug.txt")
                .exists());
            fs::remove_dir_all(root).unwrap();
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let bin = temporary("fake-cli").unwrap();
        let cli = bin.join("claude");
        fs::write(&cli,r#"#!/bin/sh
mkdir -p new-skill
cat > new-skill/SKILL.md <<'DOC'
---
name: backfill-runbook
description: Resume authorized backfill work with durable state.
---
Use the runbook.
DOC
printf '%s' "$ANTHROPIC_AUTH_TOKEN" > new-skill/debug.txt
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Skill","input":{"skill":"dao-skill-state"}}]}}' '{"type":"result","subtype":"success","errors":[],"is_error":false,"num_turns":1}'
"#).unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        let status = Command::new(env::current_exe().unwrap())
            .args(["--exact","resume_eval::tests::secret_artifacts_are_refused_and_partial_maker_evidence_survives"])
            .env(FLAG,"1").env("ANTHROPIC_AUTH_TOKEN","synthetic-ci-credential-abcdef")
            .env("PATH",format!("{}:{}",bin.display(),env::var("PATH").unwrap_or_default()))
            .status().unwrap();
        fs::remove_dir_all(bin).unwrap();
        assert!(status.success());
    }
}
