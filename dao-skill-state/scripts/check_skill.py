#!/usr/bin/env python3
"""Read-only Agent Skill frontmatter checker; not a security or behavior scan."""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

try:
    import yaml
except ImportError:
    print(json.dumps({"status": "uncheckable", "errors": [{"kind": "dependency", "message": "PyYAML is required; no dependency was installed"}], "warnings": []}))
    raise SystemExit(2)


class DuplicateKeyError(ValueError):
    pass


class StrictLoader(yaml.SafeLoader):
    pass


def _mapping(loader: StrictLoader, node: yaml.MappingNode, deep: bool = False):
    mapping = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if not isinstance(key, str):
            raise ValueError("YAML mapping keys must be strings")
        if key in mapping:
            raise DuplicateKeyError(f"duplicate YAML key: {key}")
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


StrictLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _mapping)
_PORTABLE_NAME = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
_ALLOWED_TOP = {"name", "description", "license", "compatibility", "metadata", "allowed-tools"}


def _issue(kind: str, message: str) -> dict[str, str]:
    return {"kind": kind, "message": message}


def _parse_frontmatter(text: str) -> tuple[dict[str, Any] | None, str | None]:
    lines = text.splitlines()
    if not lines or lines[0] != "---":
        return None, "SKILL.md must start with YAML frontmatter (---)"
    try:
        end = next(i for i, line in enumerate(lines[1:], 1) if line == "---")
    except StopIteration:
        return None, "YAML frontmatter has no closing ---"
    source = "\n".join(lines[1:end]) + "\n"
    try:
        data = yaml.load(source, Loader=StrictLoader)
    except (yaml.YAMLError, ValueError, RecursionError):
        return None, "invalid YAML frontmatter (syntax, key, duplicate, recursion or unsupported tag)"
    if not isinstance(data, dict):
        return None, "frontmatter must be a mapping"
    return data, None


def check_skill(skill_dir: str | Path) -> dict[str, Any]:
    root = Path(skill_dir)
    result: dict[str, Any] = {"path": str(root), "errors": [], "warnings": []}
    skill_file = root / "SKILL.md"
    try:
        root = root.resolve(strict=True)
        skill_file = root / "SKILL.md"
        # ponytail: bounded lint, not an arbitrary-size document parser.
        if not skill_file.is_file() or skill_file.stat().st_size > 1024 * 1024:
            result["status"] = "uncheckable"
            result["errors"].append(_issue("resource", "SKILL.md must be a regular file no larger than this checker's 1 MiB limit"))
            return result
        text = skill_file.read_text(encoding="utf-8")
    except (FileNotFoundError, NotADirectoryError):
        result["status"] = "uncheckable"
        result["errors"].append(_issue("uncheckable", f"missing {skill_file}"))
        return result
    except (OSError, UnicodeError, RuntimeError):
        result["status"] = "uncheckable"
        result["errors"].append(_issue("uncheckable", "cannot resolve/read SKILL.md as UTF-8"))
        return result

    frontmatter, parse_error = _parse_frontmatter(text)
    if parse_error:
        result["status"] = "invalid"
        result["errors"].append(_issue("frontmatter", parse_error))
        return result
    assert frontmatter is not None
    unknown = sorted(set(frontmatter) - _ALLOWED_TOP)
    if unknown:
        result["warnings"].append(_issue("unknown-field", f"unrecognized optional fields: {', '.join(unknown)}"))

    name = frontmatter.get("name")
    if not isinstance(name, str) or not 1 <= len(name) <= 64:
        result["errors"].append(_issue("name", "name must be a string of 1..64 characters"))
    elif name != root.name:
        result["errors"].append(_issue("name", "name must match the skill directory name"))
    elif name != name.lower():
        result["errors"].append(_issue("name", "name must not contain uppercase characters"))
    elif not all(part and all(ch.isalnum() for ch in part) for part in name.split("-")):
        result["errors"].append(_issue("name", "name segments must be non-empty alphanumeric strings separated by hyphens"))
    if isinstance(name, str) and not _PORTABLE_NAME.fullmatch(name):
        result["warnings"].append(_issue("portability", "name is not ASCII lowercase portable form; Unicode names are accepted"))

    description = frontmatter.get("description")
    if not isinstance(description, str) or not description.strip() or not 1 <= len(description) <= 1024:
        result["errors"].append(_issue("description", "description must be a string of 1..1024 characters"))

    for field, limit in (("license", None), ("compatibility", 500), ("allowed-tools", None)):
        if field not in frontmatter:
            continue
        value = frontmatter[field]
        if not isinstance(value, str) or not value.strip() or (limit is not None and len(value) > limit):
            suffix = f" string of 1..{limit} characters" if limit else " non-empty string"
            result["errors"].append(_issue(field, f"{field} must be a{suffix}"))

    if "metadata" in frontmatter:
        metadata = frontmatter["metadata"]
        if not isinstance(metadata, dict):
            result["errors"].append(_issue("metadata", "metadata must be a string:string mapping"))
        elif any(not isinstance(key, str) or not isinstance(value, str) for key, value in metadata.items()):
            result["errors"].append(_issue("metadata", "metadata keys and values must all be strings"))

    result["status"] = "valid" if not result["errors"] else "invalid"
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Check Agent Skill frontmatter without executing or mutating the skill")
    parser.add_argument("skill_dir", nargs="?", default=".")
    args = parser.parse_args(argv)
    result = check_skill(args.skill_dir)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if result["status"] == "uncheckable":
        return 2
    return 0 if result["status"] == "valid" else 1


if __name__ == "__main__":
    sys.exit(main())
