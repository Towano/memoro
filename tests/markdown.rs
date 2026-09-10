//! Mirrors `python/tests/test_markdown.py` (the markdown-specific cases; the
//! models cases live in the `src/models.rs` unit tests). Expected documents,
//! digests, and messages were captured from the Python reference so the
//! assertions are byte-for-byte.

use memoro::errors::MemoroError;
use memoro::markdown::{memory_revision, parse_memory, serialize_memory, FRONTMATTER_FIELDS};
use memoro::models::{Memory, MAX_TAGS};

const MEMORY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const CREATED_AT: &str = "2026-08-22T01:02:03.000000Z";
const SUMMARY_LINE: &str = "summary: \"What the release notes cover.\"";

const CANONICAL_DOCUMENT: &str = "---\n\
id: \"01ARZ3NDEKTSV4RRFFQ69G5FAV\"\n\
title: \"Release notes\"\n\
summary: \"What the release notes cover.\"\n\
created_at: \"2026-08-22T01:02:03.000000Z\"\n\
updated_at: \"2026-08-22T01:02:03.000000Z\"\n\
---\n\
\n\
First line\n\
Second line\n";

fn memory() -> Memory {
    let relative_path = format!("persona/{MEMORY_ID}.md");
    make_memory(Some(&relative_path), "persona", None, &[])
}

fn make_memory(
    relative_path: Option<&str>,
    kind: &str,
    project: Option<&str>,
    tags: &[&str],
) -> Memory {
    let default_path = format!("persona/{MEMORY_ID}.md");
    Memory {
        id: MEMORY_ID.to_string(),
        title: "Release notes".to_string(),
        summary: "What the release notes cover.".to_string(),
        tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        created_at: CREATED_AT.to_string(),
        updated_at: CREATED_AT.to_string(),
        body: "First line\nSecond line".to_string(),
        kind: kind.to_string(),
        project: project.map(|slug| slug.to_string()),
        relative_path: relative_path.unwrap_or(&default_path).to_string(),
    }
}

fn invalid(relative_path: &str, reason: &str) -> String {
    format!(
        "Committed memory '{relative_path}' is invalid: {reason}. \
Repair the Markdown in the memory repository and commit the correction."
    )
}

fn assert_invalid(result: Result<Memory, MemoroError>, relative_path: &str, reason: &str) {
    match result {
        Err(MemoroError::MemoryValidation(message)) => {
            assert_eq!(message, invalid(relative_path, reason));
        }
        Err(other) => panic!("expected a validation error, got {other:?}"),
        Ok(parsed) => panic!("expected a validation error, parsed {parsed:?}"),
    }
}

fn frontmatter_fields(text: &str) -> Vec<&str> {
    let frontmatter = text.split("---\n").nth(1).unwrap();
    frontmatter
        .lines()
        .map(|line| line.split(": ").next().unwrap())
        .collect()
}

#[test]
fn serialization_matches_the_python_reference_byte_for_byte() {
    assert_eq!(serialize_memory(&memory()).unwrap(), CANONICAL_DOCUMENT);
}

#[test]
fn serialized_frontmatter_omits_empty_tags_and_keeps_field_order() {
    let text = serialize_memory(&memory()).unwrap();

    let expected: Vec<&str> = FRONTMATTER_FIELDS
        .iter()
        .copied()
        .filter(|field| *field != "tags")
        .collect();
    assert_eq!(frontmatter_fields(&text), expected);
    assert!(!text.contains("tags:"));
    assert!(text.ends_with("First line\nSecond line\n"));
}

#[test]
fn serialized_frontmatter_includes_canonical_tags_in_order() {
    let memory = make_memory(None, "persona", None, &["alpha", "Zeta"]);
    let text = serialize_memory(&memory).unwrap();

    assert_eq!(frontmatter_fields(&text), FRONTMATTER_FIELDS.to_vec());
    assert!(text.contains("tags: [\"alpha\", \"Zeta\"]\n"));
}

#[test]
fn serialization_escapes_like_python_json_dumps() {
    let mut memory = memory();
    memory.title = "He said \"hi\" \\ back 中文".to_string();
    memory.tags = vec!["测试\"q'".to_string()];

    assert_eq!(
        serialize_memory(&memory).unwrap(),
        r#"---
id: "01ARZ3NDEKTSV4RRFFQ69G5FAV"
title: "He said \"hi\" \\ back 中文"
summary: "What the release notes cover."
tags: ["测试\"q'"]
created_at: "2026-08-22T01:02:03.000000Z"
updated_at: "2026-08-22T01:02:03.000000Z"
---

First line
Second line
"#
    );
}

