"""In-memory memory search: word/CJK-bigram tokens scored with a simplified BM25F."""

from __future__ import annotations

import math
import unicodedata
from bisect import bisect_right
from collections.abc import Sequence
from dataclasses import dataclass

from memoro.errors import MemoryValidationError
from memoro.models import Memory

DEFAULT_LIMIT = 3
MAX_LIMIT = 8
DEFAULT_SNIPPET_BUDGET = 700
DEFAULT_TOTAL_BUDGET = 2400

_BM25_K1 = 1.2
_BM25_B = 0.75
_FIELD_WEIGHTS = (("title", 3.0), ("tags", 2.5), ("summary", 2.0), ("body", 1.0))
_SNIPPET_ALIGNMENT = 80

# Han, Hiragana, Katakana, and Hangul blocks, including the extension blocks.
_CJK_RANGES = (
    (0x1100, 0x11FF),  # Hangul Jamo
    (0x3040, 0x309F),  # Hiragana
    (0x30A0, 0x30FF),  # Katakana
    (0x3130, 0x318F),  # Hangul Compatibility Jamo
    (0x31F0, 0x31FF),  # Katakana Phonetic Extensions
    (0x3400, 0x4DBF),  # CJK Unified Ideographs Extension A
    (0x4E00, 0x9FFF),  # CJK Unified Ideographs
    (0xA960, 0xA97F),  # Hangul Jamo Extended-A
    (0xAC00, 0xD7AF),  # Hangul Syllables
    (0xD7B0, 0xD7FF),  # Hangul Jamo Extended-B
    (0xF900, 0xFAFF),  # CJK Compatibility Ideographs
    (0x1AFF0, 0x1AFFF),  # Kana Extended-B
    (0x1B000, 0x1B0FF),  # Kana Supplement
    (0x1B100, 0x1B12F),  # Kana Extended-A
    (0x1B130, 0x1B16F),  # Small Kana Extension
    (0x20000, 0x2A6DF),  # CJK Unified Ideographs Extension B
    (0x2A700, 0x2EBEF),  # CJK Unified Ideographs Extensions C-F
    (0x2EBF0, 0x2EE5F),  # CJK Unified Ideographs Extension I
    (0x2F800, 0x2FA1F),  # CJK Compatibility Ideographs Supplement
    (0x30000, 0x3134F),  # CJK Unified Ideographs Extension G
    (0x31350, 0x323AF),  # CJK Unified Ideographs Extension H
)
_CJK_STARTS = tuple(start for start, _ in _CJK_RANGES)


@dataclass(frozen=True, slots=True)
class SearchHit:
    space: str
    memory: Memory
    score: float
    snippet: str
    snippet_truncated: bool


@dataclass(frozen=True, slots=True)
class SearchResult:
    hits: tuple[SearchHit, ...]
    total_matches: int
    truncated: bool


def tokenize(text: str) -> list[str]:
    """Split text into casefolded non-CJK words and CJK character bigrams."""

    if not isinstance(text, str):
        raise ValueError("text must be a string")
    normalized = unicodedata.normalize("NFKC", text).casefold()
    tokens: list[str] = []
    run: list[str] = []
    run_is_cjk = False

    def flush() -> None:
        if not run:
            return
        if run_is_cjk and len(run) > 1:
            tokens.extend(run[index] + run[index + 1] for index in range(len(run) - 1))
        else:
            tokens.append("".join(run))
        run.clear()

    for character in normalized:
        is_cjk = _is_cjk_character(character)
        if not is_cjk and not character.isalnum():
            flush()
            continue
        if run and run_is_cjk != is_cjk:
            flush()
        run_is_cjk = is_cjk
        run.append(character)
    flush()
    return tokens


