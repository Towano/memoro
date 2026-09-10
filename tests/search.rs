//! Mirrors `python/tests/test_search.py` case by case; the tokenizer rules,
//! BM25F ranking, and snippet windowing are the semantic contract.

use std::collections::HashMap;

use memoro::errors::MemoroError;
use memoro::models::{memory_path, Memory};
use memoro::search::{
    search_memories, tokenize, SearchResult, DEFAULT_LIMIT, DEFAULT_SNIPPET_BUDGET, MAX_LIMIT,
};

const BASE_TIME: &str = "2026-08-22T01:02:03.000000Z";
const LATER_TIME: &str = "2026-08-23T01:02:03.000000Z";

fn memory_id(index: usize) -> String {
    format!("01ARZ3NDEKTSV4RRFFQ69G5{index:03}")
}

/// The keyword arguments of Python's `_memory` helper, with the same defaults.
struct Spec<'a> {
    title: &'a str,
    summary: &'a str,
    body: &'a str,
    tags: Vec<&'a str>,
    kind: &'a str,
    project: Option<&'a str>,
    updated_at: &'a str,
}

impl<'a> Spec<'a> {
    fn new(title: &'a str) -> Self {
        Spec {
            title,
            summary: "Plain summary.",
            body: "Plain body content.",
            tags: Vec::new(),
            kind: "persona",
            project: None,
            updated_at: BASE_TIME,
        }
    }

    fn summary(mut self, summary: &'a str) -> Self {
        self.summary = summary;
        self
    }

    fn body(mut self, body: &'a str) -> Self {
        self.body = body;
        self
    }

    fn tags(mut self, tags: Vec<&'a str>) -> Self {
        self.tags = tags;
        self
    }

    fn kind(mut self, kind: &'a str) -> Self {
        self.kind = kind;
        self
    }

    fn project(mut self, project: Option<&'a str>) -> Self {
        self.project = project;
        self
    }

    fn updated_at(mut self, updated_at: &'a str) -> Self {
        self.updated_at = updated_at;
        self
    }

    fn build(self, index: usize) -> Memory {
        let id = memory_id(index);
        Memory {
            relative_path: memory_path(&id, self.kind, self.project).unwrap(),
            id,
            title: self.title.to_string(),
            summary: self.summary.to_string(),
            tags: self.tags.iter().map(|tag| tag.to_string()).collect(),
            created_at: BASE_TIME.to_string(),
            updated_at: self.updated_at.to_string(),
            body: self.body.to_string(),
            kind: self.kind.to_string(),
            project: self.project.map(str::to_string),
        }
    }
}

fn fillers(
    start: usize,
    count: usize,
    kind: &str,
    project: Option<&str>,
    space: &'static str,
) -> Vec<(&'static str, Memory)> {
    (0..count)
        .map(|offset| {
            let index = start + offset;
            (
                space,
                Spec::new(&format!("Quiet note {index:02}"))
                    .summary("Nothing relevant here.")
                    .body("Routine unrelated material only.")
                    .kind(kind)
                    .project(project)
                    .build(index),
            )
        })
        .collect()
}

fn chinese_corpus() -> Vec<(&'static str, Memory)> {
    vec![
        (
            "personal",
            Spec::new("部署流程")
                .summary("服务上线的标准部署流程。")
                .body("先构建镜像，再执行部署，最后验证服务状态。")
                .build(1),
        ),
        (
            "personal",
            Spec::new("数据库排错")
                .summary("数据库连接异常的排错步骤。")
                .body("遇到连接超时，先检查数据库配置，再查看慢查询日志。")
                .build(2),
        ),
        (
            "personal",
            Spec::new("Release checklist")
                .summary("Steps to publish a release.")
                .body("Tag the release and publish the artifacts.")
                .build(3),
        ),
    ]
}

fn deploy_notes(count: usize) -> Vec<(&'static str, Memory)> {
    (1..=count)
        .map(|index| {
            (
                "personal",
                Spec::new(&format!("Deploy note {index:02}"))
                    .body("Deploy carefully.")
                    .updated_at(&format!("2026-08-{:02}T01:02:03.000000Z", 10 + index))
                    .build(index),
            )
        })
        .collect()
}

