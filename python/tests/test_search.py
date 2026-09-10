from __future__ import annotations

import pytest

from memoro.errors import MemoryValidationError
from memoro.models import Memory, memory_path
from memoro.search import (
    DEFAULT_LIMIT,
    DEFAULT_SNIPPET_BUDGET,
    MAX_LIMIT,
    SearchResult,
    search_memories,
    tokenize,
)

BASE_TIME = "2026-08-22T01:02:03.000000Z"
LATER_TIME = "2026-08-23T01:02:03.000000Z"


def _memory_id(index: int) -> str:
    return f"01ARZ3NDEKTSV4RRFFQ69G5{index:03d}"


def _memory(
    index: int,
    *,
    title: str,
    summary: str = "Plain summary.",
    body: str = "Plain body content.",
    tags: tuple[str, ...] = (),
    kind: str = "persona",
    project: str | None = None,
    updated_at: str = BASE_TIME,
) -> Memory:
    memory_id = _memory_id(index)
    return Memory(
        id=memory_id,
        title=title,
        summary=summary,
        tags=tags,
        created_at=BASE_TIME,
        updated_at=updated_at,
        body=body,
        kind=kind,
        project=project,
        relative_path=memory_path(memory_id, kind, project),
    )


def _fillers(
    start: int,
    count: int,
    *,
    kind: str = "persona",
    project: str | None = None,
    space: str = "personal",
) -> list[tuple[str, Memory]]:
    return [
        (
            space,
            _memory(
                start + offset,
                title=f"Quiet note {start + offset:02d}",
                summary="Nothing relevant here.",
                body="Routine unrelated material only.",
                kind=kind,
                project=project,
            ),
        )
        for offset in range(count)
    ]


def _chinese_corpus() -> list[tuple[str, Memory]]:
    deploy = _memory(
        1,
        title="部署流程",
        summary="服务上线的标准部署流程。",
        body="先构建镜像，再执行部署，最后验证服务状态。",
    )
    database = _memory(
        2,
        title="数据库排错",
        summary="数据库连接异常的排错步骤。",
        body="遇到连接超时，先检查数据库配置，再查看慢查询日志。",
    )
    release = _memory(
        3,
        title="Release checklist",
        summary="Steps to publish a release.",
        body="Tag the release and publish the artifacts.",
    )
    return [("personal", deploy), ("personal", database), ("personal", release)]


def test_tokenize_english_words() -> None:
    assert tokenize("Deploy the app, v2 today!") == ["deploy", "the", "app", "v2", "today"]


def test_tokenize_chinese_bigrams() -> None:
    assert tokenize("部署流程") == ["部署", "署流", "流程"]


def test_tokenize_mixed_scripts() -> None:
    assert tokenize("在k8s上部署") == ["在", "k8s", "上部", "部署"]


def test_tokenize_single_cjk_character() -> None:
    assert tokenize("好") == ["好"]
    assert tokenize("part 一 done") == ["part", "一", "done"]


def test_tokenize_applies_nfkc_and_casefold() -> None:
    assert tokenize("Ｄｅｐｌｏｙ ＮＯＷ") == ["deploy", "now"]
    assert tokenize("Straße") == ["strasse"]


def test_chinese_query_hits_deploy_memory() -> None:
    entries = _chinese_corpus()
    result = search_memories(entries, "部署")
    assert [hit.memory.title for hit in result.hits] == ["部署流程"]
    assert result.total_matches == 1
    assert result.truncated is False
    assert search_memories(entries, "部署") == result


def test_chinese_query_with_different_wording_hits_database_memory() -> None:
    result = search_memories(_chinese_corpus(), "排查数据库问题")
    assert [hit.memory.title for hit in result.hits] == ["数据库排错"]
    assert result.total_matches == 1


def test_title_match_ranks_above_body_match() -> None:
    title_hit = _memory(
        1,
        title="Kubernetes rollout checklist",
        summary="How to roll out safely.",
        body="Follow the standard procedure.",
    )
    body_hit = _memory(
        2,
        title="General rollout checklist",
        summary="How to roll out safely.",
        body="Follow the kubernetes procedure.",
    )
    entries = [("personal", title_hit), ("personal", body_hit), *_fillers(10, 3)]
    result = search_memories(entries, "kubernetes")
    assert [hit.memory.id for hit in result.hits] == [_memory_id(1), _memory_id(2)]
    assert result.hits[0].score > result.hits[1].score