def search_memories(
    entries: Sequence[tuple[str, Memory]],
    query: str,
    *,
    kind: str | None = None,
    project: str | None = None,
    limit: int = DEFAULT_LIMIT,
) -> SearchResult:
    """Rank the given (space, memory) entries against the query with BM25F."""

    if not isinstance(query, str) or not query.strip():
        raise MemoryValidationError(
            "Search query is empty. Provide non-blank text to find memories."
        )
    limit_value = max(1, min(MAX_LIMIT, limit))
    query_terms = list(dict.fromkeys(tokenize(query)))
    filtered = [
        (space, memory)
        for space, memory in entries
        if (kind is None or memory.kind == kind)
        and (project is None or memory.project == project)
    ]
    if not filtered or not query_terms:
        return SearchResult(hits=(), total_matches=0, truncated=False)

    documents = []
    total_length = 0.0
    for space, memory in filtered:
        frequencies, length = _weighted_term_frequencies(memory)
        documents.append((space, memory, frequencies, length))
        total_length += length
    average_length = total_length / len(documents)
    if average_length <= 0:
        return SearchResult(hits=(), total_matches=0, truncated=False)

    corpus_size = len(documents)
    idf: dict[str, float] = {}
    for term in query_terms:
        document_frequency = sum(1 for _, _, frequencies, _ in documents if term in frequencies)
        idf[term] = max(
            0.0,
            math.log((corpus_size - document_frequency + 0.5) / (document_frequency + 0.5)),
        )

    matches = []
    for space, memory, frequencies, length in documents:
        saturation = _BM25_K1 * (1 - _BM25_B + _BM25_B * length / average_length)
        score = 0.0
        contributions: dict[str, float] = {}
        for term in query_terms:
            frequency = frequencies.get(term, 0.0)
            if frequency <= 0 or idf[term] <= 0:
                continue
            contribution = idf[term] * frequency * (_BM25_K1 + 1) / (frequency + saturation)
            score += contribution
            contributions[term] = contribution
        if score > 0:
            matches.append((space, memory, score, contributions))
    matches.sort(key=lambda match: (match[2], match[1].updated_at, match[1].id), reverse=True)

    hits: list[SearchHit] = []
    used_budget = 0
    for space, memory, score, contributions in matches[:limit_value]:
        snippet, snippet_truncated = _build_snippet(memory.body, contributions)
        cost = len(memory.title) + len(memory.summary) + len(snippet)
        if used_budget + cost > DEFAULT_TOTAL_BUDGET:
            break
        used_budget += cost
        hits.append(
            SearchHit(
                space=space,
                memory=memory,
                score=score,
                snippet=snippet,
                snippet_truncated=snippet_truncated,
            )
        )
    return SearchResult(
        hits=tuple(hits),
        total_matches=len(matches),
        truncated=len(hits) < len(matches),
    )


def _is_cjk_character(character: str) -> bool:
    codepoint = ord(character)
    index = bisect_right(_CJK_STARTS, codepoint) - 1
    return index >= 0 and codepoint <= _CJK_RANGES[index][1]


def _weighted_term_frequencies(memory: Memory) -> tuple[dict[str, float], float]:
    frequencies: dict[str, float] = {}
    length = 0.0
    for field, weight in _FIELD_WEIGHTS:
        tokens = _field_tokens(memory, field)
        length += weight * len(tokens)
        for token in tokens:
            frequencies[token] = frequencies.get(token, 0.0) + weight
    return frequencies, length


def _field_tokens(memory: Memory, field: str) -> list[str]:
    if field == "tags":
        tokens: list[str] = []
        for tag in memory.tags:
            tokens.extend(tokenize(tag))
        return tokens
    return tokenize(getattr(memory, field))


def _build_snippet(body: str, contributions: dict[str, float]) -> tuple[str, bool]:
    if len(body) <= DEFAULT_SNIPPET_BUDGET:
        return body, False
    # Positions are located in the folded body; NFKC + casefold is length-preserving
    # for ASCII and CJK text, so the index maps back onto the original body closely
    # enough for a snippet window. The clamp guards the rare length-changing folds.
    folded = unicodedata.normalize("NFKC", body).casefold()
    anchor = 0
    best_rank: tuple[float, int] | None = None
    for term, contribution in contributions.items():
        position = folded.find(term)
        if position < 0:
            continue
        rank = (contribution, -position)
        if best_rank is None or rank > best_rank:
            best_rank = rank
            anchor = min(position, len(body) - 1)
    half_budget = DEFAULT_SNIPPET_BUDGET // 2
    start = max(0, min(anchor - half_budget, len(body) - DEFAULT_SNIPPET_BUDGET))
    end = start + DEFAULT_SNIPPET_BUDGET
    start, end = _align_to_lines(body, start, end)
    prefix = "…" if start > 0 else ""
    suffix = "…" if end < len(body) else ""
    return f"{prefix}{body[start:end]}{suffix}", True


def _align_to_lines(body: str, start: int, end: int) -> tuple[int, int]:
    if start > 0 and body[start - 1] != "\n":
        cut = body.find("\n", start, min(end, start + _SNIPPET_ALIGNMENT))
        if cut >= 0:
            start = cut + 1
    if end < len(body) and body[end] != "\n" and body[end - 1] != "\n":
        cut = body.rfind("\n", max(start, end - _SNIPPET_ALIGNMENT), end)
        if cut >= 0:
            end = cut
    return start, end
