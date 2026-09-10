//! Markdown serialization and strict parsing of committed memory documents.
//!
//! Mirrors `python/src/memoro/markdown.py`: [`serialize_memory`] emits the
//! frontmatter exactly like Python (`field: ` + `json.dumps(value,
//! ensure_ascii=False)`), [`memory_revision`] is the SHA-256 of the serialized
//! document, and [`parse_memory`] validates the document with the YAML
//! semantics of PyYAML's `SafeLoader` driven by `_UniqueSafeLoader` — plain
//! scalars are resolved with the YAML 1.1 implicit resolver regexes (so
//! unquoted timestamps, `yes`/`no` booleans, and integers are non-strings),
//! merge keys (`<<`) are flattened before every mapping is checked for
//! unique, string field names.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use saphyr_parser::{Event, Parser, ScalarStyle, Tag};
use sha2::{Digest, Sha256};

use crate::errors::MemoroError;
use crate::models::{
    normalize_body, normalize_project, normalize_summary, normalize_tags, normalize_title,
    parse_timestamp, validate_ulid, Memory,
};

/// Frontmatter fields in canonical serialization order; every field except
/// `tags` is required.
pub const FRONTMATTER_FIELDS: &[&str] =
    &["id", "title", "summary", "tags", "created_at", "updated_at"];

const REQUIRED_FIELDS: &[&str] = &["id", "title", "summary", "created_at", "updated_at"];

