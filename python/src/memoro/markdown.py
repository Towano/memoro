# Adapted from Perenna (https://github.com/scarletkc/Perenna), MIT License.
from __future__ import annotations

import json
from hashlib import sha256
from pathlib import PurePosixPath
from typing import Any

import yaml

from memoro.errors import MemoryValidationError
from memoro.models import (
    Memory,
    normalize_body,
    normalize_project,
    normalize_summary,
    normalize_tags,
    normalize_title,
    parse_timestamp,
    validate_ulid,
)

FRONTMATTER_FIELDS = ("id", "title", "summary", "tags", "created_at", "updated_at")
_REQUIRED_FIELDS = tuple(field for field in FRONTMATTER_FIELDS if field != "tags")


class _UniqueSafeLoader(yaml.SafeLoader):
    pass


def _construct_unique_mapping(
    loader: _UniqueSafeLoader,
    node: yaml.nodes.MappingNode,
    deep: bool = False,
) -> dict[Any, Any]:
    loader.flatten_mapping(node)
    result: dict[Any, Any] = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if not isinstance(key, str):
            raise yaml.constructor.ConstructorError(
                "while constructing frontmatter",
                node.start_mark,
                "frontmatter field names must be strings",
                key_node.start_mark,
            )
        if key in result:
            raise yaml.constructor.ConstructorError(
                "while constructing frontmatter",
                node.start_mark,
                f"duplicate field {key!r}",
                key_node.start_mark,
            )
        result[key] = loader.construct_object(value_node, deep=deep)
    return result


_UniqueSafeLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG,
    _construct_unique_mapping,
)


def serialize_memory(memory: Memory) -> str:
    lines = ["---"]
    for field in FRONTMATTER_FIELDS:
        if field == "tags" and not memory.tags:
            continue
        value = getattr(memory, field)
        if field == "tags":
            value = list(value)
        lines.append(f"{field}: {json.dumps(value, ensure_ascii=False)}")
    lines.extend(("---", "", normalize_body(memory.body)))
    return "\n".join(lines) + "\n"


def memory_revision(memory: Memory) -> str:
    """Return the opaque revision of one canonical committed memory."""

    return sha256(serialize_memory(memory).encode("utf-8")).hexdigest()


def parse_memory(text: str, relative_path: str) -> Memory:
    normalized_text = text.replace("\r\n", "\n").replace("\r", "\n")
    if not normalized_text.startswith("---\n"):
        raise _invalid(relative_path, "frontmatter must start on the first line")
    frontmatter_text, separator, remainder = normalized_text[4:].partition("\n---\n")
    if not separator:
        raise _invalid(relative_path, "frontmatter closing delimiter is missing")
    if not remainder.startswith("\n"):
        raise _invalid(relative_path, "frontmatter must be followed by one blank line")

    try:
        raw = yaml.load(frontmatter_text, Loader=_UniqueSafeLoader)
    except yaml.YAMLError as exc:
        raise _invalid(relative_path, "frontmatter is not valid YAML") from exc
    if not isinstance(raw, dict):
        raise _invalid(relative_path, "frontmatter must be a mapping")
    missing = sorted(set(_REQUIRED_FIELDS) - set(raw))
    extra = sorted(str(key) for key in set(raw) - set(FRONTMATTER_FIELDS))
    if missing or extra:
        details = []
        if missing:
            details.append(f"missing fields: {', '.join(missing)}")
        if extra:
            details.append(f"unsupported fields: {', '.join(map(str, extra))}")
        raise _invalid(relative_path, "; ".join(details))

    body = remainder[1:]
    if body.endswith("\n"):
        body = body[:-1]
    try:
        memory_id = validate_ulid(raw["id"])
        title = normalize_title(raw["title"])
        summary = normalize_summary(raw["summary"])
        tags = _parse_tags(raw, relative_path)
        created_at = _timestamp_string(raw["created_at"])
        updated_at = _timestamp_string(raw["updated_at"])
        normalized_body = normalize_body(body)
    except (TypeError, ValueError) as exc:
        raise _invalid(relative_path, str(exc)) from exc

    if title != raw["title"]:
        raise _invalid(relative_path, "title is not normalized")
    if summary != raw["summary"]:
        raise _invalid(relative_path, "summary is not normalized")
    if parse_timestamp(updated_at) < parse_timestamp(created_at):
        raise _invalid(relative_path, "updated_at is earlier than created_at")

    kind, project, path_id = _location_and_id(relative_path)
    if path_id != memory_id:
        raise _invalid(relative_path, "frontmatter id does not match the filename")
    return Memory(
        id=memory_id,
        title=title,
        summary=summary,
        tags=tags,
        created_at=created_at,
        updated_at=updated_at,
        body=normalized_body,
        kind=kind,
        project=project,
        relative_path=relative_path,
    )


def _parse_tags(raw: dict[Any, Any], relative_path: str) -> tuple[str, ...]:
    if "tags" not in raw:
        return ()
    raw_tags = raw["tags"]
    if not isinstance(raw_tags, list) or any(not isinstance(tag, str) for tag in raw_tags):
        raise _invalid(relative_path, "tags must be a list of strings")
    if not raw_tags:
        raise _invalid(relative_path, "empty tags must omit the tags field")
    tags = normalize_tags(raw_tags)
    if list(tags) != raw_tags:
        raise _invalid(relative_path, "tags are not canonical")
    return tags


def _location_and_id(relative_path: str) -> tuple[str, str | None, str]:
    path = PurePosixPath(relative_path)
    parts = path.parts
    if len(parts) == 2 and parts[0] == "persona" and path.suffix == ".md":
        kind = "persona"
        project = None
    elif len(parts) == 2 and parts[0] == "playbooks" and path.suffix == ".md":
        kind = "playbook"
        project = None
    elif len(parts) == 3 and parts[0] == "projects" and path.suffix == ".md":
        try:
            project = normalize_project(parts[1])
        except ValueError as exc:
            raise _invalid(relative_path, "project directory is invalid") from exc
        if project != parts[1]:
            raise _invalid(relative_path, "project directory is not normalized")
        kind = "project"
    else:
        raise _invalid(
            relative_path,
            "path must be persona/<ULID>.md, projects/<slug>/<ULID>.md, or playbooks/<ULID>.md",
        )
    memory_id = path.stem
    try:
        validate_ulid(memory_id)
    except ValueError as exc:
        raise _invalid(relative_path, "filename is not a ULID") from exc
    return kind, project, memory_id


def _timestamp_string(value: object) -> str:
    if not isinstance(value, str):
        raise ValueError("timestamps must be quoted strings")
    parse_timestamp(value)
    return value


def _invalid(relative_path: str, reason: str) -> MemoryValidationError:
    return MemoryValidationError(
        f"Committed memory {relative_path!r} is invalid: {reason}. "
        "Repair the Markdown in the memory repository and commit the correction."
    )