fn entry_refs<'a>(entries: &'a [(&'a str, Memory)]) -> Vec<(&'a str, &'a Memory)> {
    entries
        .iter()
        .map(|(space, memory)| (*space, memory))
        .collect()
}

fn search(entries: &[(&str, Memory)], query: &str) -> SearchResult {
    search_memories(&entry_refs(entries), query, None, None, DEFAULT_LIMIT).unwrap()
}

fn search_with(
    entries: &[(&str, Memory)],
    query: &str,
    kind: Option<&str>,
    project: Option<&str>,
    limit: i64,
) -> SearchResult {
    search_memories(&entry_refs(entries), query, kind, project, limit).unwrap()
}

fn ids(result: &SearchResult) -> Vec<String> {
    result
        .hits
        .iter()
        .map(|hit| hit.memory.id.clone())
        .collect()
}

#[test]
fn tokenize_english_words() {
    assert_eq!(
        tokenize("Deploy the app, v2 today!"),
        ["deploy", "the", "app", "v2", "today"]
    );
}

#[test]
fn tokenize_chinese_bigrams() {
    assert_eq!(tokenize("部署流程"), ["部署", "署流", "流程"]);
}

#[test]
fn tokenize_mixed_scripts() {
    assert_eq!(tokenize("在k8s上部署"), ["在", "k8s", "上部", "部署"]);
}

#[test]
fn tokenize_single_cjk_character() {
    assert_eq!(tokenize("好"), ["好"]);
    assert_eq!(tokenize("part 一 done"), ["part", "一", "done"]);
}

#[test]
fn tokenize_applies_nfkc_and_casefold() {
    assert_eq!(tokenize("Ｄｅｐｌｏｙ ＮＯＷ"), ["deploy", "now"]);
    assert_eq!(tokenize("Straße"), ["strasse"]);
}

#[test]
fn chinese_query_hits_deploy_memory() {
    let entries = chinese_corpus();
    let result = search(&entries, "部署");
    let titles: Vec<&str> = result
        .hits
        .iter()
        .map(|hit| hit.memory.title.as_str())
        .collect();
    assert_eq!(titles, ["部署流程"]);
    assert_eq!(result.total_matches, 1);
    assert!(!result.truncated);
    assert_eq!(search(&entries, "部署"), result);
}

#[test]
fn chinese_query_with_different_wording_hits_database_memory() {
    let result = search(&chinese_corpus(), "排查数据库问题");
    let titles: Vec<&str> = result
        .hits
        .iter()
        .map(|hit| hit.memory.title.as_str())
        .collect();
    assert_eq!(titles, ["数据库排错"]);
    assert_eq!(result.total_matches, 1);
}

#[test]
fn title_match_ranks_above_body_match() {
    let title_hit = Spec::new("Kubernetes rollout checklist")
        .summary("How to roll out safely.")
        .body("Follow the standard procedure.")
        .build(1);
    let body_hit = Spec::new("General rollout checklist")
        .summary("How to roll out safely.")
        .body("Follow the kubernetes procedure.")
        .build(2);
    let mut entries = vec![("personal", title_hit), ("personal", body_hit)];
    entries.extend(fillers(10, 3, "persona", None, "personal"));
    let result = search(&entries, "kubernetes");
    assert_eq!(ids(&result), vec![memory_id(1), memory_id(2)]);
    assert!(result.hits[0].score > result.hits[1].score);
}

#[test]
fn tag_match_ranks_above_body_match() {
    let tag_hit = Spec::new("Cluster upgrade notes")
        .summary("Steps for the upgrade.")
        .body("Drain nodes before upgrading.")
        .tags(vec!["kubernetes"])
        .build(1);
    let body_hit = Spec::new("Cluster upgrade draft")
        .summary("Steps for the upgrade.")
        .body("Drain kubernetes nodes first.")
        .build(2);
    let mut entries = vec![("personal", tag_hit), ("personal", body_hit)];
    entries.extend(fillers(10, 3, "persona", None, "personal"));
    let result = search(&entries, "kubernetes");
    assert_eq!(ids(&result), vec![memory_id(1), memory_id(2)]);
}

#[test]
fn kind_filter_limits_scoring_corpus() {
    let persona_doc = Spec::new("Deploy habits")
        .body("Deploy with a checklist.")
        .build(1);
    let project_doc = Spec::new("Deploy runbook")
        .body("Deploy after the tests pass.")
        .kind("project")
        .project(Some("memoro"))
        .build(2);
    let mut entries = vec![("personal", persona_doc)];
    entries.extend(fillers(10, 2, "persona", None, "personal"));
    entries.push(("personal", project_doc));
    entries.extend(fillers(20, 2, "project", Some("memoro"), "personal"));
    let persona_result = search_with(&entries, "deploy", Some("persona"), None, DEFAULT_LIMIT);
    let project_result = search_with(&entries, "deploy", Some("project"), None, DEFAULT_LIMIT);
    assert_eq!(ids(&persona_result), vec![memory_id(1)]);
    assert_eq!(ids(&project_result), vec![memory_id(2)]);
}

#[test]
fn project_filter_selects_matching_project() {
    let memoro_doc = Spec::new("Deploy runbook")
        .body("Deploy after the tests pass.")
        .kind("project")
        .project(Some("memoro"))
        .build(1);
    let side_doc = Spec::new("Deploy scratchpad")
        .body("Deploy manually for now.")
        .kind("project")
        .project(Some("sideproject"))
        .build(2);
    let mut entries = vec![("personal", memoro_doc)];
    entries.extend(fillers(10, 2, "project", Some("memoro"), "personal"));
    entries.push(("personal", side_doc));
    entries.extend(fillers(20, 2, "project", Some("sideproject"), "personal"));
    let result = search_with(
        &entries,
        "deploy",
        Some("project"),
        Some("memoro"),
        DEFAULT_LIMIT,
    );
    assert_eq!(ids(&result), vec![memory_id(1)]);
    let without_kind = search_with(&entries, "deploy", None, Some("sideproject"), DEFAULT_LIMIT);
    assert_eq!(ids(&without_kind), vec![memory_id(2)]);
}

#[test]
fn hits_carry_their_space_names() {
    let personal_doc = Spec::new("Deploy habits")
        .body("Deploy with a checklist.")
        .build(1);
    let team_doc = Spec::new("Deploy runbook")
        .body("Deploy after review.")
        .build(2);
    let mut entries = vec![("personal", personal_doc)];
    entries.extend(fillers(10, 2, "persona", None, "personal"));
    entries.push(("team", team_doc));
    entries.extend(fillers(20, 2, "persona", None, "team"));
    let result = search(&entries, "deploy");
    let spaces_by_id: HashMap<String, &str> = result
        .hits
        .iter()
        .map(|hit| (hit.memory.id.clone(), hit.space.as_str()))
        .collect();
    let mut expected: HashMap<String, &str> = HashMap::new();
    expected.insert(memory_id(1), "personal");
    expected.insert(memory_id(2), "team");
    assert_eq!(spaces_by_id, expected);
    let team_entries: Vec<(&str, Memory)> = entries
        .iter()
        .filter(|(space, _)| *space == "team")
        .cloned()
        .collect();
    let team_only = search(&team_entries, "deploy");
    let pairs: Vec<(String, String)> = team_only
        .hits
        .iter()
        .map(|hit| (hit.space.clone(), hit.memory.id.clone()))
        .collect();
    assert_eq!(pairs, vec![("team".to_string(), memory_id(2))]);
}

#[test]
fn default_limit_caps_hits_and_reports_truncation() {
    let mut entries = deploy_notes(4);
    entries.extend(fillers(50, 5, "persona", None, "personal"));
    let result = search(&entries, "deploy");
    assert_eq!(result.hits.len(), DEFAULT_LIMIT as usize);
    assert_eq!(ids(&result), vec![memory_id(4), memory_id(3), memory_id(2)]);
    assert_eq!(result.total_matches, 4);
    assert!(result.truncated);
}

#[test]
fn limit_clamps_to_lower_bound() {
    let mut entries = deploy_notes(4);
    entries.extend(fillers(50, 5, "persona", None, "personal"));
    for limit in [0, -5] {
        let result = search_with(&entries, "deploy", None, None, limit);
        assert_eq!(ids(&result), vec![memory_id(4)]);
        assert!(result.truncated);
    }
}

#[test]
fn limit_clamps_to_max_limit() {
    let mut entries = deploy_notes(10);
    entries.extend(fillers(50, 12, "persona", None, "personal"));
    let result = search_with(&entries, "deploy", None, None, 99);
    assert_eq!(result.hits.len(), MAX_LIMIT as usize);
    assert_eq!(result.total_matches, 10);
    assert!(result.truncated);
}

#[test]
fn query_without_overlap_returns_empty_result() {
    let entries = chinese_corpus();
    let empty = SearchResult {
        hits: Vec::new(),
        total_matches: 0,
        truncated: false,
    };
    assert_eq!(search(&entries, "quantum entanglement"), empty);
    assert_eq!(search(&entries, "!!!"), empty);
}

#[test]
fn blank_query_is_rejected() {
    let entries = chinese_corpus();
    for query in ["", "   ", "\n\t"] {
        let error =
            search_memories(&entry_refs(&entries), query, None, None, DEFAULT_LIMIT).unwrap_err();
        match error {
            MemoroError::MemoryValidation(message) => assert!(message.contains("query")),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}

#[test]
fn snippet_returns_short_body_untruncated() {
    let doc = Spec::new("Deploy habits")
        .body("Deploy with a checklist.")
        .build(1);
    let mut entries = vec![("personal", doc)];
    entries.extend(fillers(10, 2, "persona", None, "personal"));
    let hit = &search(&entries, "deploy").hits[0];
    assert_eq!(hit.snippet, "Deploy with a checklist.");
    assert!(!hit.snippet_truncated);
}

#[test]
fn snippet_centers_on_match_in_long_body() {
    let before: Vec<String> = (0..30)
        .map(|index| format!("leading line {index:02} with steady padding text"))
        .collect();
    let after: Vec<String> = (0..30)
        .map(|index| format!("trailing line {index:02} with steady padding"))
        .collect();
    let body = format!(
        "{}\nthe kubernetes upgrade steps live here\n{}",
        before.join("\n"),
        after.join("\n")
    );
    let doc = Spec::new("Cluster notes").body(&body).build(1);
    let mut entries = vec![("personal", doc)];
    entries.extend(fillers(10, 2, "persona", None, "personal"));
    let hit = &search(&entries, "kubernetes").hits[0];
    assert!(hit.snippet_truncated);
    assert!(hit.snippet.starts_with("…"));
    assert!(hit.snippet.ends_with("…"));
    assert!(hit.snippet.contains("kubernetes"));
    assert!(hit.snippet.chars().count() <= DEFAULT_SNIPPET_BUDGET + 2);
}

#[test]
fn snippet_anchors_on_highest_scoring_token() {
    let padding: Vec<String> = (0..50)
        .map(|index| format!("log line {index:03} nothing special"))
        .collect();
    let body = format!(
        "alpha incident intro\n{}\nzulu remediation applied\nclosing remarks",
        padding.join("\n")
    );
    let target = Spec::new("Incident review").body(&body).build(1);
    let alpha_one = Spec::new("Alpha weekly")
        .body("alpha summary only")
        .build(2);
    let alpha_two = Spec::new("Alpha planning")
        .body("alpha follow ups")
        .build(3);
    let entries = vec![
        ("personal", target),
        ("personal", alpha_one),
        ("personal", alpha_two),
    ];
    let result = search(&entries, "alpha zulu");
    assert_eq!(ids(&result), vec![memory_id(1)]);
    let hit = &result.hits[0];
    assert!(hit.snippet.contains("zulu"));
    assert!(!hit.snippet.contains("alpha"));
    assert!(hit.snippet.starts_with("…"));
}

#[test]
fn snippet_falls_back_to_body_start_without_body_match() {
    let paragraphs: Vec<String> = (0..40)
        .map(|index| format!("routine paragraph {index:02} with steady text"))
        .collect();
    let body = paragraphs.join("\n");
    let doc = Spec::new("Kubernetes upgrade guide").body(&body).build(1);
    let mut entries = vec![("personal", doc)];
    entries.extend(fillers(10, 2, "persona", None, "personal"));
    let hit = &search(&entries, "kubernetes").hits[0];
    assert!(hit.snippet_truncated);
    assert!(!hit.snippet.starts_with("…"));
    assert!(hit.snippet.starts_with("routine paragraph 00"));
    assert!(hit.snippet.ends_with("…"));
}

#[test]
fn total_budget_truncates_hits() {
    fn capitalize(word: &str) -> String {
        let mut characters = word.chars();
        match characters.next() {
            Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
            None => String::new(),
        }
    }

    fn doc(index: usize, word: &str, updated_at: &str) -> Memory {
        let detail: Vec<String> = (0..24)
            .map(|line| format!("detail line {line:02} with routine text"))
            .collect();
        Spec::new(&format!("{} {}", capitalize(word), "t".repeat(90)))
            .summary(&format!("{word} {}", "s".repeat(270)))
            .body(&format!("{word} remediation record\n{}", detail.join("\n")))
            .updated_at(updated_at)
            .build(index)
    }

    let entries = vec![
        ("personal", doc(1, "alpha", "2026-08-24T01:02:03.000000Z")),
        ("personal", doc(2, "beta", LATER_TIME)),
        ("personal", doc(3, "gamma", BASE_TIME)),
    ];
    let result = search(&entries, "alpha beta gamma");
    assert_eq!(result.total_matches, 3);
    assert_eq!(ids(&result), vec![memory_id(1), memory_id(2)]);
    assert!(result.truncated);
}

#[test]
fn equal_scores_break_ties_by_updated_at_then_id() {
    fn same(index: usize, updated_at: &str) -> Memory {
        Spec::new("Deploy ritual")
            .summary("Deploy summary.")
            .body("Deploy checklist body.")
            .updated_at(updated_at)
            .build(index)
    }

    let mut entries = vec![
        ("personal", same(1, BASE_TIME)),
        ("personal", same(2, LATER_TIME)),
        ("personal", same(3, BASE_TIME)),
    ];
    entries.extend(fillers(10, 4, "persona", None, "personal"));
    let result = search(&entries, "deploy");
    assert_eq!(ids(&result), vec![memory_id(2), memory_id(3), memory_id(1)]);
    assert!(!result.truncated);
}