/// Serialize one memory into its canonical Markdown document.
///
/// Mirrors `serialize_memory`: frontmatter values use Python's
/// `json.dumps(value, ensure_ascii=False)` formatting and the body goes
/// through `normalize_body`, whose error propagates unchanged.
pub fn serialize_memory(memory: &Memory) -> Result<String, String> {
    let mut lines = vec!["---".to_string()];
    for field in FRONTMATTER_FIELDS {
        if *field == "tags" && memory.tags.is_empty() {
            continue;
        }
        let value = match *field {
            "id" => json_string(&memory.id),
            "title" => json_string(&memory.title),
            "summary" => json_string(&memory.summary),
            "tags" => format!(
                "[{}]",
                memory
                    .tags
                    .iter()
                    .map(|tag| json_string(tag))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "created_at" => json_string(&memory.created_at),
            "updated_at" => json_string(&memory.updated_at),
            _ => unreachable!("frontmatter fields are exhaustive"),
        };
        lines.push(format!("{field}: {value}"));
    }
    lines.push("---".to_string());
    lines.push(String::new());
    lines.push(normalize_body(&memory.body)?);
    Ok(lines.join("\n") + "\n")
}

/// Return the opaque revision of one canonical committed memory.
pub fn memory_revision(memory: &Memory) -> Result<String, String> {
    let text = serialize_memory(memory)?;
    let digest = Sha256::digest(text.as_bytes());
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Parse and validate one committed Markdown memory document.
///
/// Mirrors `parse_memory`: structure, YAML loading (`_UniqueSafeLoader`),
/// field presence, canonical values, and the trusted `relative_path` layout
/// are all checked, and every failure is a `MemoryValidationError` carrying
/// the Python message verbatim.
pub fn parse_memory(text: &str, relative_path: &str) -> Result<Memory, MemoroError> {
    let normalized_text = text.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized_text.starts_with("---\n") {
        return Err(invalid(
            relative_path,
            "frontmatter must start on the first line",
        ));
    }
    let after = &normalized_text[4..];
    let Some(position) = after.find("\n---\n") else {
        return Err(invalid(
            relative_path,
            "frontmatter closing delimiter is missing",
        ));
    };
    let frontmatter_text = &after[..position];
    let remainder = &after[position + 5..];
    if !remainder.starts_with('\n') {
        return Err(invalid(
            relative_path,
            "frontmatter must be followed by one blank line",
        ));
    }

    let raw = load_yaml(frontmatter_text)
        .map_err(|_| invalid(relative_path, "frontmatter is not valid YAML"))?;
    let Yaml::Map(pairs) = raw else {
        return Err(invalid(relative_path, "frontmatter must be a mapping"));
    };

    let mut missing: Vec<&str> = REQUIRED_FIELDS
        .iter()
        .copied()
        .filter(|field| map_get(&pairs, field).is_none())
        .collect();
    missing.sort_unstable();
    let mut extra: Vec<&str> = pairs
        .iter()
        .filter_map(|(key, _)| match key {
            Yaml::Str(name) if !FRONTMATTER_FIELDS.contains(&name.as_str()) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    extra.sort_unstable();
    extra.dedup();
    if !missing.is_empty() || !extra.is_empty() {
        let mut details: Vec<String> = Vec::new();
        if !missing.is_empty() {
            details.push(format!("missing fields: {}", missing.join(", ")));
        }
        if !extra.is_empty() {
            details.push(format!("unsupported fields: {}", extra.join(", ")));
        }
        return Err(invalid(relative_path, &details.join("; ")));
    }

    let mut body = &remainder[1..];
    if let Some(stripped) = body.strip_suffix('\n') {
        body = stripped;
    }

    let memory_id = match map_get(&pairs, "id") {
        Some(Yaml::Str(value)) => {
            validate_ulid(value).map_err(|message| invalid(relative_path, &message))?
        }
        _ => return Err(invalid(relative_path, "id is not a valid ULID")),
    };
    let (raw_title, title) = match map_get(&pairs, "title") {
        Some(Yaml::Str(value)) => (
            value.clone(),
            normalize_title(value).map_err(|message| invalid(relative_path, &message))?,
        ),
        _ => return Err(invalid(relative_path, "title must be a string")),
    };
    let (raw_summary, summary) = match map_get(&pairs, "summary") {
        Some(Yaml::Str(value)) => (
            value.clone(),
            normalize_summary(value).map_err(|message| invalid(relative_path, &message))?,
        ),
        _ => return Err(invalid(relative_path, "summary must be a string")),
    };
    let tags = parse_tags(&pairs, relative_path)?;
    let (created_at_text, created_time) =
        timestamp_string(map_get(&pairs, "created_at"), relative_path)?;
    let (updated_at_text, updated_time) =
        timestamp_string(map_get(&pairs, "updated_at"), relative_path)?;
    let normalized_body =
        normalize_body(body).map_err(|message| invalid(relative_path, &message))?;

    if title != raw_title {
        return Err(invalid(relative_path, "title is not normalized"));
    }
    if summary != raw_summary {
        return Err(invalid(relative_path, "summary is not normalized"));
    }
    if updated_time < created_time {
        return Err(invalid(
            relative_path,
            "updated_at is earlier than created_at",
        ));
    }

    let (kind, project, path_id) = location_and_id(relative_path)?;
    if path_id != memory_id {
        return Err(invalid(
            relative_path,
            "frontmatter id does not match the filename",
        ));
    }
    Ok(Memory {
        id: memory_id,
        title,
        summary,
        tags,
        created_at: created_at_text,
        updated_at: updated_at_text,
        body: normalized_body,
        kind,
        project,
        relative_path: relative_path.to_string(),
    })
}

fn parse_tags(pairs: &[(Yaml, Yaml)], relative_path: &str) -> Result<Vec<String>, MemoroError> {
    let Some(raw) = map_get(pairs, "tags") else {
        return Ok(Vec::new());
    };
    let Yaml::Seq(items) = raw else {
        return Err(invalid(relative_path, "tags must be a list of strings"));
    };
    let mut raw_tags: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let Yaml::Str(tag) = item else {
            return Err(invalid(relative_path, "tags must be a list of strings"));
        };
        raw_tags.push(tag.clone());
    }
    if raw_tags.is_empty() {
        return Err(invalid(
            relative_path,
            "empty tags must omit the tags field",
        ));
    }
    let tags =
        normalize_tags(raw_tags.clone()).map_err(|message| invalid(relative_path, &message))?;
    if tags != raw_tags {
        return Err(invalid(relative_path, "tags are not canonical"));
    }
    Ok(tags)
}

fn timestamp_string(
    value: Option<&Yaml>,
    relative_path: &str,
) -> Result<(String, DateTime<Utc>), MemoroError> {
    match value {
        Some(Yaml::Str(text)) => {
            let parsed =
                parse_timestamp(text).map_err(|message| invalid(relative_path, &message))?;
            Ok((text.clone(), parsed))
        }
        _ => Err(invalid(relative_path, "timestamps must be quoted strings")),
    }
}

fn location_and_id(relative_path: &str) -> Result<(String, Option<String>, String), MemoroError> {
    let parts = posix_parts(relative_path);
    let name = parts.last().map(String::as_str).unwrap_or_default();
    let (stem, suffix) = split_suffix(name);
    let (kind, project): (&str, Option<String>) =
        if parts.len() == 2 && parts[0] == "persona" && suffix == ".md" {
            ("persona", None)
        } else if parts.len() == 2 && parts[0] == "playbooks" && suffix == ".md" {
            ("playbook", None)
        } else if parts.len() == 3 && parts[0] == "projects" && suffix == ".md" {
            let normalized = normalize_project(&parts[1])
                .map_err(|_| invalid(relative_path, "project directory is invalid"))?;
            if normalized != parts[1] {
                return Err(invalid(
                    relative_path,
                    "project directory is not normalized",
                ));
            }
            ("project", Some(normalized))
        } else {
            return Err(invalid(
                relative_path,
                "path must be persona/<ULID>.md, projects/<slug>/<ULID>.md, or playbooks/<ULID>.md",
            ));
        };
    validate_ulid(&stem).map_err(|_| invalid(relative_path, "filename is not a ULID"))?;
    Ok((kind.to_string(), project, stem))
}

fn invalid(relative_path: &str, reason: &str) -> MemoroError {
    MemoroError::MemoryValidation(format!(
        "Committed memory {} is invalid: {}. Repair the Markdown in the memory repository and commit the correction.",
        py_repr(relative_path),
        reason
    ))
}

/// Render a string like Python's `json.dumps(value, ensure_ascii=False)`.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

/// Render a string like Python's `repr` (single quotes, escaped controls).
fn py_repr(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 || character as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push('\'');
    out
}

/// Split a path the way `PurePosixPath` does: absolute paths keep a root
/// component, empty and `.` components are dropped, `..` is kept.
fn posix_parts(path: &str) -> Vec<String> {
    let (root, rest) = if path.len() > 2 && path.starts_with("//") && !path.starts_with("///") {
        ("//", &path[2..])
    } else if path.starts_with('/') {
        ("/", path.trim_start_matches('/'))
    } else {
        ("", path)
    };
    let mut parts: Vec<String> = Vec::new();
    for segment in rest.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        parts.push(segment.to_string());
    }
    if !root.is_empty() {
        parts.insert(0, root.to_string());
    }
    parts
}

/// Split a filename into `(stem, suffix)` the way `PurePosixPath` does.
fn split_suffix(name: &str) -> (String, String) {
    if let Some(position) = name.rfind('.') {
        if 0 < position && position < name.len() - 1 {
            return (name[..position].to_string(), name[position..].to_string());
        }
    }
    (name.to_string(), String::new())
}

// ---------------------------------------------------------------------------
// YAML loading with the semantics of PyYAML's SafeLoader + _UniqueSafeLoader.
// ---------------------------------------------------------------------------

/// A resolved YAML node. Non-string scalars only need their kind: the parser
/// rejects them wherever a string is required, with the Python message.
#[derive(Debug, Clone, PartialEq)]
enum Yaml {
    Null,
    Bool,
    Int,
    Float,
    Timestamp,
    Binary,
    Merge,
    Equals,
    Str(String),
    Seq(Vec<Yaml>),
    Map(Vec<(Yaml, Yaml)>),
}

fn map_get<'a>(pairs: &'a [(Yaml, Yaml)], key: &str) -> Option<&'a Yaml> {
    pairs
        .iter()
        .find(|(candidate, _)| matches!(candidate, Yaml::Str(name) if name == key))
        .map(|(_, value)| value)
}