#[test]
fn serialization_normalizes_the_body() {
    let mut memory = memory();
    memory.body = "First line\nSecond line\n".to_string();
    assert_eq!(serialize_memory(&memory).unwrap(), CANONICAL_DOCUMENT);

    memory.body = "tab\there".to_string();
    assert_eq!(
        serialize_memory(&memory).unwrap(),
        CANONICAL_DOCUMENT.replace("First line\nSecond line", "tab\there")
    );
}

#[test]
fn serialization_refuses_an_empty_body() {
    let mut memory = memory();
    memory.body = " \n".to_string();

    assert_eq!(serialize_memory(&memory).unwrap_err(), "body is empty");
}

#[test]
fn revision_matches_the_python_reference_and_is_stable() {
    assert_eq!(
        memory_revision(&memory()).unwrap(),
        "f0dba97f02a3d90fbe26e57e097d615501ae5a2419e9a23c6ebd7a338c61de2f"
    );
    assert_eq!(
        memory_revision(&memory()).unwrap(),
        memory_revision(&memory()).unwrap()
    );
}

#[test]
fn revision_covers_authoritative_summary_and_tags() {
    let mut other = memory();
    other.summary = "Different coverage.".to_string();
    assert_ne!(
        memory_revision(&other).unwrap(),
        memory_revision(&memory()).unwrap()
    );

    let mut tagged = memory();
    tagged.tags = vec!["alpha".to_string()];
    assert_ne!(
        memory_revision(&tagged).unwrap(),
        memory_revision(&memory()).unwrap()
    );
}

#[test]
fn memory_round_trip_uses_location_from_the_trusted_path() {
    let cases = [
        (format!("persona/{MEMORY_ID}.md"), "persona", None),
        (format!("playbooks/{MEMORY_ID}.md"), "playbook", None),
        (
            format!("projects/memoro/{MEMORY_ID}.md"),
            "project",
            Some("memoro"),
        ),
    ];
    for (relative_path, kind, project) in cases {
        let original = make_memory(Some(&relative_path), kind, project, &[]);
        assert_eq!(
            parse_memory(&serialize_memory(&original).unwrap(), &relative_path).unwrap(),
            original
        );
    }
}

#[test]
fn memory_round_trip_preserves_tags() {
    let original = make_memory(None, "persona", None, &["alpha", "Zeta"]);

    let parsed = parse_memory(
        &serialize_memory(&original).unwrap(),
        &original.relative_path,
    )
    .unwrap();

    assert_eq!(parsed, original);
    assert_eq!(parsed.tags, vec!["alpha".to_string(), "Zeta".to_string()]);
}

#[test]
fn parse_accepts_crlf_line_endings() {
    let text = serialize_memory(&memory()).unwrap().replace('\n', "\r\n");

    assert_eq!(
        parse_memory(&text, &format!("persona/{MEMORY_ID}.md")).unwrap(),
        memory()
    );
}

#[test]
fn parser_rejects_invalid_document_structure() {
    let path = format!("persona/{MEMORY_ID}.md");
    let text = serialize_memory(&memory()).unwrap();
    let cases = [
        (
            text.strip_prefix("---\n").unwrap().to_string(),
            "frontmatter must start on the first line",
        ),
        (
            text.replacen("\n---\n\n", "\n\n", 1),
            "frontmatter closing delimiter is missing",
        ),
        (
            text.replacen("\n---\n\n", "\n---\n", 1),
            "frontmatter must be followed by one blank line",
        ),
        (
            "---\n[]\n---\n\nBody\n".to_string(),
            "frontmatter must be a mapping",
        ),
    ];
    for (mutated, reason) in cases {
        assert_invalid(parse_memory(&mutated, &path), &path, reason);
    }
}

