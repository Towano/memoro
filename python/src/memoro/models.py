# Adapted from Perenna (https://github.com/scarletkc/Perenna), MIT License.
from __future__ import annotations

import re
import secrets
import unicodedata
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import PurePosixPath

MAX_TITLE_LENGTH = 120
MAX_SUMMARY_LENGTH = 300
MAX_BODY_LENGTH = 20_000
MAX_PROJECT_LENGTH = 64
MAX_TAG_LENGTH = 32
MAX_TAGS = 8

MEMORY_KINDS = ("persona", "project", "playbook")

_PROJECT_RE = re.compile(r"[a-z0-9._-]+\Z")
_ULID_RE = re.compile(r"[0-7][0-9A-HJKMNP-TV-Z]{25}\Z")
_REVISION_RE = re.compile(r"[0-9a-f]{64}\Z")
_TIMESTAMP_RE = re.compile(
    r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?(?:Z|[+-]\d{2}:\d{2})\Z"
)
_WHITESPACE_RE = re.compile(r"\s+")
_ULID_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
_WINDOWS_RESERVED_NAMES = {
    "aux",
    "con",
    "nul",
    "prn",
    *(f"com{number}" for number in range(1, 10)),
    *(f"lpt{number}" for number in range(1, 10)),
}


@dataclass(frozen=True, slots=True)
class Memory:
    id: str
    title: str
    summary: str
    tags: tuple[str, ...]
    created_at: str
    updated_at: str
    body: str
    kind: str
    project: str | None
    relative_path: str


@dataclass(frozen=True, slots=True)
class MemorySnapshot:
    commit: str | None
    memories: tuple[Memory, ...]

    def by_id(self) -> dict[str, Memory]:
        return {memory.id: memory for memory in self.memories}


@dataclass(frozen=True, slots=True)
class PatchEdit:
    old_text: str
    new_text: str


@dataclass(frozen=True, slots=True)
class MutationReceipt:
    memory: Memory
    operation: str
    changed: bool
    previous_memory: Memory | None
    previous_commit: str | None
    commit: str


def normalize_title(value: str) -> str:
    if not isinstance(value, str):
        raise ValueError("title must be a string")
    normalized = unicodedata.normalize("NFKC", value)
    normalized = _WHITESPACE_RE.sub(" ", normalized).strip()
    if not normalized:
        raise ValueError("title is empty")
    if any(unicodedata.category(character) in {"Cc", "Cs"} for character in normalized):
        raise ValueError("title contains a control character")
    if len(normalized) > MAX_TITLE_LENGTH:
        raise ValueError("title is too long")
    return normalized


def title_key(value: str) -> str:
    return normalize_title(value).casefold()


def normalize_summary(value: str) -> str:
    if not isinstance(value, str):
        raise ValueError("summary must be a string")
    normalized = unicodedata.normalize("NFKC", value)
    normalized = _WHITESPACE_RE.sub(" ", normalized).strip()
    if not normalized:
        raise ValueError("summary is empty")
    if any(unicodedata.category(character) in {"Cc", "Cs"} for character in normalized):
        raise ValueError("summary contains a control character")
    if len(normalized) > MAX_SUMMARY_LENGTH:
        raise ValueError("summary is too long")
    return normalized


def normalize_body(value: str) -> str:
    if not isinstance(value, str):
        raise ValueError("body must be a string")
    normalized = value.replace("\r\n", "\n").replace("\r", "\n").strip("\n")
    if not normalized.strip():
        raise ValueError("body is empty")
    if any(
        unicodedata.category(character) == "Cs"
        or (unicodedata.category(character) == "Cc" and character not in {"\n", "\t"})
        for character in normalized
    ):
        raise ValueError("body contains a control character")
    if len(normalized) > MAX_BODY_LENGTH:
        raise ValueError("body is too long")
    return normalized


def normalize_project(value: str) -> str:
    if not isinstance(value, str):
        raise ValueError("project must be a string")
    normalized = unicodedata.normalize("NFKC", value).strip().lower()
    if not normalized:
        raise ValueError("project is empty")
    if len(normalized) > MAX_PROJECT_LENGTH:
        raise ValueError("project is too long")
    if normalized == "." or ".." in normalized or "/" in normalized or "\\" in normalized:
        raise ValueError("project contains path traversal")
    if _PROJECT_RE.fullmatch(normalized) is None:
        raise ValueError("project contains unsupported characters")
    if normalized.endswith(".") or normalized.split(".", 1)[0] in _WINDOWS_RESERVED_NAMES:
        raise ValueError("project is not a portable directory name")
    return normalized


