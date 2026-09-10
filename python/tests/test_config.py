# Adapted from Perenna (https://github.com/scarletkc/Perenna), MIT License.
from __future__ import annotations

import json
from pathlib import Path

import pytest

from memoro.config import (
    DEFAULT_HOME,
    DEFAULT_SPACE,
    LOCAL_CONFIG_NAME,
    RuntimePaths,
    SpaceSettings,
    load_spaces,
    normalize_space_name,
    resolve_home,
    save_spaces,
)
from memoro.errors import ConfigurationError


def test_home_flag_takes_priority_over_environment(tmp_path: Path) -> None:
    cli_home = tmp_path / "from-cli"
    env_home = tmp_path / "from-environment"

    assert resolve_home(cli_home, {"MEMORO_HOME": str(env_home)}) == cli_home.resolve()


def test_home_uses_environment_then_default(tmp_path: Path) -> None:
    env_home = tmp_path / "from-environment"

    assert resolve_home(None, {"MEMORO_HOME": str(env_home)}) == env_home.resolve()
    assert resolve_home(None, {}) == DEFAULT_HOME.expanduser().resolve(strict=False)


@pytest.mark.parametrize(
    ("cli_home", "environment"),
    [
        ("", {}),
        ("   ", {"MEMORO_HOME": "ignored"}),
        (None, {"MEMORO_HOME": "\t"}),
    ],
)
def test_home_rejects_an_explicit_empty_value(
    cli_home: str | None,
    environment: dict[str, str],
) -> None:
    with pytest.raises(ConfigurationError, match="empty"):
        resolve_home(cli_home, environment)


def test_runtime_paths_locate_space_repositories(tmp_path: Path) -> None:
    paths = RuntimePaths(tmp_path)

    assert paths.spaces == tmp_path / "spaces"
    assert paths.space("personal") == tmp_path / "spaces" / "personal"
    assert paths.space("  Work ") == tmp_path / "spaces" / "work"
    with pytest.raises(ConfigurationError, match="invalid"):
        paths.space("../escape")


def test_space_names_are_normalized_and_validated() -> None:
    assert normalize_space_name("personal") == "personal"
    assert normalize_space_name("  Work-2 ") == "work-2"

    with pytest.raises(ConfigurationError, match="must be a string"):
        normalize_space_name(None)  # type: ignore[arg-type]
    with pytest.raises(ConfigurationError, match="empty"):
        normalize_space_name("   ")
    with pytest.raises(ConfigurationError, match="reserved"):
        normalize_space_name("all")
    for invalid in ("a b", "a_b", "a/b", "a.b", "x" * 33, "名字", "-a", "a-", "-a-", "--"):
        with pytest.raises(ConfigurationError, match="invalid"):
            normalize_space_name(invalid)


def test_load_spaces_defaults_to_the_implicit_personal_space(tmp_path: Path) -> None:
    assert load_spaces(tmp_path / "missing-home") == {
        DEFAULT_SPACE: SpaceSettings(readonly=False)
    }


def test_save_and_load_round_trip_merges_the_personal_space(tmp_path: Path) -> None:
    home = tmp_path / "home"
    registry = {"work": SpaceSettings(readonly=True)}

    path = save_spaces(home, registry)

    assert path == home / LOCAL_CONFIG_NAME
    assert load_spaces(home) == {
        "personal": SpaceSettings(readonly=False),
        "work": SpaceSettings(readonly=True),
    }


def test_save_spaces_writes_canonical_json(tmp_path: Path) -> None:
    home = tmp_path / "home"

    path = save_spaces(
        home,
        {
            "work": SpaceSettings(readonly=True),
            "archive": SpaceSettings(readonly=False),
        },
    )
    text = path.read_text(encoding="utf-8")

    assert text == json.dumps(
        {
            "spaces": {
                "archive": {"readonly": False},
                "work": {"readonly": True},
            }
        },
        ensure_ascii=False,
        indent=2,
        sort_keys=True,
    ) + "\n"


def test_personal_space_can_never_be_readonly(tmp_path: Path) -> None:
    home = tmp_path / "home"

    with pytest.raises(ConfigurationError, match="always writable"):
        save_spaces(home, {"personal": SpaceSettings(readonly=True)})

    home.mkdir()
    (home / LOCAL_CONFIG_NAME).write_text(
        '{"spaces": {"personal": {"readonly": true}}}', encoding="utf-8"
    )
    with pytest.raises(ConfigurationError, match="always writable"):
        load_spaces(home)


def test_explicit_writable_personal_entry_is_accepted(tmp_path: Path) -> None:
    home = tmp_path / "home"
    save_spaces(home, {"personal": SpaceSettings(readonly=False)})

    assert load_spaces(home) == {"personal": SpaceSettings(readonly=False)}


def test_save_spaces_rejects_invalid_names_and_settings(tmp_path: Path) -> None:
    home = tmp_path / "home"

    with pytest.raises(ConfigurationError, match="reserved"):
        save_spaces(home, {"all": SpaceSettings(readonly=False)})
    with pytest.raises(ConfigurationError, match="not normalized"):
        save_spaces(home, {"Work": SpaceSettings(readonly=False)})
    with pytest.raises(ConfigurationError, match="invalid settings"):
        save_spaces(home, {"work": {"readonly": False}})  # type: ignore[dict-item]
    assert not (home / LOCAL_CONFIG_NAME).exists()


@pytest.mark.parametrize(
    "content",
    [
        "not json",
        "[]",
        "{}",
        '{"spaces": {}, "extra": true}',
        '{"spaces": []}',
        '{"spaces": {"Work": {"readonly": false}}}',
        '{"spaces": {"all": {"readonly": false}}}',
        '{"spaces": {"work": {}}}',
        '{"spaces": {"work": {"readonly": "no"}}}',
        '{"spaces": {"work": {"readonly": false, "extra": 1}}}',
    ],
)
def test_invalid_local_configuration_is_rejected(tmp_path: Path, content: str) -> None:
    home = tmp_path / "home"
    home.mkdir()
    (home / LOCAL_CONFIG_NAME).write_text(content, encoding="utf-8")

    with pytest.raises(ConfigurationError):
        load_spaces(home)