#[derive(Default)]
struct UniqueSafeLoader {
    anchors: HashMap<usize, Yaml>,
}

/// Load exactly one YAML document; an empty stream is `None`, like
/// `yaml.load`. Any structural problem is an error string (the caller maps
/// every failure to the same Python message).
fn load_yaml(text: &str) -> Result<Yaml, String> {
    let mut events: Vec<Event> = Vec::new();
    for result in Parser::new_from_str(text) {
        let (event, _) = result.map_err(|_| "the YAML stream could not be scanned".to_string())?;
        events.push(event);
    }
    UniqueSafeLoader::default().load_document(&events)
}

impl UniqueSafeLoader {
    fn load_document(&mut self, events: &[Event]) -> Result<Yaml, String> {
        if !matches!(events.first(), Some(Event::StreamStart)) {
            return Err("the YAML stream did not start".to_string());
        }
        let mut index = 1;
        if matches!(events.get(index), None | Some(Event::StreamEnd)) {
            return Ok(Yaml::Null);
        }
        if matches!(events.get(index), Some(Event::DocumentStart(_))) {
            index += 1;
        }
        let root = self.node(events, &mut index, 0)?;
        loop {
            match events.get(index) {
                Some(Event::DocumentEnd) => index += 1,
                Some(Event::StreamEnd) | None => return Ok(root),
                Some(Event::DocumentStart(_)) => {
                    return Err("expected a single document in the stream".to_string())
                }
                _ => return Err("unexpected trailing YAML content".to_string()),
            }
        }
    }