#[test]
fn parser_rejects_noncanonical_frontmatter_values() {
    let path = format!("persona/{MEMORY_ID}.md");
    let text = serialize_memory(&memory()).unwrap();
    let cases = [
        (
            format!("created_at: \"{CREATED_AT}\""),
            "created_at: 123".to_string(),
            "timestamps must be quoted strings",
        ),
        (
            "title: \"Release notes\"".to_string(),
            "title: \" Release notes \"".to_string(),
            "title is not normalized",
        ),
        (
            SUMMARY_LINE.to_string(),
            "summary: \" What the release notes cover. \"".to_string(),
            "summary is not normalized",
        ),
        (
            format!("updated_at: \"{CREATED_AT}\""),
            "updated_at: \"2026-08-22T01:02:02.000000Z\"".to_string(),
            "updated_at is earlier than created_at",
        ),
    ];
    for (old, new, reason) in cases {
        let mutated = text.replacen(&old, &new, 1);
        assert_invalid(parse_memory(&mutated, &path), &path, reason);
    }
}

#[test]
fn parser_rejects_noncanonical_tags() {
    let path = format!("persona/{MEMORY_ID}.md");
    let too_many = format!(
        "tags: [{}]",
        (0..=MAX_TAGS)
            .map(|index| format!("\"tag-{index}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let cases = [
        (
            "tags: [\"Zeta\", \"alpha\"]".to_string(),
            "tags are not canonical",
        ),
        (
            "tags: [\"alpha\", \"ALPHA\"]".to_string(),
            "tags are not canonical",
        ),
        ("tags: [\" alpha\"]".to_string(), "tags are not canonical"),
        (
            "tags: []".to_string(),
            "empty tags must omit the tags field",
        ),
        (
            "tags: \"alpha\"".to_string(),
            "tags must be a list of strings",
        ),
        ("tags: [1]".to_string(), "tags must be a list of strings"),
        (too_many, "tags exceed the maximum of 8"),
    ];
    for (tags_line, reason) in cases {
        let mutated = serialize_memory(&memory()).unwrap().replacen(
            SUMMARY_LINE,
            &format!("{SUMMARY_LINE}\n{tags_line}"),
            1,
        );
        assert_invalid(parse_memory(&mutated, &path), &path, reason);
    }
}

#[test]
fn parser_defaults_a_missing_tags_field_to_no_tags() {
    let parsed = parse_memory(
        &serialize_memory(&memory()).unwrap(),
        &format!("persona/{MEMORY_ID}.md"),
    )
    .unwrap();

    assert_eq!(parsed.tags, Vec::<String>::new());
}

#[test]
fn parser_rejects_noncanonical_memory_paths() {
    let cases = [
        (
            format!("projects/con/{MEMORY_ID}.md"),
            "project directory is invalid",
        ),
        (
            format!("projects/Memoro/{MEMORY_ID}.md"),
            "project directory is not normalized",
        ),
        (
            format!("archive/{MEMORY_ID}.md"),
            "path must be persona/<ULID>.md, projects/<slug>/<ULID>.md, or playbooks/<ULID>.md",
        ),
        (
            format!("global/{MEMORY_ID}.md"),
            "path must be persona/<ULID>.md, projects/<slug>/<ULID>.md, or playbooks/<ULID>.md",
        ),
        (
            "persona/not-a-ulid.md".to_string(),
            "filename is not a ULID",
        ),
        (
            "playbooks/not-a-ulid.md".to_string(),
            "filename is not a ULID",
        ),
    ];
    for (relative_path, reason) in cases {
        assert_invalid(
            parse_memory(&serialize_memory(&memory()).unwrap(), &relative_path),
            &relative_path,
            reason,
        );
    }
}

#[test]
fn frontmatter_rejects_missing_or_extra_fields() {
    let path = format!("persona/{MEMORY_ID}.md");
    let text = serialize_memory(&memory()).unwrap();
    let cases = [
        (
            text.replacen(&format!("{SUMMARY_LINE}\n"), "", 1),
            "missing fields: summary",
        ),
        (
            text.replacen(
                &format!("{SUMMARY_LINE}\n"),
                &format!("{SUMMARY_LINE}\nsource: \"codex\"\n"),
                1,
            ),
            "unsupported fields: source",
        ),
        (
            text.replacen(
                &format!("{SUMMARY_LINE}\n"),
                &format!("{SUMMARY_LINE}\nscope: \"global\"\n"),
                1,
            ),
            "unsupported fields: scope",
        ),
        (
            text.replacen(
                &format!("{SUMMARY_LINE}\n"),
                &format!("{SUMMARY_LINE}\nsource: \"codex\"\n"),
                1,
            )
            .replacen("title: \"Release notes\"\n", "", 1),
            "missing fields: title; unsupported fields: source",
        ),
    ];
    for (mutated, reason) in cases {
        assert_invalid(parse_memory(&mutated, &path), &path, reason);
    }
}

#[test]
fn frontmatter_rejects_duplicate_fields() {
    let path = format!("persona/{MEMORY_ID}.md");
    let text = serialize_memory(&memory()).unwrap().replacen(
        "title: \"Release notes\"\n",
        "title: \"Release notes\"\ntitle: \"Shadow title\"\n",
        1,
    );

    assert_invalid(
        parse_memory(&text, &path),
        &path,
        "frontmatter is not valid YAML",
    );
}

#[test]
fn frontmatter_id_must_match_the_filename() {
    let other_path = "persona/01ARZ3NDEKTSV4RRFFQ69G5FAW.md";

    assert_invalid(
        parse_memory(&serialize_memory(&memory()).unwrap(), other_path),
        other_path,
        "frontmatter id does not match the filename",
    );
}

#[test]
fn parser_rejects_malformed_yaml() {
    let path = format!("persona/{MEMORY_ID}.md");
    let text = serialize_memory(&memory()).unwrap().replacen(
        &format!("{SUMMARY_LINE}\n"),
        "summary: [\"unclosed\n",
        1,
    );

    assert_invalid(
        parse_memory(&text, &path),
        &path,
        "frontmatter is not valid YAML",
    );
}

#[test]
fn parser_rejects_non_string_field_names() {
    let path = format!("persona/{MEMORY_ID}.md");
    let text = serialize_memory(&memory()).unwrap().replacen(
        "title: \"Release notes\"\n",
        "title: \"Release notes\"\n123: x\n",
        1,
    );

    assert_invalid(
        parse_memory(&text, &path),
        &path,
        "frontmatter is not valid YAML",
    );
}

#[test]
fn parser_expands_merge_keys_and_rejects_merge_duplicates() {
    let path = format!("persona/{MEMORY_ID}.md");
    let merged = serialize_memory(&memory()).unwrap().replacen(
        &format!("{SUMMARY_LINE}\n"),
        "<<: &base {summary: \"What the release notes cover.\"}\n",
        1,
    );
    assert_eq!(
        parse_memory(&merged, &path).unwrap().summary,
        "What the release notes cover."
    );

    let duplicated = serialize_memory(&memory()).unwrap().replacen(
        &format!("{SUMMARY_LINE}\n"),
        "summary: \"What the release notes cover.\"\n<<: &base {title: \"Shadow\"}\n",
        1,
    );
    assert_invalid(
        parse_memory(&duplicated, &path),
        &path,
        "frontmatter is not valid YAML",
    );
}

#[test]
fn parser_rejects_unquoted_timestamps() {
    let path = format!("persona/{MEMORY_ID}.md");
    let cases = [
        format!("created_at: {CREATED_AT}"),
        "created_at: 2026-08-22".to_string(),
    ];
    for new in cases {
        let text = serialize_memory(&memory()).unwrap().replacen(
            &format!("created_at: \"{CREATED_AT}\""),
            &new,
            1,
        );
        assert_invalid(
            parse_memory(&text, &path),
            &path,
            "timestamps must be quoted strings",
        );
    }
}

#[test]
fn parser_rejects_yaml_typed_field_values() {
    let path = format!("persona/{MEMORY_ID}.md");
    let cases = [
        (
            "title: \"Release notes\"",
            "title: yes",
            "title must be a string",
        ),
        (
            "title: \"Release notes\"",
            "title: 123",
            "title must be a string",
        ),
        (
            "id: \"01ARZ3NDEKTSV4RRFFQ69G5FAV\"",
            "id: 123",
            "id is not a valid ULID",
        ),
        (
            SUMMARY_LINE,
            "summary: 2026-08-22",
            "summary must be a string",
        ),
    ];
    for (old, new, reason) in cases {
        let text = serialize_memory(&memory()).unwrap().replacen(old, new, 1);
        assert_invalid(parse_memory(&text, &path), &path, reason);
    }
}

#[test]
fn parser_rejects_deeply_nested_yaml_without_crashing() {
    let path = format!("persona/{MEMORY_ID}.md");
    let text = format!("---\n{}{}\n---\n\nBody\n", "[".repeat(600), "]".repeat(600));

    assert_invalid(
        parse_memory(&text, &path),
        &path,
        "frontmatter is not valid YAML",
    );
}
