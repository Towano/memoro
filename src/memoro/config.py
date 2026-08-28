# Adapted from Perenna (https://github.com/scarletkc/Perenna), MIT License.
from __future__ import annotations

import json
import os
import re
import unicodedata
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path

from memoro.errors import ConfigurationError
from memoro.filesystem import atomic_replace

DEFAULT_HOME = Path.home() / ".memoro"
LOCAL_CONFIG_NAME = "config.json"
DEFAULT_SPACE = "personal"
RESERVED_SPACE_NAMES = frozenset({"all"})
MAX_SPACE_NAME_LENGTH = 32

_SPACE_NAME_RE = re.compile(r"[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?\Z")
_SPACE_NAME_GUIDANCE = (
    "Use 1-32 lowercase letters, digits, or hyphens, starting and ending with a letter "
    "or digit, and avoid reserved names."
)


@dataclass(frozen=True, slots=True)
class SpaceSettings:
    readonly: bool


@dataclass(frozen=True, slots=True)
class RuntimePaths:
    home: Path

    @property
    def spaces(self) -> Path:
        return self.home / "spaces"

    def space(self, name: str) -> Path:
        return self.spaces / normalize_space_name(name)


def resolve_home(
    cli_home: str | os.PathLike[str] | None,
    environ: Mapping[str, str] | None = None,
) -> Path:
    env = os.environ if environ is None else environ
    if cli_home is not None:
        raw = os.fspath(cli_home)
        origin = "--home"
    elif "MEMORO_HOME" in env:
        raw = env["MEMORO_HOME"]
        origin = "MEMORO_HOME"
    else:
        return DEFAULT_HOME.expanduser().resolve(strict=False)

    if not raw.strip():
        raise ConfigurationError(f"{origin} is empty. Provide a directory for Memoro data.")
    expanded = os.path.expandvars(os.path.expanduser(raw.strip()))
    return Path(expanded).resolve(strict=False)


def normalize_space_name(value: str) -> str:
    if not isinstance(value, str):
        raise ConfigurationError(f"Space name must be a string. {_SPACE_NAME_GUIDANCE}")
    normalized = unicodedata.normalize("NFKC", value).strip().lower()
    if not normalized:
        raise ConfigurationError(f"Space name is empty. {_SPACE_NAME_GUIDANCE}")
    if _SPACE_NAME_RE.fullmatch(normalized) is None:
        raise ConfigurationError(f"Space name {value!r} is invalid. {_SPACE_NAME_GUIDANCE}")
    if normalized in RESERVED_SPACE_NAMES:
        raise ConfigurationError(
            f"Space name {normalized!r} is reserved. Choose a different name."
        )
    return normalized


def load_spaces(home: Path) -> dict[str, SpaceSettings]:
    """Return the space registry, with the default space always present."""

    path = home / LOCAL_CONFIG_NAME
    if not path.exists():
        return {DEFAULT_SPACE: SpaceSettings(readonly=False)}
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ConfigurationError(
            f"Memoro local configuration {path} is invalid or unreadable. Repair or remove "
            "that file, then retry."
        ) from exc
    if not isinstance(payload, dict) or set(payload) != {"spaces"}:
        raise ConfigurationError(
            f"Memoro local configuration {path} must contain exactly the spaces field. "
            "Repair or remove that file, then retry."
        )
    raw_spaces = payload["spaces"]
    if not isinstance(raw_spaces, dict):
        raise ConfigurationError(
            f"Memoro local configuration {path} has an invalid spaces value. It must map "
            "space names to their settings. Repair or remove that file, then retry."
        )
    spaces: dict[str, SpaceSettings] = {}
    for name, entry in raw_spaces.items():
        if not isinstance(name, str) or normalize_space_name(name) != name:
            raise ConfigurationError(
                f"Memoro local configuration {path} has a space name {name!r} that is not "
                f"normalized. {_SPACE_NAME_GUIDANCE}"
            )
        if (
            not isinstance(entry, dict)
            or set(entry) != {"readonly"}
            or not isinstance(entry["readonly"], bool)
        ):
            raise ConfigurationError(
                f"Memoro local configuration {path} has invalid settings for space {name!r}. "
                "Each space must contain exactly a boolean readonly field."
            )
        spaces[name] = SpaceSettings(readonly=entry["readonly"])
    if spaces.get(DEFAULT_SPACE, SpaceSettings(readonly=False)).readonly:
        raise ConfigurationError(
            f"Memoro local configuration {path} marks the default space "
            f"{DEFAULT_SPACE!r} as readonly. The default space is always writable; "
            "remove that entry or set readonly to false, then retry."
        )
    spaces.setdefault(DEFAULT_SPACE, SpaceSettings(readonly=False))
    return spaces


def save_spaces(home: Path, spaces: Mapping[str, SpaceSettings]) -> Path:
    normalized: dict[str, SpaceSettings] = {}
    for name, settings in spaces.items():
        canonical = normalize_space_name(name)
        if canonical != name:
            raise ConfigurationError(
                f"Space name {name!r} is not normalized. Use {canonical!r} instead."
            )
        if not isinstance(settings, SpaceSettings):
            raise ConfigurationError(
                f"Space {name!r} has invalid settings. Provide SpaceSettings values."
            )
        normalized[name] = settings
    if normalized.get(DEFAULT_SPACE, SpaceSettings(readonly=False)).readonly:
        raise ConfigurationError(
            f"The default space {DEFAULT_SPACE!r} is always writable. Set readonly to "
            "false before saving the space registry."
        )
    path = home / LOCAL_CONFIG_NAME
    payload = json.dumps(
        {
            "spaces": {
                name: {"readonly": settings.readonly}
                for name, settings in normalized.items()
            }
        },
        ensure_ascii=False,
        indent=2,
        sort_keys=True,
    )
    try:
        atomic_replace(path, f"{payload}\n".encode())
    except OSError as exc:
        raise ConfigurationError(
            f"Memoro could not save the space registry to {path}. Check the directory "
            "permissions, then retry."
        ) from exc
    return path