def normalize_kind(value: str) -> str:
    if not isinstance(value, str):
        raise ValueError("kind must be a string")
    normalized = value.strip().lower()
    if normalized not in MEMORY_KINDS:
        raise ValueError("kind must be persona, project, or playbook")
    return normalized


def validate_location(kind: str, project: str | None) -> tuple[str, str | None]:
    """Validate the kind/project pairing and return its normalized form."""

    normalized_kind = normalize_kind(kind)
    if normalized_kind == "project":
        if project is None:
            raise ValueError("project kind requires a project slug")
        return normalized_kind, normalize_project(project)
    if project is not None:
        raise ValueError(f"{normalized_kind} kind does not take a project slug")
    return normalized_kind, None


def location_label(kind: str, project: str | None) -> str:
    normalized_kind, normalized_project = validate_location(kind, project)
    if normalized_kind == "project":
        return f"project/{normalized_project}"
    return normalized_kind


def normalize_tags(value: Sequence[str]) -> tuple[str, ...]:
    if isinstance(value, (str, bytes)) or not isinstance(value, Sequence):
        raise ValueError("tags must be a list of strings")
    normalized: list[str] = []
    seen: set[str] = set()
    for item in value:
        if not isinstance(item, str):
            raise ValueError("tags must be a list of strings")
        tag = unicodedata.normalize("NFKC", item).strip()
        if not tag:
            raise ValueError("tags contain an empty value")
        if len(tag) > MAX_TAG_LENGTH:
            raise ValueError("tags contain a value that is too long")
        if any(unicodedata.category(character) in {"Cc", "Cs"} for character in tag):
            raise ValueError("tags contain a control character")
        key = tag.casefold()
        if key in seen:
            continue
        seen.add(key)
        normalized.append(tag)
    if len(normalized) > MAX_TAGS:
        raise ValueError(f"tags exceed the maximum of {MAX_TAGS}")
    return tuple(sorted(normalized, key=str.casefold))


def memory_path(memory_id: str, kind: str, project: str | None) -> str:
    validate_ulid(memory_id)
    normalized_kind, normalized_project = validate_location(kind, project)
    if normalized_kind == "persona":
        return str(PurePosixPath("persona", f"{memory_id}.md"))
    if normalized_kind == "playbook":
        return str(PurePosixPath("playbooks", f"{memory_id}.md"))
    return str(PurePosixPath("projects", normalized_project, f"{memory_id}.md"))


def validate_ulid(value: str) -> str:
    if not isinstance(value, str) or _ULID_RE.fullmatch(value) is None:
        raise ValueError("id is not a valid ULID")
    return value


def validate_revision(value: str) -> str:
    if not isinstance(value, str) or _REVISION_RE.fullmatch(value) is None:
        raise ValueError("revision is not a lowercase SHA-256 digest")
    return value


def new_ulid(now: datetime | None = None) -> str:
    instant = datetime.now(UTC) if now is None else now.astimezone(UTC)
    timestamp_ms = int(instant.timestamp() * 1000)
    if not 0 <= timestamp_ms < 2**48:
        raise ValueError("timestamp cannot be encoded as a ULID")
    value = (timestamp_ms << 80) | int.from_bytes(secrets.token_bytes(10), "big")
    chars = ["0"] * 26
    for index in range(25, -1, -1):
        chars[index] = _ULID_ALPHABET[value & 31]
        value >>= 5
    return "".join(chars)


def utc_now() -> datetime:
    return datetime.now(UTC)


def format_timestamp(value: datetime) -> str:
    return value.astimezone(UTC).isoformat(timespec="microseconds").replace("+00:00", "Z")


def parse_timestamp(value: str) -> datetime:
    if not isinstance(value, str):
        raise ValueError("timestamp must be a string")
    if _TIMESTAMP_RE.fullmatch(value) is None:
        raise ValueError("timestamp must use RFC 3339 date-time syntax")
    candidate = value[:-1] + "+00:00" if value.endswith("Z") else value
    parsed = datetime.fromisoformat(candidate)
    if parsed.tzinfo is None:
        raise ValueError("timestamp must include a timezone")
    return parsed.astimezone(UTC)


def next_update_time(now: datetime, previous: str) -> datetime:
    previous_time = parse_timestamp(previous)
    normalized_now = now.astimezone(UTC)
    if normalized_now <= previous_time:
        return previous_time + timedelta(microseconds=1)
    return normalized_now
