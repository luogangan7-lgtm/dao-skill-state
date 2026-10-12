#!/usr/bin/env python3
"""Validate and count dao-assessment/v1 JSON (10 MiB, 8 arms, 100,000 planned rows max).

This is bookkeeping only: evidence strings are never opened, executed, or verified.
Unknown fields are rejected, and the output contains counters rather than a score.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import re
import stat
import sys
from pathlib import Path
from typing import Any

MAX_BYTES = 10 * 1024 * 1024
MAX_ARMS = 8  # Resource bound, not a statistical recommendation.
MAX_PLANNED_ROWS = 100_000
KINDS = ("positive", "boundary", "nontrigger")
STATUSES = ("PASS", "FAIL", "UNKNOWN", "NOT_RUN", "INVALID")
TRUST = ("PASS", "CONDITIONAL", "FAIL", "UNKNOWN", "N/A")
COST_FIELDS = ("seconds", "input_tokens", "output_tokens", "cached_input_tokens")
ROOT_FIELDS = {"schema", "identity", "arms", "repetitions", "cases", "isolation", "runs"}
IDENTITY_FIELDS = {"skill", "skill_sha256", "task_set_sha256", "model", "host", "date", "rubric"}
HEX64 = re.compile(r"[0-9a-fA-F]{64}\Z")
DATE = re.compile(r"\d{4}-\d{2}-\d{2}\Z")


class DuplicateKeyError(ValueError):
    pass


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateKeyError(f"duplicate object key: {key!r}")
        result[key] = value
    return result


def _error(errors: list[dict[str, str]], path: str, message: str) -> None:
    errors.append({"path": path, "message": message})


def _object(value: Any, path: str, fields: set[str], errors: list[dict[str, str]]) -> bool:
    if not isinstance(value, dict):
        _error(errors, path, "must be an object")
        return False
    missing, unknown = fields - value.keys(), value.keys() - fields
    if missing:
        _error(errors, path, f"missing fields: {', '.join(sorted(missing))}")
    if unknown:
        _error(errors, path, f"unknown fields: {', '.join(sorted(unknown))}")
    return not missing and not unknown


def _text(value: Any, path: str, errors: list[dict[str, str]]) -> bool:
    if not isinstance(value, str) or not value.strip():
        _error(errors, path, "must be a nonempty string")
        return False
    return True


def validate(data: Any) -> list[dict[str, str]]:
    errors: list[dict[str, str]] = []
    if not isinstance(data, dict):
        _error(errors, "$", "must be an object")
        return errors
    _object(data, "$", ROOT_FIELDS, errors)
    if ROOT_FIELDS - data.keys():
        return errors
    if data["schema"] != "dao-assessment/v1":
        _error(errors, "$.schema", "must equal dao-assessment/v1")

    identity = data["identity"]
    if _object(identity, "$.identity", IDENTITY_FIELDS, errors):
        for field in ("skill", "model", "host", "rubric"):
            _text(identity[field], f"$.identity.{field}", errors)
        for field in ("skill_sha256", "task_set_sha256"):
            if not isinstance(identity[field], str) or not HEX64.fullmatch(identity[field]):
                _error(errors, f"$.identity.{field}", "must be a 64-character SHA-256 hex string")
        date = identity["date"]
        try:
            if not isinstance(date, str) or not DATE.fullmatch(date):
                raise ValueError
            dt.date.fromisoformat(date)
        except ValueError:
            _error(errors, "$.identity.date", "must be a valid ISO date YYYY-MM-DD")

    arms = data["arms"]
    arm_set: set[str] = set()
    valid_arms = isinstance(arms, list)
    if not valid_arms:
        _error(errors, "$.arms", "must be an array")
        arms = []
    else:
        for i, arm in enumerate(arms):
            _text(arm, f"$.arms[{i}]", errors)
        if not arms:
            _error(errors, "$.arms", "must contain at least one arm")
        elif len(arms) > MAX_ARMS:
            _error(errors, "$.arms", f"must contain at most {MAX_ARMS} arms")
        arm_set = {a for a in arms if isinstance(a, str)}
        if len(arms) != len(arm_set):
            _error(errors, "$.arms", "contains duplicate arms")

    repetitions = data["repetitions"]
    if type(repetitions) is not int or repetitions <= 0:
        _error(errors, "$.repetitions", "must be a positive integer (boolean is not allowed)")

    cases, case_ids = data["cases"], set()
    if not isinstance(cases, list):
        _error(errors, "$.cases", "must be an array")
        cases = []
    elif not cases:
        _error(errors, "$.cases", "must contain at least one case")
    if isinstance(arms, list) and isinstance(cases, list) and type(repetitions) is int and repetitions > 0:
        planned = len(arms) * len(cases) * repetitions
        if planned > MAX_PLANNED_ROWS:
            _error(errors, "$", f"expanded plan has {planned} rows; maximum is {MAX_PLANNED_ROWS}")
    for i, case in enumerate(cases):
        path = f"$.cases[{i}]"
        if not _object(case, path, {"id", "kind"}, errors):
            continue
        if _text(case["id"], path + ".id", errors):
            if case["id"] in case_ids:
                _error(errors, path + ".id", "duplicate case id")
            case_ids.add(case["id"])
        if case["kind"] not in KINDS:
            _error(errors, path + ".kind", f"must be one of {', '.join(KINDS)}")

    isolation = data["isolation"]
    if _object(isolation, "$.isolation", {"status", "evidence"}, errors):
        if isolation["status"] not in ("PASS", "FAIL", "UNKNOWN"):
            _error(errors, "$.isolation.status", "must be PASS, FAIL, or UNKNOWN")
        _text(isolation["evidence"], "$.isolation.evidence", errors)

    runs, run_keys = data["runs"], set()
    if not isinstance(runs, list):
        _error(errors, "$.runs", "must be an array")
        runs = []
    for i, run in enumerate(runs):
        path = f"$.runs[{i}]"
        fields = {"case_id", "arm", "repetition", "status", "trust", "evidence", "cost"}
        if not _object(run, path, fields, errors):
            continue
        case_ok = _text(run["case_id"], path + ".case_id", errors)
        arm_ok = _text(run["arm"], path + ".arm", errors)
        if case_ok and run["case_id"] not in case_ids:
            _error(errors, path + ".case_id", "unknown case id")
        if arm_ok and run["arm"] not in arm_set:
            _error(errors, path + ".arm", "unknown arm")
        rep = run["repetition"]
        rep_ok = type(rep) is int and type(repetitions) is int and 1 <= rep <= repetitions
        if not rep_ok:
            _error(errors, path + ".repetition", "must be an integer from 1 through repetitions")
        if case_ok and arm_ok and rep_ok:
            key = (run["case_id"], run["arm"], rep)
            if key in run_keys:
                _error(errors, path, "duplicate case_id/arm/repetition run")
            run_keys.add(key)
        if run["status"] not in STATUSES:
            _error(errors, path + ".status", f"must be one of {', '.join(STATUSES)}")
        if run["trust"] not in TRUST:
            _error(errors, path + ".trust", f"must be one of {', '.join(TRUST)}")
        _text(run["evidence"], path + ".evidence", errors)
        cost = run["cost"]
        if cost is not None and _object(cost, path + ".cost", set(COST_FIELDS), errors):
            for field in COST_FIELDS:
                value = cost[field]
                if value is None:
                    continue
                integer = field != "seconds"
                valid = type(value) is int if integer else type(value) in (int, float)
                finite = valid and (integer or (type(value) is int and value <= sys.float_info.max)
                                    or (type(value) is float and math.isfinite(value)))
                if not valid or value < 0 or not finite:
                    kind = "nonnegative integer or null" if integer else "finite nonnegative number or null"
                    _error(errors, path + f".cost.{field}", f"must be a {kind} (boolean is not allowed)")
    return errors


def _counter(rows: list[dict[str, Any]]) -> dict[str, Any]:
    planned = len(rows)
    counts = {status: sum(row["status"] == status for row in rows) for status in STATUSES}
    assessed = counts["PASS"] + counts["FAIL"]
    costs = {}
    for field in COST_FIELDS:
        values = [row["cost"][field] for row in rows if row["cost"] is not None and row["cost"][field] is not None]
        total, reason = None, "no planned rows" if not planned else "missing planned values"
        if planned and len(values) == planned:
            reason = None
            try:
                total = sum(values) if field != "seconds" else math.fsum(values)
                if field == "seconds" and not math.isfinite(total):
                    total, reason = None, "sum is non-finite"
            except OverflowError:
                reason = "sum is non-finite"
        costs[field] = {"sum": total, "known_values": len(values), "unavailable_reason": reason}
    return {
        "planned": planned,
        "counts": counts,
        "coverage": assessed / planned if planned else None,
        "conditional_pass": counts["PASS"] / assessed if assessed else None,
        "cost": costs,
    }


def summarize(data: dict[str, Any]) -> dict[str, Any]:
    by_key = {(r["case_id"], r["arm"], r["repetition"]): r for r in data["runs"]}
    case_kind = {case["id"]: case["kind"] for case in data["cases"]}
    rows: list[dict[str, Any]] = []
    rows_by_arm: dict[str, list[dict[str, Any]]] = {arm: [] for arm in data["arms"]}
    for case in data["cases"]:
        for arm in data["arms"]:
            for rep in range(1, data["repetitions"] + 1):
                key = (case["id"], arm, rep)
                row = by_key.get(key) or {"case_id": case["id"], "arm": arm, "repetition": rep,
                                          "status": "NOT_RUN", "trust": "UNKNOWN", "cost": None}
                by_key[key] = row
                rows.append(row)
                rows_by_arm[arm].append(row)
    arms = {}
    for arm in data["arms"]:
        arm_rows = rows_by_arm[arm]
        arms[arm] = {
            "overall": _counter(arm_rows),
            "by_kind": {kind: _counter([r for r in arm_rows if case_kind[r["case_id"]] == kind]) for kind in KINDS},
        }
    data_complete = bool(rows) and all(row["status"] in ("PASS", "FAIL") for row in rows)
    trust_clear = bool(rows) and all(row["trust"] in ("PASS", "N/A") for row in rows)
    result: dict[str, Any] = {
        "schema": "dao-assessment-summary/v1",
        "identity": dict(data["identity"]),
        "data_complete": data_complete,
        "trust_clear": trust_clear,
        "gate": "COMPLETE" if data_complete and trust_clear else "INCOMPLETE_OR_BLOCKED",
        "all_tasks_passed": bool(rows) and all(row["status"] == "PASS" for row in rows),
        "arms": arms,
    }
    if len(data["arms"]) == 2:
        first, second = data["arms"]
        pairs = []
        for case in data["cases"]:
            for rep in range(1, data["repetitions"] + 1):
                pair = [by_key[(case["id"], arm, rep)] for arm in (first, second)]
                if all(r["status"] in ("PASS", "FAIL") for r in pair):
                    pairs.append(pair)
        planned = len(data["cases"]) * data["repetitions"]
        wins = sum(a["status"] == "FAIL" and b["status"] == "PASS" for a, b in pairs)
        losses = sum(a["status"] == "PASS" and b["status"] == "FAIL" for a, b in pairs)
        ties = len(pairs) - wins - losses
        full = len(pairs) == planned
        isolation_pass = data["isolation"]["status"] == "PASS"
        reason = None
        if not planned:
            reason = "requires at least one planned pair"
        elif not full:
            reason = "requires full paired PASS/FAIL coverage"
        elif not isolation_pass:
            reason = "requires isolation PASS"
        delta = ((sum(b["status"] == "PASS" for _, b in pairs) - sum(a["status"] == "PASS" for a, _ in pairs)) / planned * 100) if full and isolation_pass and planned else None
        result["pairwise"] = {
            "reference_arm": first, "comparison_arm": second, "distinct_cases": len(data["cases"]),
            "planned_pairs": planned, "comparable_pairs": len(pairs), "wins": wins, "ties": ties, "losses": losses,
            "delta_pp": delta, "delta_unavailable_reason": reason,
        }
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Validate and count a dao-assessment/v1 JSON file without verifying evidence")
    parser.add_argument("input", help="assessment JSON file (maximum 10 MiB)")
    args = parser.parse_args(argv)
    try:
        path = Path(args.input)
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NONBLOCK", 0))
        try:
            info = os.fstat(descriptor)
            if not stat.S_ISREG(info.st_mode):
                raise OSError("input must be a regular file")
            with os.fdopen(descriptor, "rb") as stream:
                descriptor = -1
                raw = stream.read(MAX_BYTES + 1)
        finally:
            if descriptor >= 0:
                os.close(descriptor)
        if len(raw) > MAX_BYTES:
            raise OSError(f"input exceeds {MAX_BYTES} bytes")
        data = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_object,
                          parse_constant=lambda value: (_ for _ in ()).throw(ValueError(f"non-finite JSON number {value}")))
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError, RecursionError) as exc:
        print(json.dumps({"status": "input_error", "errors": [{"path": "$", "message": str(exc)}]}))
        return 2
    errors = validate(data)
    if errors:
        print(json.dumps({"status": "invalid", "errors": errors}, ensure_ascii=True, indent=2))
        return 1
    print(json.dumps(summarize(data), ensure_ascii=True, indent=2, allow_nan=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
