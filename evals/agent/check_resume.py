#!/usr/bin/env python3
"""CI resume gate; consumes harness rows/created skills and create.structure_ok; never runs a model."""
import argparse
import hashlib
import itertools
import json
import os
from pathlib import Path
import re
import subprocess
import sys

REPO = Path(__file__).resolve().parents[2]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def hashes(root):
    require(not root.is_symlink() and root.is_dir(), f"missing directory or symlink: {root}")
    result = {}
    for path in sorted(root.rglob("*")):
        require(not path.is_symlink(), f"symlink in manifest: {path}")
        if path.is_file() and "__pycache__" not in path.parts:
            result[path.relative_to(root).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def validate_rows(rows, tasks, arms, reps, tasks_dir):
    """Validate an exact selected matrix and authoritative event order; artifact checks belong to CLI."""
    require(type(reps) is int and reps > 0, "reps must be positive")
    for values in (tasks, arms):
        require(values and len(set(values)) == len(values), "empty or duplicate selection")
    require(all(re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_-]*", t) for t in tasks), "invalid task")
    require(all(a in ("make:none", "make:skill-creator", "make:dao") for a in arms), "invalid creator arm")
    expected = set(itertools.product(tasks, arms, range(reps)))
    seen = set()
    require(type(rows) is list and rows, "empty result matrix")
    for row in rows:
        require(type(row) is dict, "row must be an object")
        require(type(row.get("task")) is str and type(row.get("arm")) is str
                and type(row.get("rep")) is int, "invalid row identity")
        key = row["task"], row["arm"], row["rep"]
        require(key in expected and key not in seen, f"extra or duplicate row: {key}")
        seen.add(key)
        for field in ("pass", "made", "clean_stop", "s2_loaded_runbook"):
            require(row.get(field) is True, f"{key}: {field} must be true")
        require(row.get("tampered") is False, f"{key}: tampered")
        for field in ("reruns", "probes"):
            require(type(row.get(field)) is int and row[field] == 0, f"{key}: {field}")
        skill = row.get("skill_name")
        require(type(skill) is str and re.fullmatch(r"[a-z0-9-]{1,64}", skill), "invalid skill name")
        for phase in ("make", "s1", "s2"):
            run = row.get(phase)
            require(type(run) is dict, f"{key}: missing {phase}")
            require(type(run.get("rc")) is int and run["rc"] == 0, f"{key}: {phase} rc")
            require("error" in run and run["error"] in (None, ""), f"{key}: {phase} error")
            require("is_error" in run and (run["is_error"] is None or run["is_error"] is False), f"{key}: {phase} is_error")
            require(type(run.get("turns")) is int and run["turns"] > 0, f"{key}: {phase} turns")
            require(run.get("subtype") == "success" and type(run.get("errors")) is list
                    and not run["errors"], f"{key}: {phase} failed result diagnostics")
            require(run.get("diagnostics_truncated") is False, f"{key}: {phase} incomplete diagnostics")
            require(type(run.get("skill_calls")) is list, f"{key}: {phase} calls")
            require(type(run.get("tool_trace")) is list and run["tool_trace"]
                    and run.get("trace_truncated") is False, f"{key}: incomplete {phase} tool trace")
            if phase != "make":
                require(skill in json.dumps(run["skill_calls"]), f"{key}: {phase} did not load skill")
            elif row["arm"] != "make:none":
                creator = "dao-skill-state" if row["arm"] == "make:dao" else "skill-creator"
                require(any(call in (creator, "read:" + creator) for call in run["skill_calls"]), f"{key}: maker did not load {creator}")
        for field in ("after_session1", "ran"):
            require(type(row.get(field)) is list and all(type(n) is int and 1 <= n <= 8 for n in row[field]), f"{key}: {field}")
        troot = Path(tasks_dir) / row["task"]
        expect = troot / "expect_s1.json"
        require(row["after_session1"] == (json.loads(expect.read_text()) if expect.exists() else [1, 2, 3, 4]), f"{key}: false clean stop")
        for field, loads in (("events_after_s1", "after_session1"), ("events", "ran")):
            events = row.get(field)
            require(type(events) is list, f"{key}: missing events")
            committed = set()
            for event in events:
                require(type(event) is dict and event.get("kind") in ("attempt", "load", "query")
                        and type(event.get("n")) is int and 1 <= event["n"] <= 8, f"{key}: malformed event")
                require(set(event) == ({"kind", "n", "loaded"} if event["kind"] == "query" else {"kind", "n"}), f"{key}: event fields")
                if event["kind"] == "query":
                    require(type(event["loaded"]) is bool and event["loaded"] is (event["n"] in committed), f"{key}: query contradicts commits")
                if event["kind"] == "load":
                    committed.add(event["n"])
            require([e["n"] for e in events if e["kind"] == "load"] == row[loads], f"{key}: inconsistent load audit")
        first, final = row["events_after_s1"], row["events"]
        require(final[:len(first)] == first, f"{key}: changed audit prefix")
        policy = troot / "resume_gate.json"
        if policy.exists():
            config = json.loads(policy.read_text())
            require(type(config) is dict and set(config) in ({"query"}, {"no_query"}), "invalid resume policy")
            if "no_query" in config:
                require(config["no_query"] is True and not any(e["kind"] == "query" for e in final), f"{key}: forbidden query")
            query = config.get("query")
            if "query" in config:
                require(type(query) is dict and set(query) == {"step", "loaded"}
                        and type(query["step"]) is int and type(query["loaded"]) is bool, "invalid query policy")
                confirmed = False
                for event in final[len(first):]:
                    if not confirmed and event["kind"] == "query" and event["n"] == query["step"]:
                        require(event["loaded"] is query["loaded"], f"{key}: wrong query answer")
                        confirmed = True
                    if event["kind"] in ("attempt", "load"):
                        require(confirmed, f"{key}: action before reconciliation")
                require(confirmed, f"{key}: missing reconciliation query")
    require(seen == expected, f"missing rows: {sorted(expected - seen)}")
    return len(seen)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out_dir", type=Path)
    parser.add_argument("--tasks", required=True)
    parser.add_argument("--arms", required=True)
    parser.add_argument("--reps", type=int, required=True)
    parser.add_argument("--tasks-dir", type=Path, default=Path(__file__).parent / "resume-tasks")
    args = parser.parse_args()
    receipt = args.out_dir / "resume-gate.json"
    try:
        tasks, arms = args.tasks.split(","), args.arms.split(",")
        source = args.out_dir / "resume.jsonl"
        rows = [json.loads(line) for line in source.read_text().splitlines()]
        count = validate_rows(rows, tasks, arms, args.reps, args.tasks_dir)
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip()
        require("GITHUB_SHA" not in os.environ or os.environ["GITHUB_SHA"] == head, "GITHUB_SHA differs from HEAD")
        authoring = REPO / "dao-skill-state/references/authoring.md"
        current = {"sha": head, "authoring_sha256": hashlib.sha256(authoring.read_bytes()).hexdigest(),
                   "fixtures": {task: hashes(args.tasks_dir / task) for task in tasks},
                   "tasks": tasks, "arms": arms, "reps": args.reps}
        producer_bytes = (args.out_dir / "resume-producer.json").read_bytes()
        producer_hash = hashlib.sha256(producer_bytes).hexdigest()
        producer = json.loads(producer_bytes)
        require(type(producer) is dict and set(producer) == set(current) | {"github_run_id", "github_run_attempt", "budgets"}, "invalid producer manifest")
        require(type(producer["reps"]) is int and all(producer[k] == value for k, value in current.items()), "stale or mismatched producer")
        budgets = producer["budgets"]
        require(type(budgets) is dict and set(budgets) == {"make", "s1", "s2"}, "invalid phase budgets")
        for budget in budgets.values():
            require(type(budget) is dict and set(budget) == {"max_turns", "timeout_s"}
                    and all(type(value) is int and value > 0 for value in budget.values()), "invalid phase budget")
        for field in ("github_run_id", "github_run_attempt"):
            require(producer[field] is None or type(producer[field]) is str, "invalid producer run identity")
            if field.upper() in os.environ:
                require(producer[field] == os.environ[field.upper()], f"producer {field} mismatch")
        require(all(row.get("producer_sha256") == producer_hash for row in rows), "row producer hash mismatch")
        import create
        require(not (args.out_dir / "created").is_symlink(), "created artifacts directory is a symlink")
        for row in rows:
            artifact = args.out_dir / "created" / f"{row['task']}-{row['arm'][5:]}-{row['rep']}"
            manifest = hashes(artifact)
            require(manifest and manifest == row.get("skill_hashes"), f"generated skill hash mismatch: {artifact.name}")
            require(create.structure_ok(str(artifact)) is True, f"invalid skill structure: {artifact.name}")
        result = {**current, "pass": True, "github_sha": os.environ.get("GITHUB_SHA"), "rows": count,
                  "producer_sha256": producer_hash, "github_run_id": producer["github_run_id"],
                  "github_run_attempt": producer["github_run_attempt"], "budgets": budgets,
                  "results_sha256": hashlib.sha256(source.read_bytes()).hexdigest()}
        receipt.write_text(json.dumps(result, indent=2) + "\n")
        print(f"resume gate: {count} rows passed at {head}")
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError) as error:
        if args.out_dir.is_dir():
            receipt.write_text(json.dumps({"pass": False, "error": str(error)}) + "\n")
        sys.exit(f"resume gate: {error}")


if __name__ == "__main__":
    main()