    fn node(&mut self, events: &[Event], index: &mut usize, depth: usize) -> Result<Yaml, String> {
        if depth > 500 {
            return Err("YAML nesting is too deep".to_string());
        }
        let (anchor, value) = match events.get(*index) {
            None => return Err("unexpected end of the YAML stream".to_string()),
            Some(Event::Alias(id)) => {
                *index += 1;
                return self
                    .anchors
                    .get(id)
                    .cloned()
                    .ok_or_else(|| "found undefined alias".to_string());
            }
            Some(Event::Scalar(text, style, anchor, tag)) => {
                *index += 1;
                (*anchor, self.scalar(text, *style, tag.as_deref())?)
            }
            Some(Event::SequenceStart(anchor, tag)) => {
                *index += 1;
                self.collection_tag(tag.as_deref(), "seq")?;
                let mut items: Vec<Yaml> = Vec::new();
                while !matches!(events.get(*index), Some(Event::SequenceEnd)) {
                    if events.get(*index).is_none() {
                        return Err("the YAML sequence was not terminated".to_string());
                    }
                    items.push(self.node(events, index, depth + 1)?);
                }
                *index += 1;
                (*anchor, Yaml::Seq(items))
            }
            Some(Event::MappingStart(anchor, tag)) => {
                *index += 1;
                self.collection_tag(tag.as_deref(), "map")?;
                let mut pairs: Vec<(Yaml, Yaml)> = Vec::new();
                while !matches!(events.get(*index), Some(Event::MappingEnd)) {
                    if events.get(*index).is_none() {
                        return Err("the YAML mapping was not terminated".to_string());
                    }
                    let key = self.node(events, index, depth + 1)?;
                    let value = self.node(events, index, depth + 1)?;
                    pairs.push((key, value));
                }
                *index += 1;
                (*anchor, self.unique_mapping(pairs)?)
            }
            _ => return Err("unexpected YAML event".to_string()),
        };
        if anchor != 0 {
            self.anchors.insert(anchor, value.clone());
        }
        Ok(value)
    }

    fn scalar(&self, text: &str, style: ScalarStyle, tag: Option<&Tag>) -> Result<Yaml, String> {
        if let Some(tag) = tag {
            return match yaml_core_suffix(tag) {
                Some("str") => Ok(Yaml::Str(text.to_string())),
                Some("int") => Ok(Yaml::Int),
                Some("float") => Ok(Yaml::Float),
                Some("bool") => Ok(Yaml::Bool),
                Some("null") => Ok(Yaml::Null),
                Some("timestamp") => Ok(Yaml::Timestamp),
                Some("binary") => Ok(Yaml::Binary),
                Some("merge") => Ok(Yaml::Merge),
                _ => Err("could not determine a constructor for the tag".to_string()),
            };
        }
        if style != ScalarStyle::Plain {
            return Ok(Yaml::Str(text.to_string()));
        }
        Ok(resolve_implicit(text))
    }

    fn collection_tag(&self, tag: Option<&Tag>, kind: &str) -> Result<(), String> {
        match tag.and_then(yaml_core_suffix) {
            None => Ok(()),
            Some(suffix) if suffix == kind => Ok(()),
            Some(_) => Err("could not determine a constructor for the tag".to_string()),
        }
    }