def test_tag_match_ranks_above_body_match() -> None:
    tag_hit = _memory(
        1,
        title="Cluster upgrade notes",
        summary="Steps for the upgrade.",
        body="Drain nodes before upgrading.",
        tags=("kubernetes",),
    )
    body_hit = _memory(
        2,
        title="Cluster upgrade draft",
        summary="Steps for the upgrade.",
        body="Drain kubernetes nodes first.",
    )
    entries = [("personal", tag_hit), ("personal", body_hit), *_fillers(10, 3)]
    result = search_memories(entries, "kubernetes")
    assert [hit.memory.id for hit in result.hits] == [_memory_id(1), _memory_id(2)]


def test_kind_filter_limits_scoring_corpus() -> None:
    persona_doc = _memory(1, title="Deploy habits", body="Deploy with a checklist.")
    project_doc = _memory(
        2,
        title="Deploy runbook",
        body="Deploy after the tests pass.",
        kind="project",
        project="memoro",
    )
    entries = [
        ("personal", persona_doc),
        *_fillers(10, 2),
        ("personal", project_doc),
        *_fillers(20, 2, kind="project", project="memoro"),
    ]
    persona_result = search_memories(entries, "deploy", kind="persona")
    project_result = search_memories(entries, "deploy", kind="project")
    assert [hit.memory.id for hit in persona_result.hits] == [_memory_id(1)]
    assert [hit.memory.id for hit in project_result.hits] == [_memory_id(2)]


def test_project_filter_selects_matching_project() -> None:
    memoro_doc = _memory(
        1,
        title="Deploy runbook",
        body="Deploy after the tests pass.",
        kind="project",
        project="memoro",
    )
    side_doc = _memory(
        2,
        title="Deploy scratchpad",
        body="Deploy manually for now.",
        kind="project",
        project="sideproject",
    )
    entries = [
        ("personal", memoro_doc),
        *_fillers(10, 2, kind="project", project="memoro"),
        ("personal", side_doc),
        *_fillers(20, 2, kind="project", project="sideproject"),
    ]
    result = search_memories(entries, "deploy", kind="project", project="memoro")
    assert [hit.memory.id for hit in result.hits] == [_memory_id(1)]
    without_kind = search_memories(entries, "deploy", project="sideproject")
    assert [hit.memory.id for hit in without_kind.hits] == [_memory_id(2)]


def test_hits_carry_their_space_names() -> None:
    personal_doc = _memory(1, title="Deploy habits", body="Deploy with a checklist.")
    team_doc = _memory(2, title="Deploy runbook", body="Deploy after review.")
    entries = [
        ("personal", personal_doc),
        *_fillers(10, 2),
        ("team", team_doc),
        *_fillers(20, 2, space="team"),
    ]
    result = search_memories(entries, "deploy")
    assert {hit.memory.id: hit.space for hit in result.hits} == {
        _memory_id(1): "personal",
        _memory_id(2): "team",
    }
    team_only = search_memories([entry for entry in entries if entry[0] == "team"], "deploy")
    assert [(hit.space, hit.memory.id) for hit in team_only.hits] == [("team", _memory_id(2))]


def _deploy_notes(count: int) -> list[tuple[str, Memory]]:
    return [
        (
            "personal",
            _memory(
                index,
                title=f"Deploy note {index:02d}",
                body="Deploy carefully.",
                updated_at=f"2026-08-{10 + index:02d}T01:02:03.000000Z",
            ),
        )
        for index in range(1, count + 1)
    ]


def test_default_limit_caps_hits_and_reports_truncation() -> None:
    entries = [*_deploy_notes(4), *_fillers(50, 5)]
    result = search_memories(entries, "deploy")
    assert len(result.hits) == DEFAULT_LIMIT
    assert [hit.memory.id for hit in result.hits] == [
        _memory_id(4),
        _memory_id(3),
        _memory_id(2),
    ]
    assert result.total_matches == 4
    assert result.truncated is True


def test_limit_clamps_to_lower_bound() -> None:
    entries = [*_deploy_notes(4), *_fillers(50, 5)]
    for limit in (0, -5):
        result = search_memories(entries, "deploy", limit=limit)
        assert [hit.memory.id for hit in result.hits] == [_memory_id(4)]
        assert result.truncated is True


def test_limit_clamps_to_max_limit() -> None:
    entries = [*_deploy_notes(10), *_fillers(50, 12)]
    result = search_memories(entries, "deploy", limit=99)
    assert len(result.hits) == MAX_LIMIT
    assert result.total_matches == 10
    assert result.truncated is True