    /// Construct one mapping the way `_construct_unique_mapping` does:
    /// flatten merge keys first, then require string, unique field names.
    fn unique_mapping(&self, pairs: Vec<(Yaml, Yaml)>) -> Result<Yaml, String> {
        let mut merged: Vec<(Yaml, Yaml)> = Vec::new();
        let mut explicit: Vec<(Yaml, Yaml)> = Vec::new();
        for (key, value) in pairs {
            match key {
                Yaml::Merge => match value {
                    Yaml::Map(inner) => merged.extend(inner),
                    Yaml::Seq(items) => {
                        for item in items {
                            match item {
                                Yaml::Map(inner) => merged.extend(inner),
                                other => {
                                    return Err(format!(
                                        "expected a mapping for merging, but found {other:?}"
                                    ))
                                }
                            }
                        }
                    }
                    other => {
                        return Err(format!(
                        "expected a mapping or list of mappings for merging, but found {other:?}"
                    ))
                    }
                },
                Yaml::Equals => explicit.push((Yaml::Str("=".to_string()), value)),
                key => explicit.push((key, value)),
            }
        }
        merged.extend(explicit);
        let mut seen: HashSet<String> = HashSet::new();
        for (key, _) in &merged {
            match key {
                Yaml::Str(name) => {
                    if !seen.insert(name.clone()) {
                        return Err(format!("duplicate field {}", py_repr(name)));
                    }
                }
                other => {
                    return Err(format!(
                        "frontmatter field names must be strings, but found {other:?}"
                    ))
                }
            }
        }
        Ok(Yaml::Map(merged))
    }
}

/// Return the `tag:yaml.org,2002` suffix for a tag, if it is one.
fn yaml_core_suffix(tag: &Tag) -> Option<&str> {
    if tag.handle == "tag:yaml.org,2002:" {
        Some(tag.suffix.as_str())
    } else if tag.handle.is_empty() {
        tag.suffix.strip_prefix("tag:yaml.org,2002:")
    } else {
        None
    }
}

struct ImplicitRegexes {
    boolean: Regex,
    float: Regex,
    integer: Regex,
    null: Regex,
    timestamp: Regex,
}

/// Resolve a plain scalar with PyYAML's YAML 1.1 implicit resolvers; the
/// merge (`<<`) and value (`=`) tags are exact-string matches.
fn resolve_implicit(value: &str) -> Yaml {
    static REGEXES: OnceLock<ImplicitRegexes> = OnceLock::new();
    let regexes = REGEXES.get_or_init(|| ImplicitRegexes {
        boolean: Regex::new(
            r"^(?:yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF)$",
        )
        .expect("compiled boolean regex"),
        float: Regex::new(
            r"^(?:[-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?|\.[0-9_]+(?:[eE][-+][0-9]+)?|[-+]?\.(?:inf|Inf|INF)|\.(?:nan|NaN|NAN))$",
        )
        .expect("compiled float regex"),
        integer: Regex::new(
            r"^(?:[-+]?0b[0-1_]+|[-+]?0[0-7_]+|[-+]?(?:0|[1-9][0-9_]*)|[-+]?0x[0-9a-fA-F_]+|[-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+)$",
        )
        .expect("compiled integer regex"),
        null: Regex::new(r"^(?:~|null|Null|NULL|)$").expect("compiled null regex"),
        timestamp: Regex::new(
            r"^(?:[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]|[0-9][0-9][0-9][0-9]-[0-9][0-9]?-[0-9][0-9]?(?:[Tt]|[ \t]+)[0-9][0-9]?:[0-9][0-9]:[0-9][0-9](?:\.(?:[0-9]*)?)?(?:[ \t]*(?:Z|[-+][0-9][0-9]?(?::[0-9][0-9])?))?)$",
        )
        .expect("compiled timestamp regex"),
    });
    if regexes.boolean.is_match(value) {
        return Yaml::Bool;
    }
    if regexes.float.is_match(value) {
        return Yaml::Float;
    }
    if regexes.integer.is_match(value) {
        return Yaml::Int;
    }
    if value == "<<" {
        return Yaml::Merge;
    }
    if regexes.null.is_match(value) {
        return Yaml::Null;
    }
    if regexes.timestamp.is_match(value) {
        return Yaml::Timestamp;
    }
    if value == "=" {
        return Yaml::Equals;
    }
    Yaml::Str(value.to_string())
}