def test_query_without_overlap_returns_empty_result() -> None:
    entries = _chinese_corpus()
    empty = SearchResult(hits=(), total_matches=0, truncated=False)
    assert search_memories(entries, "quantum entanglement") == empty
    assert search_memories(entries, "!!!") == empty


def test_blank_query_is_rejected() -> None:
    entries = _chinese_corpus()
    for query in ("", "   ", "\n\t"):
        with pytest.raises(MemoryValidationError, match="query"):
            search_memories(entries, query)


def test_snippet_returns_short_body_untruncated() -> None:
    doc = _memory(1, title="Deploy habits", body="Deploy with a checklist.")
    entries = [("personal", doc), *_fillers(10, 2)]
    hit = search_memories(entries, "deploy").hits[0]
    assert hit.snippet == "Deploy with a checklist."
    assert hit.snippet_truncated is False


def test_snippet_centers_on_match_in_long_body() -> None:
    before = "\n".join(f"leading line {index:02d} with steady padding text" for index in range(30))
    after = "\n".join(f"trailing line {index:02d} with steady padding" for index in range(30))
    body = f"{before}\nthe kubernetes upgrade steps live here\n{after}"
    doc = _memory(1, title="Cluster notes", body=body)
    entries = [("personal", doc), *_fillers(10, 2)]
    hit = search_memories(entries, "kubernetes").hits[0]
    assert hit.snippet_truncated is True
    assert hit.snippet.startswith("…")
    assert hit.snippet.endswith("…")
    assert "kubernetes" in hit.snippet
    assert len(hit.snippet) <= DEFAULT_SNIPPET_BUDGET + 2


def test_snippet_anchors_on_highest_scoring_token() -> None:
    padding = "\n".join(f"log line {index:03d} nothing special" for index in range(50))
    body = f"alpha incident intro\n{padding}\nzulu remediation applied\nclosing remarks"
    target = _memory(1, title="Incident review", body=body)
    alpha_one = _memory(2, title="Alpha weekly", body="alpha summary only")
    alpha_two = _memory(3, title="Alpha planning", body="alpha follow ups")
    entries = [("personal", target), ("personal", alpha_one), ("personal", alpha_two)]
    result = search_memories(entries, "alpha zulu")
    assert [hit.memory.id for hit in result.hits] == [_memory_id(1)]
    hit = result.hits[0]
    assert "zulu" in hit.snippet
    assert "alpha" not in hit.snippet
    assert hit.snippet.startswith("…")


def test_snippet_falls_back_to_body_start_without_body_match() -> None:
    body = "\n".join(f"routine paragraph {index:02d} with steady text" for index in range(40))
    doc = _memory(1, title="Kubernetes upgrade guide", body=body)
    entries = [("personal", doc), *_fillers(10, 2)]
    hit = search_memories(entries, "kubernetes").hits[0]
    assert hit.snippet_truncated is True
    assert not hit.snippet.startswith("…")
    assert hit.snippet.startswith("routine paragraph 00")
    assert hit.snippet.endswith("…")


def test_total_budget_truncates_hits() -> None:
    def _doc(index: int, word: str, updated_at: str) -> Memory:
        detail = "\n".join(f"detail line {line:02d} with routine text" for line in range(24))
        return _memory(
            index,
            title=f"{word.capitalize()} " + "t" * 90,
            summary=f"{word} " + "s" * 270,
            body=f"{word} remediation record\n{detail}",
            updated_at=updated_at,
        )

    entries = [
        ("personal", _doc(1, "alpha", "2026-08-24T01:02:03.000000Z")),
        ("personal", _doc(2, "beta", LATER_TIME)),
        ("personal", _doc(3, "gamma", BASE_TIME)),
    ]
    result = search_memories(entries, "alpha beta gamma")
    assert result.total_matches == 3
    assert [hit.memory.id for hit in result.hits] == [_memory_id(1), _memory_id(2)]
    assert result.truncated is True


def test_equal_scores_break_ties_by_updated_at_then_id() -> None:
    def _same(index: int, updated_at: str) -> Memory:
        return _memory(
            index,
            title="Deploy ritual",
            summary="Deploy summary.",
            body="Deploy checklist body.",
            updated_at=updated_at,
        )

    entries = [
        ("personal", _same(1, BASE_TIME)),
        ("personal", _same(2, LATER_TIME)),
        ("personal", _same(3, BASE_TIME)),
        *_fillers(10, 4),
    ]
    result = search_memories(entries, "deploy")
    assert [hit.memory.id for hit in result.hits] == [
        _memory_id(2),
        _memory_id(3),
        _memory_id(1),
    ]
    assert result.truncated is False
