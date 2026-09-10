//! Core memory types, limits, and pure validation helpers.
//!
//! Mirrors `python/src/memoro/models.py`; the Unicode behavior (NFKC, full
//! case folding, Python's whitespace set) is backed by generated tables at
//! the bottom of this file.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use chrono::{DateTime, Datelike, Duration, SecondsFormat, Timelike, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

pub const MAX_TITLE_LENGTH: usize = 120;
pub const MAX_SUMMARY_LENGTH: usize = 300;
pub const MAX_BODY_LENGTH: usize = 20_000;
pub const MAX_PROJECT_LENGTH: usize = 64;
pub const MAX_TAG_LENGTH: usize = 32;
pub const MAX_TAGS: usize = 8;

pub const MEMORY_KINDS: &[&str] = &["persona", "project", "playbook"];

const ULID_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "aux", "con", "nul", "prn", //
    "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", //
    "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub body: String,
    pub kind: String,
    pub project: Option<String>,
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub commit: Option<String>,
    pub memories: Vec<Memory>,
}

impl MemorySnapshot {
    pub fn by_id(&self) -> HashMap<String, &Memory> {
        self.memories
            .iter()
            .map(|memory| (memory.id.clone(), memory))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchEdit {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutationReceipt {
    pub memory: Memory,
    pub operation: String,
    pub changed: bool,
    pub previous_memory: Option<Memory>,
    pub previous_commit: Option<String>,
    pub commit: String,
}

pub fn normalize_title(value: &str) -> Result<String, String> {
    let folded = fold_whitespace(&nfkc(value));
    let normalized = folded.trim_matches(is_python_space);
    if normalized.is_empty() {
        return Err("title is empty".to_string());
    }
    if normalized.chars().any(char::is_control) {
        return Err("title contains a control character".to_string());
    }
    if normalized.chars().count() > MAX_TITLE_LENGTH {
        return Err("title is too long".to_string());
    }
    Ok(normalized.to_string())
}

pub fn title_key(value: &str) -> Result<String, String> {
    Ok(casefold(&normalize_title(value)?))
}

pub fn normalize_summary(value: &str) -> Result<String, String> {
    let folded = fold_whitespace(&nfkc(value));
    let normalized = folded.trim_matches(is_python_space);
    if normalized.is_empty() {
        return Err("summary is empty".to_string());
    }
    if normalized.chars().any(char::is_control) {
        return Err("summary contains a control character".to_string());
    }
    if normalized.chars().count() > MAX_SUMMARY_LENGTH {
        return Err("summary is too long".to_string());
    }
    Ok(normalized.to_string())
}

pub fn normalize_body(value: &str) -> Result<String, String> {
    let joined = value.replace("\r\n", "\n").replace('\r', "\n");
    let normalized = joined.trim_matches(|c| c == '\n');
    if normalized.chars().all(is_python_space) {
        return Err("body is empty".to_string());
    }
    if normalized
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("body contains a control character".to_string());
    }
    if normalized.chars().count() > MAX_BODY_LENGTH {
        return Err("body is too long".to_string());
    }
    Ok(normalized.to_string())
}

pub fn normalize_project(value: &str) -> Result<String, String> {
    let normalized = nfkc(value).trim_matches(is_python_space).to_lowercase();
    if normalized.is_empty() {
        return Err("project is empty".to_string());
    }
    if normalized.chars().count() > MAX_PROJECT_LENGTH {
        return Err("project is too long".to_string());
    }
    if normalized == "."
        || normalized.contains("..")
        || normalized.contains('/')
        || normalized.contains('\\')
    {
        return Err("project contains path traversal".to_string());
    }
    if !normalized
        .chars()
        .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '.' | '_' | '-'))
    {
        return Err("project contains unsupported characters".to_string());
    }
    if normalized.ends_with('.')
        || WINDOWS_RESERVED_NAMES.contains(&normalized.split('.').next().unwrap_or(""))
    {
        return Err("project is not a portable directory name".to_string());
    }
    Ok(normalized)
}

pub fn normalize_kind(value: &str) -> Result<String, String> {
    let normalized = value.trim_matches(is_python_space).to_lowercase();
    if !MEMORY_KINDS.contains(&normalized.as_str()) {
        return Err("kind must be persona, project, or playbook".to_string());
    }
    Ok(normalized)
}

pub fn validate_location(
    kind: &str,
    project: Option<&str>,
) -> Result<(String, Option<String>), String> {
    let normalized_kind = normalize_kind(kind)?;
    if normalized_kind == "project" {
        if let Some(slug) = project {
            return Ok((normalized_kind, Some(normalize_project(slug)?)));
        }
        return Err("project kind requires a project slug".to_string());
    }
    if project.is_some() {
        return Err(format!(
            "{normalized_kind} kind does not take a project slug"
        ));
    }
    Ok((normalized_kind, None))
}

pub fn location_label(kind: &str, project: Option<&str>) -> Result<String, String> {
    let (normalized_kind, normalized_project) = validate_location(kind, project)?;
    if normalized_kind == "project" {
        return Ok(format!(
            "project/{}",
            normalized_project.as_deref().unwrap_or("")
        ));
    }
    Ok(normalized_kind)
}

pub fn normalize_tags(value: Vec<String>) -> Result<Vec<String>, String> {
    let mut normalized: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for item in value {
        let tag = nfkc(&item).trim_matches(is_python_space).to_string();
        if tag.is_empty() {
            return Err("tags contain an empty value".to_string());
        }
        if tag.chars().count() > MAX_TAG_LENGTH {
            return Err("tags contain a value that is too long".to_string());
        }
        if tag.chars().any(char::is_control) {
            return Err("tags contain a control character".to_string());
        }
        if seen.insert(casefold(&tag)) {
            normalized.push(tag);
        }
    }
    if normalized.len() > MAX_TAGS {
        return Err(format!("tags exceed the maximum of {MAX_TAGS}"));
    }
    normalized.sort_by_key(|tag| casefold(tag));
    Ok(normalized)
}

pub fn memory_path(memory_id: &str, kind: &str, project: Option<&str>) -> Result<String, String> {
    let id = validate_ulid(memory_id)?;
    let (normalized_kind, normalized_project) = validate_location(kind, project)?;
    match normalized_kind.as_str() {
        "persona" => Ok(format!("persona/{id}.md")),
        "playbook" => Ok(format!("playbooks/{id}.md")),
        _ => Ok(format!(
            "projects/{}/{id}.md",
            normalized_project.as_deref().unwrap_or("")
        )),
    }
}

pub fn validate_ulid(value: &str) -> Result<String, String> {
    let mut chars = value.chars();
    let first = chars.next();
    let valid = value.chars().count() == 26
        && matches!(first, Some(c) if matches!(c, '0'..='7'))
        && chars.all(
            |c| matches!(c, '0'..='9' | 'A'..='H' | 'J' | 'K' | 'M' | 'N' | 'P'..='T' | 'V'..='Z'),
        );
    if valid {
        Ok(value.to_string())
    } else {
        Err("id is not a valid ULID".to_string())
    }
}

pub fn validate_revision(value: &str) -> Result<String, String> {
    let valid =
        value.chars().count() == 64 && value.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
    if valid {
        Ok(value.to_string())
    } else {
        Err("revision is not a lowercase SHA-256 digest".to_string())
    }
}

pub fn new_ulid(now: Option<DateTime<Utc>>) -> String {
    let instant = now.unwrap_or_else(Utc::now);
    let timestamp_ms = instant.timestamp_millis();
    if !(0..(1u64 << 48) as i64).contains(&timestamp_ms) {
        panic!("timestamp cannot be encoded as a ULID");
    }
    let mut random = [0u8; 10];
    fill_random_bytes(&mut random);
    let mut value: u128 = (timestamp_ms as u128) << 80;
    for (index, byte) in random.iter().enumerate() {
        value |= u128::from(*byte) << (8 * (9 - index));
    }
    let mut chars = ['0'; 26];
    for index in (0..26).rev() {
        chars[index] = ULID_ALPHABET[(value & 31) as usize] as char;
        value >>= 5;
    }
    chars.iter().collect()
}

#[cfg(unix)]
fn fill_random_bytes(bytes: &mut [u8]) {
    use std::io::Read;

    let mut source = std::fs::File::open("/dev/urandom").expect("unable to open OS random source");
    source
        .read_exact(bytes)
        .expect("unable to read OS random source");
}

#[cfg(windows)]
fn fill_random_bytes(bytes: &mut [u8]) {
    #[link(name = "bcrypt")]
    unsafe extern "system" {
        #[link_name = "BCryptGenRandom"]
        fn bcrypt_gen_random(
            algorithm: *mut std::ffi::c_void,
            buffer: *mut u8,
            length: u32,
            flags: u32,
        ) -> i32;
    }

    let status = unsafe {
        bcrypt_gen_random(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            0x0000_0002,
        )
    };
    if status != 0 {
        panic!("unable to read OS random source");
    }
}

#[cfg(not(any(unix, windows)))]
fn fill_random_bytes(_bytes: &mut [u8]) {
    panic!("ULID generation is unsupported on this platform");
}

pub fn utc_now() -> DateTime<Utc> {
    Utc::now()
}

pub fn format_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, String> {
    static TIMESTAMP_RE: OnceLock<Regex> = OnceLock::new();
    let timestamp_re = TIMESTAMP_RE.get_or_init(|| {
        Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?(?:Z|[+-]\d{2}:\d{2})$")
            .expect("compiled timestamp regex")
    });
    if !timestamp_re.is_match(value) {
        return Err("timestamp must use RFC 3339 date-time syntax".to_string());
    }
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|error| error.to_string())?;
    // Python's `datetime` rejects leap seconds and year 0; chrono models them.
    if parsed.second() == 59 && parsed.nanosecond() >= 1_000_000_000 {
        return Err("second must be in 0..59".to_string());
    }
    if parsed.year() == 0 {
        return Err("year 0 is out of range".to_string());
    }
    Ok(parsed.with_timezone(&Utc))
}

pub fn next_update_time(now: DateTime<Utc>, previous: &str) -> DateTime<Utc> {
    let previous_time = parse_timestamp(previous).unwrap_or_else(|message| panic!("{message}"));
    if now <= previous_time {
        previous_time + Duration::microseconds(1)
    } else {
        now
    }
}

// ---------------------------------------------------------------------------
// Unicode helpers (semantics of Python's `unicodedata`, `str.casefold`,
// `re.\s`, and `str.strip`), backed by generated tables below.
// ---------------------------------------------------------------------------

/// The characters Python treats as whitespace for `\s`, `str.isspace()`, and
/// `str.strip()` — Unicode `White_Space` plus U+001C..U+001F.
fn is_python_space(c: char) -> bool {
    matches!(c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r'
        | '\u{1c}'..='\u{1f}'
        | ' '
        | '\u{85}' | '\u{a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
}

/// Replaces every run of Python whitespace with a single space (`\s+` → `" "`).
fn fold_whitespace(value: &str) -> String {
    let mut folded = String::with_capacity(value.len());
    let mut in_run = false;
    for c in value.chars() {
        if is_python_space(c) {
            if !in_run {
                folded.push(' ');
                in_run = true;
            }
        } else {
            folded.push(c);
            in_run = false;
        }
    }
    folded
}

/// Full case folding (`str.casefold()`): a per-character mapping, no context.
fn casefold(value: &str) -> String {
    let mut folded = String::with_capacity(value.len());
    for c in value.chars() {
        match CASEFOLD_TABLE.binary_search_by_key(&(c as u32), |entry| entry.0) {
            Ok(index) => folded.push_str(CASEFOLD_TABLE[index].1),
            Err(_) => folded.push(c),
        }
    }
    folded
}

fn combining_class(cp: u32) -> u8 {
    match CCC_TABLE.binary_search_by_key(&cp, |entry| entry.0) {
        Ok(index) => CCC_TABLE[index].1,
        Err(_) => 0,
    }
}

fn decompose_char(c: char, out: &mut String) {
    let cp = c as u32;
    if (0xAC00..=0xD7A3).contains(&cp) {
        // Hangul syllables decompose algorithmically.
        let s = cp - 0xAC00;
        out.push(char::from_u32(0x1100 + s / 588).expect("leading jamo"));
        out.push(char::from_u32(0x1161 + (s % 588) / 28).expect("vowel jamo"));
        let t = s % 28;
        if t != 0 {
            out.push(char::from_u32(0x11A7 + t).expect("trailing jamo"));
        }
        return;
    }
    match NFKD_TABLE.binary_search_by_key(&cp, |entry| entry.0) {
        Ok(index) => out.push_str(NFKD_TABLE[index].1),
        Err(_) => out.push(c),
    }
}

fn compose_pair(starter: u32, c: u32) -> Option<u32> {
    // Hangul composition precedes the table lookup.
    if (0x1100..=0x1112).contains(&starter) && (0x1161..=0x1175).contains(&c) {
        return Some(0xAC00 + (starter - 0x1100) * 588 + (c - 0x1161) * 28);
    }
    if (0xAC00..=0xD7A3).contains(&starter)
        && (0x11A8..=0x11C2).contains(&c)
        && (starter - 0xAC00).is_multiple_of(28)
    {
        return Some(starter + (c - 0x11A7));
    }
    match COMPOSE_TABLE.binary_search_by(|entry| entry.0.cmp(&starter).then(entry.1.cmp(&c))) {
        Ok(index) => Some(COMPOSE_TABLE[index].2),
        Err(_) => None,
    }
}

/// NFKC normalization: full compatibility decomposition, canonical
/// reordering, then canonical composition (Unicode TR15).
fn nfkc(input: &str) -> String {
    let mut decomposed_text = String::with_capacity(input.len());
    for c in input.chars() {
        decompose_char(c, &mut decomposed_text);
    }
    let mut decomposed: Vec<(u32, u8)> = decomposed_text
        .chars()
        .map(|c| {
            let cp = c as u32;
            (cp, combining_class(cp))
        })
        .collect();

    // Canonical ordering: stable insertion sort of combining marks.
    for i in 1..decomposed.len() {
        let mut j = i;
        while j > 0 && decomposed[j].1 != 0 && decomposed[j - 1].1 > decomposed[j].1 {
            decomposed.swap(j - 1, j);
            j -= 1;
        }
    }

    let mut composed: Vec<(u32, u8)> = Vec::with_capacity(decomposed.len());
    let mut last_starter: Option<usize> = None;
    let mut last_cc: u8 = 0;
    for &(cp, cc) in &decomposed {
        let mut merged = false;
        if let Some(starter) = last_starter {
            if last_cc == 0 || last_cc < cc {
                if let Some(merged_cp) = compose_pair(composed[starter].0, cp) {
                    composed[starter].0 = merged_cp;
                    merged = true;
                }
            }
        }
        if !merged {
            if cc == 0 {
                last_starter = Some(composed.len());
            }
            composed.push((cp, cc));
            last_cc = cc;
        }
    }
    composed
        .iter()
        .map(|&(cp, _)| char::from_u32(cp).expect("valid scalar value"))
        .collect()
}

// ---------------------------------------------------------------------------
// Generated Unicode tables (from Python 3.13's `unicodedata`, the semantic
// reference). Do not edit by hand: regenerate from the reference interpreter.
// NFKD decompositions are fully expanded; Hangul syllables are excluded and
// handled algorithmically. Tables are sorted for binary search.
// ---------------------------------------------------------------------------

/// Full NFKD decomposition per code point (identity mappings omitted).
#[rustfmt::skip]
static NFKD_TABLE: &[(u32, &str)] = &[
    (0xA0, " "), (0xA8, " \u{308}"), (0xAA, "a"), (0xAF, " \u{304}"), (0xB2, "2"), (0xB3, "3"),
    (0xB4, " \u{301}"), (0xB5, "\u{3bc}"), (0xB8, " \u{327}"), (0xB9, "1"), (0xBA, "o"),
    (0xBC, "1\u{2044}4"), (0xBD, "1\u{2044}2"), (0xBE, "3\u{2044}4"), (0xC0, "A\u{300}"),
    (0xC1, "A\u{301}"), (0xC2, "A\u{302}"), (0xC3, "A\u{303}"), (0xC4, "A\u{308}"),
    (0xC5, "A\u{30a}"), (0xC7, "C\u{327}"), (0xC8, "E\u{300}"), (0xC9, "E\u{301}"),
    (0xCA, "E\u{302}"), (0xCB, "E\u{308}"), (0xCC, "I\u{300}"), (0xCD, "I\u{301}"),
    (0xCE, "I\u{302}"), (0xCF, "I\u{308}"), (0xD1, "N\u{303}"), (0xD2, "O\u{300}"),
    (0xD3, "O\u{301}"), (0xD4, "O\u{302}"), (0xD5, "O\u{303}"), (0xD6, "O\u{308}"),
    (0xD9, "U\u{300}"), (0xDA, "U\u{301}"), (0xDB, "U\u{302}"), (0xDC, "U\u{308}"),
    (0xDD, "Y\u{301}"), (0xE0, "a\u{300}"), (0xE1, "a\u{301}"), (0xE2, "a\u{302}"),
    (0xE3, "a\u{303}"), (0xE4, "a\u{308}"), (0xE5, "a\u{30a}"), (0xE7, "c\u{327}"),
    (0xE8, "e\u{300}"), (0xE9, "e\u{301}"), (0xEA, "e\u{302}"), (0xEB, "e\u{308}"),
    (0xEC, "i\u{300}"), (0xED, "i\u{301}"), (0xEE, "i\u{302}"), (0xEF, "i\u{308}"),
    (0xF1, "n\u{303}"), (0xF2, "o\u{300}"), (0xF3, "o\u{301}"), (0xF4, "o\u{302}"),
    (0xF5, "o\u{303}"), (0xF6, "o\u{308}"), (0xF9, "u\u{300}"), (0xFA, "u\u{301}"),
    (0xFB, "u\u{302}"), (0xFC, "u\u{308}"), (0xFD, "y\u{301}"), (0xFF, "y\u{308}"),
    (0x100, "A\u{304}"), (0x101, "a\u{304}"), (0x102, "A\u{306}"), (0x103, "a\u{306}"),
    (0x104, "A\u{328}"), (0x105, "a\u{328}"), (0x106, "C\u{301}"), (0x107, "c\u{301}"),
    (0x108, "C\u{302}"), (0x109, "c\u{302}"), (0x10A, "C\u{307}"), (0x10B, "c\u{307}"),
    (0x10C, "C\u{30c}"), (0x10D, "c\u{30c}"), (0x10E, "D\u{30c}"), (0x10F, "d\u{30c}"),
    (0x112, "E\u{304}"), (0x113, "e\u{304}"), (0x114, "E\u{306}"), (0x115, "e\u{306}"),
    (0x116, "E\u{307}"), (0x117, "e\u{307}"), (0x118, "E\u{328}"), (0x119, "e\u{328}"),
    (0x11A, "E\u{30c}"), (0x11B, "e\u{30c}"), (0x11C, "G\u{302}"), (0x11D, "g\u{302}"),
    (0x11E, "G\u{306}"), (0x11F, "g\u{306}"), (0x120, "G\u{307}"), (0x121, "g\u{307}"),
    (0x122, "G\u{327}"), (0x123, "g\u{327}"), (0x124, "H\u{302}"), (0x125, "h\u{302}"),
    (0x128, "I\u{303}"), (0x129, "i\u{303}"), (0x12A, "I\u{304}"), (0x12B, "i\u{304}"),
    (0x12C, "I\u{306}"), (0x12D, "i\u{306}"), (0x12E, "I\u{328}"), (0x12F, "i\u{328}"),
    (0x130, "I\u{307}"), (0x132, "IJ"), (0x133, "ij"), (0x134, "J\u{302}"),
    (0x135, "j\u{302}"), (0x136, "K\u{327}"), (0x137, "k\u{327}"), (0x139, "L\u{301}"),
    (0x13A, "l\u{301}"), (0x13B, "L\u{327}"), (0x13C, "l\u{327}"), (0x13D, "L\u{30c}"),
    (0x13E, "l\u{30c}"), (0x13F, "L\u{b7}"), (0x140, "l\u{b7}"), (0x143, "N\u{301}"),
    (0x144, "n\u{301}"), (0x145, "N\u{327}"), (0x146, "n\u{327}"), (0x147, "N\u{30c}"),
    (0x148, "n\u{30c}"), (0x149, "\u{2bc}n"), (0x14C, "O\u{304}"), (0x14D, "o\u{304}"),
    (0x14E, "O\u{306}"), (0x14F, "o\u{306}"), (0x150, "O\u{30b}"), (0x151, "o\u{30b}"),
    (0x154, "R\u{301}"), (0x155, "r\u{301}"), (0x156, "R\u{327}"), (0x157, "r\u{327}"),
    (0x158, "R\u{30c}"), (0x159, "r\u{30c}"), (0x15A, "S\u{301}"), (0x15B, "s\u{301}"),
    (0x15C, "S\u{302}"), (0x15D, "s\u{302}"), (0x15E, "S\u{327}"), (0x15F, "s\u{327}"),
    (0x160, "S\u{30c}"), (0x161, "s\u{30c}"), (0x162, "T\u{327}"), (0x163, "t\u{327}"),
    (0x164, "T\u{30c}"), (0x165, "t\u{30c}"), (0x168, "U\u{303}"), (0x169, "u\u{303}"),
    (0x16A, "U\u{304}"), (0x16B, "u\u{304}"), (0x16C, "U\u{306}"), (0x16D, "u\u{306}"),
    (0x16E, "U\u{30a}"), (0x16F, "u\u{30a}"), (0x170, "U\u{30b}"), (0x171, "u\u{30b}"),
    (0x172, "U\u{328}"), (0x173, "u\u{328}"), (0x174, "W\u{302}"), (0x175, "w\u{302}"),
    (0x176, "Y\u{302}"), (0x177, "y\u{302}"), (0x178, "Y\u{308}"), (0x179, "Z\u{301}"),
    (0x17A, "z\u{301}"), (0x17B, "Z\u{307}"), (0x17C, "z\u{307}"), (0x17D, "Z\u{30c}"),
    (0x17E, "z\u{30c}"), (0x17F, "s"), (0x1A0, "O\u{31b}"), (0x1A1, "o\u{31b}"),
    (0x1AF, "U\u{31b}"), (0x1B0, "u\u{31b}"), (0x1C4, "DZ\u{30c}"), (0x1C5, "Dz\u{30c}"),
    (0x1C6, "dz\u{30c}"), (0x1C7, "LJ"), (0x1C8, "Lj"), (0x1C9, "lj"), (0x1CA, "NJ"),
    (0x1CB, "Nj"), (0x1CC, "nj"), (0x1CD, "A\u{30c}"), (0x1CE, "a\u{30c}"),
    (0x1CF, "I\u{30c}"), (0x1D0, "i\u{30c}"), (0x1D1, "O\u{30c}"), (0x1D2, "o\u{30c}"),
    (0x1D3, "U\u{30c}"), (0x1D4, "u\u{30c}"), (0x1D5, "U\u{308}\u{304}"),
    (0x1D6, "u\u{308}\u{304}"), (0x1D7, "U\u{308}\u{301}"), (0x1D8, "u\u{308}\u{301}"),
    (0x1D9, "U\u{308}\u{30c}"), (0x1DA, "u\u{308}\u{30c}"), (0x1DB, "U\u{308}\u{300}"),
    (0x1DC, "u\u{308}\u{300}"), (0x1DE, "A\u{308}\u{304}"), (0x1DF, "a\u{308}\u{304}"),
    (0x1E0, "A\u{307}\u{304}"), (0x1E1, "a\u{307}\u{304}"), (0x1E2, "\u{c6}\u{304}"),
    (0x1E3, "\u{e6}\u{304}"), (0x1E6, "G\u{30c}"), (0x1E7, "g\u{30c}"), (0x1E8, "K\u{30c}"),
    (0x1E9, "k\u{30c}"), (0x1EA, "O\u{328}"), (0x1EB, "o\u{328}"), (0x1EC, "O\u{328}\u{304}"),
    (0x1ED, "o\u{328}\u{304}"), (0x1EE, "\u{1b7}\u{30c}"), (0x1EF, "\u{292}\u{30c}"),
    (0x1F0, "j\u{30c}"), (0x1F1, "DZ"), (0x1F2, "Dz"), (0x1F3, "dz"), (0x1F4, "G\u{301}"),
    (0x1F5, "g\u{301}"), (0x1F8, "N\u{300}"), (0x1F9, "n\u{300}"), (0x1FA, "A\u{30a}\u{301}"),
    (0x1FB, "a\u{30a}\u{301}"), (0x1FC, "\u{c6}\u{301}"), (0x1FD, "\u{e6}\u{301}"),
    (0x1FE, "\u{d8}\u{301}"), (0x1FF, "\u{f8}\u{301}"), (0x200, "A\u{30f}"),
    (0x201, "a\u{30f}"), (0x202, "A\u{311}"), (0x203, "a\u{311}"), (0x204, "E\u{30f}"),
    (0x205, "e\u{30f}"), (0x206, "E\u{311}"), (0x207, "e\u{311}"), (0x208, "I\u{30f}"),
    (0x209, "i\u{30f}"), (0x20A, "I\u{311}"), (0x20B, "i\u{311}"), (0x20C, "O\u{30f}"),
    (0x20D, "o\u{30f}"), (0x20E, "O\u{311}"), (0x20F, "o\u{311}"), (0x210, "R\u{30f}"),
    (0x211, "r\u{30f}"), (0x212, "R\u{311}"), (0x213, "r\u{311}"), (0x214, "U\u{30f}"),
    (0x215, "u\u{30f}"), (0x216, "U\u{311}"), (0x217, "u\u{311}"), (0x218, "S\u{326}"),
    (0x219, "s\u{326}"), (0x21A, "T\u{326}"), (0x21B, "t\u{326}"), (0x21E, "H\u{30c}"),
    (0x21F, "h\u{30c}"), (0x226, "A\u{307}"), (0x227, "a\u{307}"), (0x228, "E\u{327}"),
    (0x229, "e\u{327}"), (0x22A, "O\u{308}\u{304}"), (0x22B, "o\u{308}\u{304}"),
    (0x22C, "O\u{303}\u{304}"), (0x22D, "o\u{303}\u{304}"), (0x22E, "O\u{307}"),
    (0x22F, "o\u{307}"), (0x230, "O\u{307}\u{304}"), (0x231, "o\u{307}\u{304}"),
    (0x232, "Y\u{304}"), (0x233, "y\u{304}"), (0x2B0, "h"), (0x2B1, "\u{266}"), (0x2B2, "j"),
    (0x2B3, "r"), (0x2B4, "\u{279}"), (0x2B5, "\u{27b}"), (0x2B6, "\u{281}"), (0x2B7, "w"),
    (0x2B8, "y"), (0x2D8, " \u{306}"), (0x2D9, " \u{307}"), (0x2DA, " \u{30a}"),
    (0x2DB, " \u{328}"), (0x2DC, " \u{303}"), (0x2DD, " \u{30b}"), (0x2E0, "\u{263}"),
    (0x2E1, "l"), (0x2E2, "s"), (0x2E3, "x"), (0x2E4, "\u{295}"), (0x340, "\u{300}"),
    (0x341, "\u{301}"), (0x343, "\u{313}"), (0x344, "\u{308}\u{301}"), (0x374, "\u{2b9}"),
    (0x37A, " \u{345}"), (0x37E, ";"), (0x384, " \u{301}"), (0x385, " \u{308}\u{301}"),
    (0x386, "\u{391}\u{301}"), (0x387, "\u{b7}"), (0x388, "\u{395}\u{301}"),
    (0x389, "\u{397}\u{301}"), (0x38A, "\u{399}\u{301}"), (0x38C, "\u{39f}\u{301}"),
    (0x38E, "\u{3a5}\u{301}"), (0x38F, "\u{3a9}\u{301}"), (0x390, "\u{3b9}\u{308}\u{301}"),
    (0x3AA, "\u{399}\u{308}"), (0x3AB, "\u{3a5}\u{308}"), (0x3AC, "\u{3b1}\u{301}"),
    (0x3AD, "\u{3b5}\u{301}"), (0x3AE, "\u{3b7}\u{301}"), (0x3AF, "\u{3b9}\u{301}"),
    (0x3B0, "\u{3c5}\u{308}\u{301}"), (0x3CA, "\u{3b9}\u{308}"), (0x3CB, "\u{3c5}\u{308}"),
    (0x3CC, "\u{3bf}\u{301}"), (0x3CD, "\u{3c5}\u{301}"), (0x3CE, "\u{3c9}\u{301}"),
    (0x3D0, "\u{3b2}"), (0x3D1, "\u{3b8}"), (0x3D2, "\u{3a5}"), (0x3D3, "\u{3a5}\u{301}"),
    (0x3D4, "\u{3a5}\u{308}"), (0x3D5, "\u{3c6}"), (0x3D6, "\u{3c0}"), (0x3F0, "\u{3ba}"),
    (0x3F1, "\u{3c1}"), (0x3F2, "\u{3c2}"), (0x3F4, "\u{398}"), (0x3F5, "\u{3b5}"),
    (0x3F9, "\u{3a3}"), (0x400, "\u{415}\u{300}"), (0x401, "\u{415}\u{308}"),
    (0x403, "\u{413}\u{301}"), (0x407, "\u{406}\u{308}"), (0x40C, "\u{41a}\u{301}"),
    (0x40D, "\u{418}\u{300}"), (0x40E, "\u{423}\u{306}"), (0x419, "\u{418}\u{306}"),
    (0x439, "\u{438}\u{306}"), (0x450, "\u{435}\u{300}"), (0x451, "\u{435}\u{308}"),
    (0x453, "\u{433}\u{301}"), (0x457, "\u{456}\u{308}"), (0x45C, "\u{43a}\u{301}"),
    (0x45D, "\u{438}\u{300}"), (0x45E, "\u{443}\u{306}"), (0x476, "\u{474}\u{30f}"),
    (0x477, "\u{475}\u{30f}"), (0x4C1, "\u{416}\u{306}"), (0x4C2, "\u{436}\u{306}"),
    (0x4D0, "\u{410}\u{306}"), (0x4D1, "\u{430}\u{306}"), (0x4D2, "\u{410}\u{308}"),
    (0x4D3, "\u{430}\u{308}"), (0x4D6, "\u{415}\u{306}"), (0x4D7, "\u{435}\u{306}"),
    (0x4DA, "\u{4d8}\u{308}"), (0x4DB, "\u{4d9}\u{308}"), (0x4DC, "\u{416}\u{308}"),
    (0x4DD, "\u{436}\u{308}"), (0x4DE, "\u{417}\u{308}"), (0x4DF, "\u{437}\u{308}"),
    (0x4E2, "\u{418}\u{304}"), (0x4E3, "\u{438}\u{304}"), (0x4E4, "\u{418}\u{308}"),
    (0x4E5, "\u{438}\u{308}"), (0x4E6, "\u{41e}\u{308}"), (0x4E7, "\u{43e}\u{308}"),
    (0x4EA, "\u{4e8}\u{308}"), (0x4EB, "\u{4e9}\u{308}"), (0x4EC, "\u{42d}\u{308}"),
    (0x4ED, "\u{44d}\u{308}"), (0x4EE, "\u{423}\u{304}"), (0x4EF, "\u{443}\u{304}"),
    (0x4F0, "\u{423}\u{308}"), (0x4F1, "\u{443}\u{308}"), (0x4F2, "\u{423}\u{30b}"),
    (0x4F3, "\u{443}\u{30b}"), (0x4F4, "\u{427}\u{308}"), (0x4F5, "\u{447}\u{308}"),
    (0x4F8, "\u{42b}\u{308}"), (0x4F9, "\u{44b}\u{308}"), (0x587, "\u{565}\u{582}"),
    (0x622, "\u{627}\u{653}"), (0x623, "\u{627}\u{654}"), (0x624, "\u{648}\u{654}"),
    (0x625, "\u{627}\u{655}"), (0x626, "\u{64a}\u{654}"), (0x675, "\u{627}\u{674}"),
    (0x676, "\u{648}\u{674}"), (0x677, "\u{6c7}\u{674}"), (0x678, "\u{64a}\u{674}"),
    (0x6C0, "\u{6d5}\u{654}"), (0x6C2, "\u{6c1}\u{654}"), (0x6D3, "\u{6d2}\u{654}"),
    (0x929, "\u{928}\u{93c}"), (0x931, "\u{930}\u{93c}"), (0x934, "\u{933}\u{93c}"),
    (0x958, "\u{915}\u{93c}"), (0x959, "\u{916}\u{93c}"), (0x95A, "\u{917}\u{93c}"),
    (0x95B, "\u{91c}\u{93c}"), (0x95C, "\u{921}\u{93c}"), (0x95D, "\u{922}\u{93c}"),
    (0x95E, "\u{92b}\u{93c}"), (0x95F, "\u{92f}\u{93c}"), (0x9CB, "\u{9c7}\u{9be}"),
    (0x9CC, "\u{9c7}\u{9d7}"), (0x9DC, "\u{9a1}\u{9bc}"), (0x9DD, "\u{9a2}\u{9bc}"),
    (0x9DF, "\u{9af}\u{9bc}"), (0xA33, "\u{a32}\u{a3c}"), (0xA36, "\u{a38}\u{a3c}"),
    (0xA59, "\u{a16}\u{a3c}"), (0xA5A, "\u{a17}\u{a3c}"), (0xA5B, "\u{a1c}\u{a3c}"),
    (0xA5E, "\u{a2b}\u{a3c}"), (0xB48, "\u{b47}\u{b56}"), (0xB4B, "\u{b47}\u{b3e}"),
    (0xB4C, "\u{b47}\u{b57}"), (0xB5C, "\u{b21}\u{b3c}"), (0xB5D, "\u{b22}\u{b3c}"),
    (0xB94, "\u{b92}\u{bd7}"), (0xBCA, "\u{bc6}\u{bbe}"), (0xBCB, "\u{bc7}\u{bbe}"),
    (0xBCC, "\u{bc6}\u{bd7}"), (0xC48, "\u{c46}\u{c56}"), (0xCC0, "\u{cbf}\u{cd5}"),
    (0xCC7, "\u{cc6}\u{cd5}"), (0xCC8, "\u{cc6}\u{cd6}"), (0xCCA, "\u{cc6}\u{cc2}"),
    (0xCCB, "\u{cc6}\u{cc2}\u{cd5}"), (0xD4A, "\u{d46}\u{d3e}"), (0xD4B, "\u{d47}\u{d3e}"),
    (0xD4C, "\u{d46}\u{d57}"), (0xDDA, "\u{dd9}\u{dca}"), (0xDDC, "\u{dd9}\u{dcf}"),
    (0xDDD, "\u{dd9}\u{dcf}\u{dca}"), (0xDDE, "\u{dd9}\u{ddf}"), (0xE33, "\u{e4d}\u{e32}"),
    (0xEB3, "\u{ecd}\u{eb2}"), (0xEDC, "\u{eab}\u{e99}"), (0xEDD, "\u{eab}\u{ea1}"),
    (0xF0C, "\u{f0b}"), (0xF43, "\u{f42}\u{fb7}"), (0xF4D, "\u{f4c}\u{fb7}"),
    (0xF52, "\u{f51}\u{fb7}"), (0xF57, "\u{f56}\u{fb7}"), (0xF5C, "\u{f5b}\u{fb7}"),
    (0xF69, "\u{f40}\u{fb5}"), (0xF73, "\u{f71}\u{f72}"), (0xF75, "\u{f71}\u{f74}"),
    (0xF76, "\u{fb2}\u{f80}"), (0xF77, "\u{fb2}\u{f71}\u{f80}"), (0xF78, "\u{fb3}\u{f80}"),
    (0xF79, "\u{fb3}\u{f71}\u{f80}"), (0xF81, "\u{f71}\u{f80}"), (0xF93, "\u{f92}\u{fb7}"),
    (0xF9D, "\u{f9c}\u{fb7}"), (0xFA2, "\u{fa1}\u{fb7}"), (0xFA7, "\u{fa6}\u{fb7}"),
    (0xFAC, "\u{fab}\u{fb7}"), (0xFB9, "\u{f90}\u{fb5}"), (0x1026, "\u{1025}\u{102e}"),
    (0x10FC, "\u{10dc}"), (0x1B06, "\u{1b05}\u{1b35}"), (0x1B08, "\u{1b07}\u{1b35}"),
    (0x1B0A, "\u{1b09}\u{1b35}"), (0x1B0C, "\u{1b0b}\u{1b35}"), (0x1B0E, "\u{1b0d}\u{1b35}"),
    (0x1B12, "\u{1b11}\u{1b35}"), (0x1B3B, "\u{1b3a}\u{1b35}"), (0x1B3D, "\u{1b3c}\u{1b35}"),
    (0x1B40, "\u{1b3e}\u{1b35}"), (0x1B41, "\u{1b3f}\u{1b35}"), (0x1B43, "\u{1b42}\u{1b35}"),
    (0x1D2C, "A"), (0x1D2D, "\u{c6}"), (0x1D2E, "B"), (0x1D30, "D"), (0x1D31, "E"),
    (0x1D32, "\u{18e}"), (0x1D33, "G"), (0x1D34, "H"), (0x1D35, "I"), (0x1D36, "J"),
    (0x1D37, "K"), (0x1D38, "L"), (0x1D39, "M"), (0x1D3A, "N"), (0x1D3C, "O"),
    (0x1D3D, "\u{222}"), (0x1D3E, "P"), (0x1D3F, "R"), (0x1D40, "T"), (0x1D41, "U"),
    (0x1D42, "W"), (0x1D43, "a"), (0x1D44, "\u{250}"), (0x1D45, "\u{251}"),
    (0x1D46, "\u{1d02}"), (0x1D47, "b"), (0x1D48, "d"), (0x1D49, "e"), (0x1D4A, "\u{259}"),
    (0x1D4B, "\u{25b}"), (0x1D4C, "\u{25c}"), (0x1D4D, "g"), (0x1D4F, "k"), (0x1D50, "m"),
    (0x1D51, "\u{14b}"), (0x1D52, "o"), (0x1D53, "\u{254}"), (0x1D54, "\u{1d16}"),
    (0x1D55, "\u{1d17}"), (0x1D56, "p"), (0x1D57, "t"), (0x1D58, "u"), (0x1D59, "\u{1d1d}"),
    (0x1D5A, "\u{26f}"), (0x1D5B, "v"), (0x1D5C, "\u{1d25}"), (0x1D5D, "\u{3b2}"),
    (0x1D5E, "\u{3b3}"), (0x1D5F, "\u{3b4}"), (0x1D60, "\u{3c6}"), (0x1D61, "\u{3c7}"),
    (0x1D62, "i"), (0x1D63, "r"), (0x1D64, "u"), (0x1D65, "v"), (0x1D66, "\u{3b2}"),
    (0x1D67, "\u{3b3}"), (0x1D68, "\u{3c1}"), (0x1D69, "\u{3c6}"), (0x1D6A, "\u{3c7}"),
    (0x1D78, "\u{43d}"), (0x1D9B, "\u{252}"), (0x1D9C, "c"), (0x1D9D, "\u{255}"),
    (0x1D9E, "\u{f0}"), (0x1D9F, "\u{25c}"), (0x1DA0, "f"), (0x1DA1, "\u{25f}"),
    (0x1DA2, "\u{261}"), (0x1DA3, "\u{265}"), (0x1DA4, "\u{268}"), (0x1DA5, "\u{269}"),
    (0x1DA6, "\u{26a}"), (0x1DA7, "\u{1d7b}"), (0x1DA8, "\u{29d}"), (0x1DA9, "\u{26d}"),
    (0x1DAA, "\u{1d85}"), (0x1DAB, "\u{29f}"), (0x1DAC, "\u{271}"), (0x1DAD, "\u{270}"),
    (0x1DAE, "\u{272}"), (0x1DAF, "\u{273}"), (0x1DB0, "\u{274}"), (0x1DB1, "\u{275}"),
    (0x1DB2, "\u{278}"), (0x1DB3, "\u{282}"), (0x1DB4, "\u{283}"), (0x1DB5, "\u{1ab}"),
    (0x1DB6, "\u{289}"), (0x1DB7, "\u{28a}"), (0x1DB8, "\u{1d1c}"), (0x1DB9, "\u{28b}"),
    (0x1DBA, "\u{28c}"), (0x1DBB, "z"), (0x1DBC, "\u{290}"), (0x1DBD, "\u{291}"),
    (0x1DBE, "\u{292}"), (0x1DBF, "\u{3b8}"), (0x1E00, "A\u{325}"), (0x1E01, "a\u{325}"),
    (0x1E02, "B\u{307}"), (0x1E03, "b\u{307}"), (0x1E04, "B\u{323}"), (0x1E05, "b\u{323}"),
    (0x1E06, "B\u{331}"), (0x1E07, "b\u{331}"), (0x1E08, "C\u{327}\u{301}"),
    (0x1E09, "c\u{327}\u{301}"), (0x1E0A, "D\u{307}"), (0x1E0B, "d\u{307}"),
    (0x1E0C, "D\u{323}"), (0x1E0D, "d\u{323}"), (0x1E0E, "D\u{331}"), (0x1E0F, "d\u{331}"),
    (0x1E10, "D\u{327}"), (0x1E11, "d\u{327}"), (0x1E12, "D\u{32d}"), (0x1E13, "d\u{32d}"),
    (0x1E14, "E\u{304}\u{300}"), (0x1E15, "e\u{304}\u{300}"), (0x1E16, "E\u{304}\u{301}"),
    (0x1E17, "e\u{304}\u{301}"), (0x1E18, "E\u{32d}"), (0x1E19, "e\u{32d}"),
    (0x1E1A, "E\u{330}"), (0x1E1B, "e\u{330}"), (0x1E1C, "E\u{327}\u{306}"),
    (0x1E1D, "e\u{327}\u{306}"), (0x1E1E, "F\u{307}"), (0x1E1F, "f\u{307}"),
    (0x1E20, "G\u{304}"), (0x1E21, "g\u{304}"), (0x1E22, "H\u{307}"), (0x1E23, "h\u{307}"),
    (0x1E24, "H\u{323}"), (0x1E25, "h\u{323}"), (0x1E26, "H\u{308}"), (0x1E27, "h\u{308}"),
    (0x1E28, "H\u{327}"), (0x1E29, "h\u{327}"), (0x1E2A, "H\u{32e}"), (0x1E2B, "h\u{32e}"),
    (0x1E2C, "I\u{330}"), (0x1E2D, "i\u{330}"), (0x1E2E, "I\u{308}\u{301}"),
    (0x1E2F, "i\u{308}\u{301}"), (0x1E30, "K\u{301}"), (0x1E31, "k\u{301}"),
    (0x1E32, "K\u{323}"), (0x1E33, "k\u{323}"), (0x1E34, "K\u{331}"), (0x1E35, "k\u{331}"),
    (0x1E36, "L\u{323}"), (0x1E37, "l\u{323}"), (0x1E38, "L\u{323}\u{304}"),
    (0x1E39, "l\u{323}\u{304}"), (0x1E3A, "L\u{331}"), (0x1E3B, "l\u{331}"),
    (0x1E3C, "L\u{32d}"), (0x1E3D, "l\u{32d}"), (0x1E3E, "M\u{301}"), (0x1E3F, "m\u{301}"),
    (0x1E40, "M\u{307}"), (0x1E41, "m\u{307}"), (0x1E42, "M\u{323}"), (0x1E43, "m\u{323}"),
    (0x1E44, "N\u{307}"), (0x1E45, "n\u{307}"), (0x1E46, "N\u{323}"), (0x1E47, "n\u{323}"),
    (0x1E48, "N\u{331}"), (0x1E49, "n\u{331}"), (0x1E4A, "N\u{32d}"), (0x1E4B, "n\u{32d}"),
    (0x1E4C, "O\u{303}\u{301}"), (0x1E4D, "o\u{303}\u{301}"), (0x1E4E, "O\u{303}\u{308}"),
    (0x1E4F, "o\u{303}\u{308}"), (0x1E50, "O\u{304}\u{300}"), (0x1E51, "o\u{304}\u{300}"),
    (0x1E52, "O\u{304}\u{301}"), (0x1E53, "o\u{304}\u{301}"), (0x1E54, "P\u{301}"),
    (0x1E55, "p\u{301}"), (0x1E56, "P\u{307}"), (0x1E57, "p\u{307}"), (0x1E58, "R\u{307}"),
    (0x1E59, "r\u{307}"), (0x1E5A, "R\u{323}"), (0x1E5B, "r\u{323}"),
    (0x1E5C, "R\u{323}\u{304}"), (0x1E5D, "r\u{323}\u{304}"), (0x1E5E, "R\u{331}"),
    (0x1E5F, "r\u{331}"), (0x1E60, "S\u{307}"), (0x1E61, "s\u{307}"), (0x1E62, "S\u{323}"),
    (0x1E63, "s\u{323}"), (0x1E64, "S\u{301}\u{307}"), (0x1E65, "s\u{301}\u{307}"),
    (0x1E66, "S\u{30c}\u{307}"), (0x1E67, "s\u{30c}\u{307}"), (0x1E68, "S\u{323}\u{307}"),
    (0x1E69, "s\u{323}\u{307}"), (0x1E6A, "T\u{307}"), (0x1E6B, "t\u{307}"),
    (0x1E6C, "T\u{323}"), (0x1E6D, "t\u{323}"), (0x1E6E, "T\u{331}"), (0x1E6F, "t\u{331}"),
    (0x1E70, "T\u{32d}"), (0x1E71, "t\u{32d}"), (0x1E72, "U\u{324}"), (0x1E73, "u\u{324}"),
    (0x1E74, "U\u{330}"), (0x1E75, "u\u{330}"), (0x1E76, "U\u{32d}"), (0x1E77, "u\u{32d}"),
    (0x1E78, "U\u{303}\u{301}"), (0x1E79, "u\u{303}\u{301}"), (0x1E7A, "U\u{304}\u{308}"),
    (0x1E7B, "u\u{304}\u{308}"), (0x1E7C, "V\u{303}"), (0x1E7D, "v\u{303}"),
    (0x1E7E, "V\u{323}"), (0x1E7F, "v\u{323}"), (0x1E80, "W\u{300}"), (0x1E81, "w\u{300}"),
    (0x1E82, "W\u{301}"), (0x1E83, "w\u{301}"), (0x1E84, "W\u{308}"), (0x1E85, "w\u{308}"),
    (0x1E86, "W\u{307}"), (0x1E87, "w\u{307}"), (0x1E88, "W\u{323}"), (0x1E89, "w\u{323}"),
    (0x1E8A, "X\u{307}"), (0x1E8B, "x\u{307}"), (0x1E8C, "X\u{308}"), (0x1E8D, "x\u{308}"),
    (0x1E8E, "Y\u{307}"), (0x1E8F, "y\u{307}"), (0x1E90, "Z\u{302}"), (0x1E91, "z\u{302}"),
    (0x1E92, "Z\u{323}"), (0x1E93, "z\u{323}"), (0x1E94, "Z\u{331}"), (0x1E95, "z\u{331}"),
    (0x1E96, "h\u{331}"), (0x1E97, "t\u{308}"), (0x1E98, "w\u{30a}"), (0x1E99, "y\u{30a}"),
    (0x1E9A, "a\u{2be}"), (0x1E9B, "s\u{307}"), (0x1EA0, "A\u{323}"), (0x1EA1, "a\u{323}"),
    (0x1EA2, "A\u{309}"), (0x1EA3, "a\u{309}"), (0x1EA4, "A\u{302}\u{301}"),
    (0x1EA5, "a\u{302}\u{301}"), (0x1EA6, "A\u{302}\u{300}"), (0x1EA7, "a\u{302}\u{300}"),
    (0x1EA8, "A\u{302}\u{309}"), (0x1EA9, "a\u{302}\u{309}"), (0x1EAA, "A\u{302}\u{303}"),
    (0x1EAB, "a\u{302}\u{303}"), (0x1EAC, "A\u{323}\u{302}"), (0x1EAD, "a\u{323}\u{302}"),
    (0x1EAE, "A\u{306}\u{301}"), (0x1EAF, "a\u{306}\u{301}"), (0x1EB0, "A\u{306}\u{300}"),
    (0x1EB1, "a\u{306}\u{300}"), (0x1EB2, "A\u{306}\u{309}"), (0x1EB3, "a\u{306}\u{309}"),
    (0x1EB4, "A\u{306}\u{303}"), (0x1EB5, "a\u{306}\u{303}"), (0x1EB6, "A\u{323}\u{306}"),
    (0x1EB7, "a\u{323}\u{306}"), (0x1EB8, "E\u{323}"), (0x1EB9, "e\u{323}"),
    (0x1EBA, "E\u{309}"), (0x1EBB, "e\u{309}"), (0x1EBC, "E\u{303}"), (0x1EBD, "e\u{303}"),
    (0x1EBE, "E\u{302}\u{301}"), (0x1EBF, "e\u{302}\u{301}"), (0x1EC0, "E\u{302}\u{300}"),
    (0x1EC1, "e\u{302}\u{300}"), (0x1EC2, "E\u{302}\u{309}"), (0x1EC3, "e\u{302}\u{309}"),
    (0x1EC4, "E\u{302}\u{303}"), (0x1EC5, "e\u{302}\u{303}"), (0x1EC6, "E\u{323}\u{302}"),
    (0x1EC7, "e\u{323}\u{302}"), (0x1EC8, "I\u{309}"), (0x1EC9, "i\u{309}"),
    (0x1ECA, "I\u{323}"), (0x1ECB, "i\u{323}"), (0x1ECC, "O\u{323}"), (0x1ECD, "o\u{323}"),
    (0x1ECE, "O\u{309}"), (0x1ECF, "o\u{309}"), (0x1ED0, "O\u{302}\u{301}"),
    (0x1ED1, "o\u{302}\u{301}"), (0x1ED2, "O\u{302}\u{300}"), (0x1ED3, "o\u{302}\u{300}"),
    (0x1ED4, "O\u{302}\u{309}"), (0x1ED5, "o\u{302}\u{309}"), (0x1ED6, "O\u{302}\u{303}"),
    (0x1ED7, "o\u{302}\u{303}"), (0x1ED8, "O\u{323}\u{302}"), (0x1ED9, "o\u{323}\u{302}"),
    (0x1EDA, "O\u{31b}\u{301}"), (0x1EDB, "o\u{31b}\u{301}"), (0x1EDC, "O\u{31b}\u{300}"),
    (0x1EDD, "o\u{31b}\u{300}"), (0x1EDE, "O\u{31b}\u{309}"), (0x1EDF, "o\u{31b}\u{309}"),
    (0x1EE0, "O\u{31b}\u{303}"), (0x1EE1, "o\u{31b}\u{303}"), (0x1EE2, "O\u{31b}\u{323}"),
    (0x1EE3, "o\u{31b}\u{323}"), (0x1EE4, "U\u{323}"), (0x1EE5, "u\u{323}"),
    (0x1EE6, "U\u{309}"), (0x1EE7, "u\u{309}"), (0x1EE8, "U\u{31b}\u{301}"),
    (0x1EE9, "u\u{31b}\u{301}"), (0x1EEA, "U\u{31b}\u{300}"), (0x1EEB, "u\u{31b}\u{300}"),
    (0x1EEC, "U\u{31b}\u{309}"), (0x1EED, "u\u{31b}\u{309}"), (0x1EEE, "U\u{31b}\u{303}"),
    (0x1EEF, "u\u{31b}\u{303}"), (0x1EF0, "U\u{31b}\u{323}"), (0x1EF1, "u\u{31b}\u{323}"),
    (0x1EF2, "Y\u{300}"), (0x1EF3, "y\u{300}"), (0x1EF4, "Y\u{323}"), (0x1EF5, "y\u{323}"),
    (0x1EF6, "Y\u{309}"), (0x1EF7, "y\u{309}"), (0x1EF8, "Y\u{303}"), (0x1EF9, "y\u{303}"),
    (0x1F00, "\u{3b1}\u{313}"), (0x1F01, "\u{3b1}\u{314}"), (0x1F02, "\u{3b1}\u{313}\u{300}"),
    (0x1F03, "\u{3b1}\u{314}\u{300}"), (0x1F04, "\u{3b1}\u{313}\u{301}"),
    (0x1F05, "\u{3b1}\u{314}\u{301}"), (0x1F06, "\u{3b1}\u{313}\u{342}"),
    (0x1F07, "\u{3b1}\u{314}\u{342}"), (0x1F08, "\u{391}\u{313}"), (0x1F09, "\u{391}\u{314}"),
    (0x1F0A, "\u{391}\u{313}\u{300}"), (0x1F0B, "\u{391}\u{314}\u{300}"),
    (0x1F0C, "\u{391}\u{313}\u{301}"), (0x1F0D, "\u{391}\u{314}\u{301}"),
    (0x1F0E, "\u{391}\u{313}\u{342}"), (0x1F0F, "\u{391}\u{314}\u{342}"),
    (0x1F10, "\u{3b5}\u{313}"), (0x1F11, "\u{3b5}\u{314}"), (0x1F12, "\u{3b5}\u{313}\u{300}"),
    (0x1F13, "\u{3b5}\u{314}\u{300}"), (0x1F14, "\u{3b5}\u{313}\u{301}"),
    (0x1F15, "\u{3b5}\u{314}\u{301}"), (0x1F18, "\u{395}\u{313}"), (0x1F19, "\u{395}\u{314}"),
    (0x1F1A, "\u{395}\u{313}\u{300}"), (0x1F1B, "\u{395}\u{314}\u{300}"),
    (0x1F1C, "\u{395}\u{313}\u{301}"), (0x1F1D, "\u{395}\u{314}\u{301}"),
    (0x1F20, "\u{3b7}\u{313}"), (0x1F21, "\u{3b7}\u{314}"), (0x1F22, "\u{3b7}\u{313}\u{300}"),
    (0x1F23, "\u{3b7}\u{314}\u{300}"), (0x1F24, "\u{3b7}\u{313}\u{301}"),
    (0x1F25, "\u{3b7}\u{314}\u{301}"), (0x1F26, "\u{3b7}\u{313}\u{342}"),
    (0x1F27, "\u{3b7}\u{314}\u{342}"), (0x1F28, "\u{397}\u{313}"), (0x1F29, "\u{397}\u{314}"),
    (0x1F2A, "\u{397}\u{313}\u{300}"), (0x1F2B, "\u{397}\u{314}\u{300}"),
    (0x1F2C, "\u{397}\u{313}\u{301}"), (0x1F2D, "\u{397}\u{314}\u{301}"),
    (0x1F2E, "\u{397}\u{313}\u{342}"), (0x1F2F, "\u{397}\u{314}\u{342}"),
    (0x1F30, "\u{3b9}\u{313}"), (0x1F31, "\u{3b9}\u{314}"), (0x1F32, "\u{3b9}\u{313}\u{300}"),
    (0x1F33, "\u{3b9}\u{314}\u{300}"), (0x1F34, "\u{3b9}\u{313}\u{301}"),
    (0x1F35, "\u{3b9}\u{314}\u{301}"), (0x1F36, "\u{3b9}\u{313}\u{342}"),
    (0x1F37, "\u{3b9}\u{314}\u{342}"), (0x1F38, "\u{399}\u{313}"), (0x1F39, "\u{399}\u{314}"),
    (0x1F3A, "\u{399}\u{313}\u{300}"), (0x1F3B, "\u{399}\u{314}\u{300}"),
    (0x1F3C, "\u{399}\u{313}\u{301}"), (0x1F3D, "\u{399}\u{314}\u{301}"),
    (0x1F3E, "\u{399}\u{313}\u{342}"), (0x1F3F, "\u{399}\u{314}\u{342}"),
    (0x1F40, "\u{3bf}\u{313}"), (0x1F41, "\u{3bf}\u{314}"), (0x1F42, "\u{3bf}\u{313}\u{300}"),
    (0x1F43, "\u{3bf}\u{314}\u{300}"), (0x1F44, "\u{3bf}\u{313}\u{301}"),
    (0x1F45, "\u{3bf}\u{314}\u{301}"), (0x1F48, "\u{39f}\u{313}"), (0x1F49, "\u{39f}\u{314}"),
    (0x1F4A, "\u{39f}\u{313}\u{300}"), (0x1F4B, "\u{39f}\u{314}\u{300}"),
    (0x1F4C, "\u{39f}\u{313}\u{301}"), (0x1F4D, "\u{39f}\u{314}\u{301}"),
    (0x1F50, "\u{3c5}\u{313}"), (0x1F51, "\u{3c5}\u{314}"), (0x1F52, "\u{3c5}\u{313}\u{300}"),
    (0x1F53, "\u{3c5}\u{314}\u{300}"), (0x1F54, "\u{3c5}\u{313}\u{301}"),
    (0x1F55, "\u{3c5}\u{314}\u{301}"), (0x1F56, "\u{3c5}\u{313}\u{342}"),
    (0x1F57, "\u{3c5}\u{314}\u{342}"), (0x1F59, "\u{3a5}\u{314}"),
    (0x1F5B, "\u{3a5}\u{314}\u{300}"), (0x1F5D, "\u{3a5}\u{314}\u{301}"),
    (0x1F5F, "\u{3a5}\u{314}\u{342}"), (0x1F60, "\u{3c9}\u{313}"), (0x1F61, "\u{3c9}\u{314}"),
    (0x1F62, "\u{3c9}\u{313}\u{300}"), (0x1F63, "\u{3c9}\u{314}\u{300}"),
    (0x1F64, "\u{3c9}\u{313}\u{301}"), (0x1F65, "\u{3c9}\u{314}\u{301}"),
    (0x1F66, "\u{3c9}\u{313}\u{342}"), (0x1F67, "\u{3c9}\u{314}\u{342}"),
    (0x1F68, "\u{3a9}\u{313}"), (0x1F69, "\u{3a9}\u{314}"), (0x1F6A, "\u{3a9}\u{313}\u{300}"),
    (0x1F6B, "\u{3a9}\u{314}\u{300}"), (0x1F6C, "\u{3a9}\u{313}\u{301}"),
    (0x1F6D, "\u{3a9}\u{314}\u{301}"), (0x1F6E, "\u{3a9}\u{313}\u{342}"),
    (0x1F6F, "\u{3a9}\u{314}\u{342}"), (0x1F70, "\u{3b1}\u{300}"), (0x1F71, "\u{3b1}\u{301}"),
    (0x1F72, "\u{3b5}\u{300}"), (0x1F73, "\u{3b5}\u{301}"), (0x1F74, "\u{3b7}\u{300}"),
    (0x1F75, "\u{3b7}\u{301}"), (0x1F76, "\u{3b9}\u{300}"), (0x1F77, "\u{3b9}\u{301}"),
    (0x1F78, "\u{3bf}\u{300}"), (0x1F79, "\u{3bf}\u{301}"), (0x1F7A, "\u{3c5}\u{300}"),
    (0x1F7B, "\u{3c5}\u{301}"), (0x1F7C, "\u{3c9}\u{300}"), (0x1F7D, "\u{3c9}\u{301}"),
    (0x1F80, "\u{3b1}\u{313}\u{345}"), (0x1F81, "\u{3b1}\u{314}\u{345}"),
    (0x1F82, "\u{3b1}\u{313}\u{300}\u{345}"), (0x1F83, "\u{3b1}\u{314}\u{300}\u{345}"),
    (0x1F84, "\u{3b1}\u{313}\u{301}\u{345}"), (0x1F85, "\u{3b1}\u{314}\u{301}\u{345}"),
    (0x1F86, "\u{3b1}\u{313}\u{342}\u{345}"), (0x1F87, "\u{3b1}\u{314}\u{342}\u{345}"),
    (0x1F88, "\u{391}\u{313}\u{345}"), (0x1F89, "\u{391}\u{314}\u{345}"),
    (0x1F8A, "\u{391}\u{313}\u{300}\u{345}"), (0x1F8B, "\u{391}\u{314}\u{300}\u{345}"),
    (0x1F8C, "\u{391}\u{313}\u{301}\u{345}"), (0x1F8D, "\u{391}\u{314}\u{301}\u{345}"),
    (0x1F8E, "\u{391}\u{313}\u{342}\u{345}"), (0x1F8F, "\u{391}\u{314}\u{342}\u{345}"),
    (0x1F90, "\u{3b7}\u{313}\u{345}"), (0x1F91, "\u{3b7}\u{314}\u{345}"),
    (0x1F92, "\u{3b7}\u{313}\u{300}\u{345}"), (0x1F93, "\u{3b7}\u{314}\u{300}\u{345}"),
    (0x1F94, "\u{3b7}\u{313}\u{301}\u{345}"), (0x1F95, "\u{3b7}\u{314}\u{301}\u{345}"),
    (0x1F96, "\u{3b7}\u{313}\u{342}\u{345}"), (0x1F97, "\u{3b7}\u{314}\u{342}\u{345}"),
    (0x1F98, "\u{397}\u{313}\u{345}"), (0x1F99, "\u{397}\u{314}\u{345}"),
    (0x1F9A, "\u{397}\u{313}\u{300}\u{345}"), (0x1F9B, "\u{397}\u{314}\u{300}\u{345}"),
    (0x1F9C, "\u{397}\u{313}\u{301}\u{345}"), (0x1F9D, "\u{397}\u{314}\u{301}\u{345}"),
    (0x1F9E, "\u{397}\u{313}\u{342}\u{345}"), (0x1F9F, "\u{397}\u{314}\u{342}\u{345}"),
    (0x1FA0, "\u{3c9}\u{313}\u{345}"), (0x1FA1, "\u{3c9}\u{314}\u{345}"),
    (0x1FA2, "\u{3c9}\u{313}\u{300}\u{345}"), (0x1FA3, "\u{3c9}\u{314}\u{300}\u{345}"),
    (0x1FA4, "\u{3c9}\u{313}\u{301}\u{345}"), (0x1FA5, "\u{3c9}\u{314}\u{301}\u{345}"),
    (0x1FA6, "\u{3c9}\u{313}\u{342}\u{345}"), (0x1FA7, "\u{3c9}\u{314}\u{342}\u{345}"),
    (0x1FA8, "\u{3a9}\u{313}\u{345}"), (0x1FA9, "\u{3a9}\u{314}\u{345}"),
    (0x1FAA, "\u{3a9}\u{313}\u{300}\u{345}"), (0x1FAB, "\u{3a9}\u{314}\u{300}\u{345}"),
    (0x1FAC, "\u{3a9}\u{313}\u{301}\u{345}"), (0x1FAD, "\u{3a9}\u{314}\u{301}\u{345}"),
    (0x1FAE, "\u{3a9}\u{313}\u{342}\u{345}"), (0x1FAF, "\u{3a9}\u{314}\u{342}\u{345}"),
    (0x1FB0, "\u{3b1}\u{306}"), (0x1FB1, "\u{3b1}\u{304}"), (0x1FB2, "\u{3b1}\u{300}\u{345}"),
    (0x1FB3, "\u{3b1}\u{345}"), (0x1FB4, "\u{3b1}\u{301}\u{345}"), (0x1FB6, "\u{3b1}\u{342}"),
    (0x1FB7, "\u{3b1}\u{342}\u{345}"), (0x1FB8, "\u{391}\u{306}"), (0x1FB9, "\u{391}\u{304}"),
    (0x1FBA, "\u{391}\u{300}"), (0x1FBB, "\u{391}\u{301}"), (0x1FBC, "\u{391}\u{345}"),
    (0x1FBD, " \u{313}"), (0x1FBE, "\u{3b9}"), (0x1FBF, " \u{313}"), (0x1FC0, " \u{342}"),
    (0x1FC1, " \u{308}\u{342}"), (0x1FC2, "\u{3b7}\u{300}\u{345}"), (0x1FC3, "\u{3b7}\u{345}"),
    (0x1FC4, "\u{3b7}\u{301}\u{345}"), (0x1FC6, "\u{3b7}\u{342}"),
    (0x1FC7, "\u{3b7}\u{342}\u{345}"), (0x1FC8, "\u{395}\u{300}"), (0x1FC9, "\u{395}\u{301}"),
    (0x1FCA, "\u{397}\u{300}"), (0x1FCB, "\u{397}\u{301}"), (0x1FCC, "\u{397}\u{345}"),
    (0x1FCD, " \u{313}\u{300}"), (0x1FCE, " \u{313}\u{301}"), (0x1FCF, " \u{313}\u{342}"),
    (0x1FD0, "\u{3b9}\u{306}"), (0x1FD1, "\u{3b9}\u{304}"), (0x1FD2, "\u{3b9}\u{308}\u{300}"),
    (0x1FD3, "\u{3b9}\u{308}\u{301}"), (0x1FD6, "\u{3b9}\u{342}"),
    (0x1FD7, "\u{3b9}\u{308}\u{342}"), (0x1FD8, "\u{399}\u{306}"), (0x1FD9, "\u{399}\u{304}"),
    (0x1FDA, "\u{399}\u{300}"), (0x1FDB, "\u{399}\u{301}"), (0x1FDD, " \u{314}\u{300}"),
    (0x1FDE, " \u{314}\u{301}"), (0x1FDF, " \u{314}\u{342}"), (0x1FE0, "\u{3c5}\u{306}"),
    (0x1FE1, "\u{3c5}\u{304}"), (0x1FE2, "\u{3c5}\u{308}\u{300}"),
    (0x1FE3, "\u{3c5}\u{308}\u{301}"), (0x1FE4, "\u{3c1}\u{313}"), (0x1FE5, "\u{3c1}\u{314}"),
    (0x1FE6, "\u{3c5}\u{342}"), (0x1FE7, "\u{3c5}\u{308}\u{342}"), (0x1FE8, "\u{3a5}\u{306}"),
    (0x1FE9, "\u{3a5}\u{304}"), (0x1FEA, "\u{3a5}\u{300}"), (0x1FEB, "\u{3a5}\u{301}"),
    (0x1FEC, "\u{3a1}\u{314}"), (0x1FED, " \u{308}\u{300}"), (0x1FEE, " \u{308}\u{301}"),
    (0x1FEF, "`"), (0x1FF2, "\u{3c9}\u{300}\u{345}"), (0x1FF3, "\u{3c9}\u{345}"),
    (0x1FF4, "\u{3c9}\u{301}\u{345}"), (0x1FF6, "\u{3c9}\u{342}"),
    (0x1FF7, "\u{3c9}\u{342}\u{345}"), (0x1FF8, "\u{39f}\u{300}"), (0x1FF9, "\u{39f}\u{301}"),
    (0x1FFA, "\u{3a9}\u{300}"), (0x1FFB, "\u{3a9}\u{301}"), (0x1FFC, "\u{3a9}\u{345}"),
    (0x1FFD, " \u{301}"), (0x1FFE, " \u{314}"), (0x2000, " "), (0x2001, " "), (0x2002, " "),
    (0x2003, " "), (0x2004, " "), (0x2005, " "), (0x2006, " "), (0x2007, " "), (0x2008, " "),
    (0x2009, " "), (0x200A, " "), (0x2011, "\u{2010}"), (0x2017, " \u{333}"), (0x2024, "."),
    (0x2025, ".."), (0x2026, "..."), (0x202F, " "), (0x2033, "\u{2032}\u{2032}"),
    (0x2034, "\u{2032}\u{2032}\u{2032}"), (0x2036, "\u{2035}\u{2035}"),
    (0x2037, "\u{2035}\u{2035}\u{2035}"), (0x203C, "!!"), (0x203E, " \u{305}"), (0x2047, "??"),
    (0x2048, "?!"), (0x2049, "!?"), (0x2057, "\u{2032}\u{2032}\u{2032}\u{2032}"),
    (0x205F, " "), (0x2070, "0"), (0x2071, "i"), (0x2074, "4"), (0x2075, "5"), (0x2076, "6"),
    (0x2077, "7"), (0x2078, "8"), (0x2079, "9"), (0x207A, "+"), (0x207B, "\u{2212}"),
    (0x207C, "="), (0x207D, "("), (0x207E, ")"), (0x207F, "n"), (0x2080, "0"), (0x2081, "1"),
    (0x2082, "2"), (0x2083, "3"), (0x2084, "4"), (0x2085, "5"), (0x2086, "6"), (0x2087, "7"),
    (0x2088, "8"), (0x2089, "9"), (0x208A, "+"), (0x208B, "\u{2212}"), (0x208C, "="),
    (0x208D, "("), (0x208E, ")"), (0x2090, "a"), (0x2091, "e"), (0x2092, "o"), (0x2093, "x"),
    (0x2094, "\u{259}"), (0x2095, "h"), (0x2096, "k"), (0x2097, "l"), (0x2098, "m"),
    (0x2099, "n"), (0x209A, "p"), (0x209B, "s"), (0x209C, "t"), (0x20A8, "Rs"),
    (0x2100, "a/c"), (0x2101, "a/s"), (0x2102, "C"), (0x2103, "\u{b0}C"), (0x2105, "c/o"),
    (0x2106, "c/u"), (0x2107, "\u{190}"), (0x2109, "\u{b0}F"), (0x210A, "g"), (0x210B, "H"),
    (0x210C, "H"), (0x210D, "H"), (0x210E, "h"), (0x210F, "\u{127}"), (0x2110, "I"),
    (0x2111, "I"), (0x2112, "L"), (0x2113, "l"), (0x2115, "N"), (0x2116, "No"), (0x2119, "P"),
    (0x211A, "Q"), (0x211B, "R"), (0x211C, "R"), (0x211D, "R"), (0x2120, "SM"),
    (0x2121, "TEL"), (0x2122, "TM"), (0x2124, "Z"), (0x2126, "\u{3a9}"), (0x2128, "Z"),
    (0x212A, "K"), (0x212B, "A\u{30a}"), (0x212C, "B"), (0x212D, "C"), (0x212F, "e"),
    (0x2130, "E"), (0x2131, "F"), (0x2133, "M"), (0x2134, "o"), (0x2135, "\u{5d0}"),
    (0x2136, "\u{5d1}"), (0x2137, "\u{5d2}"), (0x2138, "\u{5d3}"), (0x2139, "i"),
    (0x213B, "FAX"), (0x213C, "\u{3c0}"), (0x213D, "\u{3b3}"), (0x213E, "\u{393}"),
    (0x213F, "\u{3a0}"), (0x2140, "\u{2211}"), (0x2145, "D"), (0x2146, "d"), (0x2147, "e"),
    (0x2148, "i"), (0x2149, "j"), (0x2150, "1\u{2044}7"), (0x2151, "1\u{2044}9"),
    (0x2152, "1\u{2044}10"), (0x2153, "1\u{2044}3"), (0x2154, "2\u{2044}3"),
    (0x2155, "1\u{2044}5"), (0x2156, "2\u{2044}5"), (0x2157, "3\u{2044}5"),
    (0x2158, "4\u{2044}5"), (0x2159, "1\u{2044}6"), (0x215A, "5\u{2044}6"),
    (0x215B, "1\u{2044}8"), (0x215C, "3\u{2044}8"), (0x215D, "5\u{2044}8"),
    (0x215E, "7\u{2044}8"), (0x215F, "1\u{2044}"), (0x2160, "I"), (0x2161, "II"),
    (0x2162, "III"), (0x2163, "IV"), (0x2164, "V"), (0x2165, "VI"), (0x2166, "VII"),
    (0x2167, "VIII"), (0x2168, "IX"), (0x2169, "X"), (0x216A, "XI"), (0x216B, "XII"),
    (0x216C, "L"), (0x216D, "C"), (0x216E, "D"), (0x216F, "M"), (0x2170, "i"), (0x2171, "ii"),
    (0x2172, "iii"), (0x2173, "iv"), (0x2174, "v"), (0x2175, "vi"), (0x2176, "vii"),
    (0x2177, "viii"), (0x2178, "ix"), (0x2179, "x"), (0x217A, "xi"), (0x217B, "xii"),
    (0x217C, "l"), (0x217D, "c"), (0x217E, "d"), (0x217F, "m"), (0x2189, "0\u{2044}3"),
    (0x219A, "\u{2190}\u{338}"), (0x219B, "\u{2192}\u{338}"), (0x21AE, "\u{2194}\u{338}"),
    (0x21CD, "\u{21d0}\u{338}"), (0x21CE, "\u{21d4}\u{338}"), (0x21CF, "\u{21d2}\u{338}"),
    (0x2204, "\u{2203}\u{338}"), (0x2209, "\u{2208}\u{338}"), (0x220C, "\u{220b}\u{338}"),
    (0x2224, "\u{2223}\u{338}"), (0x2226, "\u{2225}\u{338}"), (0x222C, "\u{222b}\u{222b}"),
    (0x222D, "\u{222b}\u{222b}\u{222b}"), (0x222F, "\u{222e}\u{222e}"),
    (0x2230, "\u{222e}\u{222e}\u{222e}"), (0x2241, "\u{223c}\u{338}"),
    (0x2244, "\u{2243}\u{338}"), (0x2247, "\u{2245}\u{338}"), (0x2249, "\u{2248}\u{338}"),
    (0x2260, "=\u{338}"), (0x2262, "\u{2261}\u{338}"), (0x226D, "\u{224d}\u{338}"),
    (0x226E, "<\u{338}"), (0x226F, ">\u{338}"), (0x2270, "\u{2264}\u{338}"),
    (0x2271, "\u{2265}\u{338}"), (0x2274, "\u{2272}\u{338}"), (0x2275, "\u{2273}\u{338}"),
    (0x2278, "\u{2276}\u{338}"), (0x2279, "\u{2277}\u{338}"), (0x2280, "\u{227a}\u{338}"),
    (0x2281, "\u{227b}\u{338}"), (0x2284, "\u{2282}\u{338}"), (0x2285, "\u{2283}\u{338}"),
    (0x2288, "\u{2286}\u{338}"), (0x2289, "\u{2287}\u{338}"), (0x22AC, "\u{22a2}\u{338}"),
    (0x22AD, "\u{22a8}\u{338}"), (0x22AE, "\u{22a9}\u{338}"), (0x22AF, "\u{22ab}\u{338}"),
    (0x22E0, "\u{227c}\u{338}"), (0x22E1, "\u{227d}\u{338}"), (0x22E2, "\u{2291}\u{338}"),
    (0x22E3, "\u{2292}\u{338}"), (0x22EA, "\u{22b2}\u{338}"), (0x22EB, "\u{22b3}\u{338}"),
    (0x22EC, "\u{22b4}\u{338}"), (0x22ED, "\u{22b5}\u{338}"), (0x2329, "\u{3008}"),
    (0x232A, "\u{3009}"), (0x2460, "1"), (0x2461, "2"), (0x2462, "3"), (0x2463, "4"),
    (0x2464, "5"), (0x2465, "6"), (0x2466, "7"), (0x2467, "8"), (0x2468, "9"), (0x2469, "10"),
    (0x246A, "11"), (0x246B, "12"), (0x246C, "13"), (0x246D, "14"), (0x246E, "15"),
    (0x246F, "16"), (0x2470, "17"), (0x2471, "18"), (0x2472, "19"), (0x2473, "20"),
    (0x2474, "(1)"), (0x2475, "(2)"), (0x2476, "(3)"), (0x2477, "(4)"), (0x2478, "(5)"),
    (0x2479, "(6)"), (0x247A, "(7)"), (0x247B, "(8)"), (0x247C, "(9)"), (0x247D, "(10)"),
    (0x247E, "(11)"), (0x247F, "(12)"), (0x2480, "(13)"), (0x2481, "(14)"), (0x2482, "(15)"),
    (0x2483, "(16)"), (0x2484, "(17)"), (0x2485, "(18)"), (0x2486, "(19)"), (0x2487, "(20)"),
    (0x2488, "1."), (0x2489, "2."), (0x248A, "3."), (0x248B, "4."), (0x248C, "5."),
    (0x248D, "6."), (0x248E, "7."), (0x248F, "8."), (0x2490, "9."), (0x2491, "10."),
    (0x2492, "11."), (0x2493, "12."), (0x2494, "13."), (0x2495, "14."), (0x2496, "15."),
    (0x2497, "16."), (0x2498, "17."), (0x2499, "18."), (0x249A, "19."), (0x249B, "20."),
    (0x249C, "(a)"), (0x249D, "(b)"), (0x249E, "(c)"), (0x249F, "(d)"), (0x24A0, "(e)"),
    (0x24A1, "(f)"), (0x24A2, "(g)"), (0x24A3, "(h)"), (0x24A4, "(i)"), (0x24A5, "(j)"),
    (0x24A6, "(k)"), (0x24A7, "(l)"), (0x24A8, "(m)"), (0x24A9, "(n)"), (0x24AA, "(o)"),
    (0x24AB, "(p)"), (0x24AC, "(q)"), (0x24AD, "(r)"), (0x24AE, "(s)"), (0x24AF, "(t)"),
    (0x24B0, "(u)"), (0x24B1, "(v)"), (0x24B2, "(w)"), (0x24B3, "(x)"), (0x24B4, "(y)"),
    (0x24B5, "(z)"), (0x24B6, "A"), (0x24B7, "B"), (0x24B8, "C"), (0x24B9, "D"), (0x24BA, "E"),
    (0x24BB, "F"), (0x24BC, "G"), (0x24BD, "H"), (0x24BE, "I"), (0x24BF, "J"), (0x24C0, "K"),
    (0x24C1, "L"), (0x24C2, "M"), (0x24C3, "N"), (0x24C4, "O"), (0x24C5, "P"), (0x24C6, "Q"),
    (0x24C7, "R"), (0x24C8, "S"), (0x24C9, "T"), (0x24CA, "U"), (0x24CB, "V"), (0x24CC, "W"),
    (0x24CD, "X"), (0x24CE, "Y"), (0x24CF, "Z"), (0x24D0, "a"), (0x24D1, "b"), (0x24D2, "c"),
    (0x24D3, "d"), (0x24D4, "e"), (0x24D5, "f"), (0x24D6, "g"), (0x24D7, "h"), (0x24D8, "i"),
    (0x24D9, "j"), (0x24DA, "k"), (0x24DB, "l"), (0x24DC, "m"), (0x24DD, "n"), (0x24DE, "o"),
    (0x24DF, "p"), (0x24E0, "q"), (0x24E1, "r"), (0x24E2, "s"), (0x24E3, "t"), (0x24E4, "u"),
    (0x24E5, "v"), (0x24E6, "w"), (0x24E7, "x"), (0x24E8, "y"), (0x24E9, "z"), (0x24EA, "0"),
    (0x2A0C, "\u{222b}\u{222b}\u{222b}\u{222b}"), (0x2A74, "::="), (0x2A75, "=="),
    (0x2A76, "==="), (0x2ADC, "\u{2add}\u{338}"), (0x2C7C, "j"), (0x2C7D, "V"),
    (0x2D6F, "\u{2d61}"), (0x2E9F, "\u{6bcd}"), (0x2EF3, "\u{9f9f}"), (0x2F00, "\u{4e00}"),
    (0x2F01, "\u{4e28}"), (0x2F02, "\u{4e36}"), (0x2F03, "\u{4e3f}"), (0x2F04, "\u{4e59}"),
    (0x2F05, "\u{4e85}"), (0x2F06, "\u{4e8c}"), (0x2F07, "\u{4ea0}"), (0x2F08, "\u{4eba}"),
    (0x2F09, "\u{513f}"), (0x2F0A, "\u{5165}"), (0x2F0B, "\u{516b}"), (0x2F0C, "\u{5182}"),
    (0x2F0D, "\u{5196}"), (0x2F0E, "\u{51ab}"), (0x2F0F, "\u{51e0}"), (0x2F10, "\u{51f5}"),
    (0x2F11, "\u{5200}"), (0x2F12, "\u{529b}"), (0x2F13, "\u{52f9}"), (0x2F14, "\u{5315}"),
    (0x2F15, "\u{531a}"), (0x2F16, "\u{5338}"), (0x2F17, "\u{5341}"), (0x2F18, "\u{535c}"),
    (0x2F19, "\u{5369}"), (0x2F1A, "\u{5382}"), (0x2F1B, "\u{53b6}"), (0x2F1C, "\u{53c8}"),
    (0x2F1D, "\u{53e3}"), (0x2F1E, "\u{56d7}"), (0x2F1F, "\u{571f}"), (0x2F20, "\u{58eb}"),
    (0x2F21, "\u{5902}"), (0x2F22, "\u{590a}"), (0x2F23, "\u{5915}"), (0x2F24, "\u{5927}"),
    (0x2F25, "\u{5973}"), (0x2F26, "\u{5b50}"), (0x2F27, "\u{5b80}"), (0x2F28, "\u{5bf8}"),
    (0x2F29, "\u{5c0f}"), (0x2F2A, "\u{5c22}"), (0x2F2B, "\u{5c38}"), (0x2F2C, "\u{5c6e}"),
    (0x2F2D, "\u{5c71}"), (0x2F2E, "\u{5ddb}"), (0x2F2F, "\u{5de5}"), (0x2F30, "\u{5df1}"),
    (0x2F31, "\u{5dfe}"), (0x2F32, "\u{5e72}"), (0x2F33, "\u{5e7a}"), (0x2F34, "\u{5e7f}"),
    (0x2F35, "\u{5ef4}"), (0x2F36, "\u{5efe}"), (0x2F37, "\u{5f0b}"), (0x2F38, "\u{5f13}"),
    (0x2F39, "\u{5f50}"), (0x2F3A, "\u{5f61}"), (0x2F3B, "\u{5f73}"), (0x2F3C, "\u{5fc3}"),
    (0x2F3D, "\u{6208}"), (0x2F3E, "\u{6236}"), (0x2F3F, "\u{624b}"), (0x2F40, "\u{652f}"),
    (0x2F41, "\u{6534}"), (0x2F42, "\u{6587}"), (0x2F43, "\u{6597}"), (0x2F44, "\u{65a4}"),
    (0x2F45, "\u{65b9}"), (0x2F46, "\u{65e0}"), (0x2F47, "\u{65e5}"), (0x2F48, "\u{66f0}"),
    (0x2F49, "\u{6708}"), (0x2F4A, "\u{6728}"), (0x2F4B, "\u{6b20}"), (0x2F4C, "\u{6b62}"),
    (0x2F4D, "\u{6b79}"), (0x2F4E, "\u{6bb3}"), (0x2F4F, "\u{6bcb}"), (0x2F50, "\u{6bd4}"),
    (0x2F51, "\u{6bdb}"), (0x2F52, "\u{6c0f}"), (0x2F53, "\u{6c14}"), (0x2F54, "\u{6c34}"),
    (0x2F55, "\u{706b}"), (0x2F56, "\u{722a}"), (0x2F57, "\u{7236}"), (0x2F58, "\u{723b}"),
    (0x2F59, "\u{723f}"), (0x2F5A, "\u{7247}"), (0x2F5B, "\u{7259}"), (0x2F5C, "\u{725b}"),
    (0x2F5D, "\u{72ac}"), (0x2F5E, "\u{7384}"), (0x2F5F, "\u{7389}"), (0x2F60, "\u{74dc}"),
    (0x2F61, "\u{74e6}"), (0x2F62, "\u{7518}"), (0x2F63, "\u{751f}"), (0x2F64, "\u{7528}"),
    (0x2F65, "\u{7530}"), (0x2F66, "\u{758b}"), (0x2F67, "\u{7592}"), (0x2F68, "\u{7676}"),
    (0x2F69, "\u{767d}"), (0x2F6A, "\u{76ae}"), (0x2F6B, "\u{76bf}"), (0x2F6C, "\u{76ee}"),
    (0x2F6D, "\u{77db}"), (0x2F6E, "\u{77e2}"), (0x2F6F, "\u{77f3}"), (0x2F70, "\u{793a}"),
    (0x2F71, "\u{79b8}"), (0x2F72, "\u{79be}"), (0x2F73, "\u{7a74}"), (0x2F74, "\u{7acb}"),
    (0x2F75, "\u{7af9}"), (0x2F76, "\u{7c73}"), (0x2F77, "\u{7cf8}"), (0x2F78, "\u{7f36}"),
    (0x2F79, "\u{7f51}"), (0x2F7A, "\u{7f8a}"), (0x2F7B, "\u{7fbd}"), (0x2F7C, "\u{8001}"),
    (0x2F7D, "\u{800c}"), (0x2F7E, "\u{8012}"), (0x2F7F, "\u{8033}"), (0x2F80, "\u{807f}"),
    (0x2F81, "\u{8089}"), (0x2F82, "\u{81e3}"), (0x2F83, "\u{81ea}"), (0x2F84, "\u{81f3}"),
    (0x2F85, "\u{81fc}"), (0x2F86, "\u{820c}"), (0x2F87, "\u{821b}"), (0x2F88, "\u{821f}"),
    (0x2F89, "\u{826e}"), (0x2F8A, "\u{8272}"), (0x2F8B, "\u{8278}"), (0x2F8C, "\u{864d}"),
    (0x2F8D, "\u{866b}"), (0x2F8E, "\u{8840}"), (0x2F8F, "\u{884c}"), (0x2F90, "\u{8863}"),
    (0x2F91, "\u{897e}"), (0x2F92, "\u{898b}"), (0x2F93, "\u{89d2}"), (0x2F94, "\u{8a00}"),
    (0x2F95, "\u{8c37}"), (0x2F96, "\u{8c46}"), (0x2F97, "\u{8c55}"), (0x2F98, "\u{8c78}"),
    (0x2F99, "\u{8c9d}"), (0x2F9A, "\u{8d64}"), (0x2F9B, "\u{8d70}"), (0x2F9C, "\u{8db3}"),
    (0x2F9D, "\u{8eab}"), (0x2F9E, "\u{8eca}"), (0x2F9F, "\u{8f9b}"), (0x2FA0, "\u{8fb0}"),
    (0x2FA1, "\u{8fb5}"), (0x2FA2, "\u{9091}"), (0x2FA3, "\u{9149}"), (0x2FA4, "\u{91c6}"),
    (0x2FA5, "\u{91cc}"), (0x2FA6, "\u{91d1}"), (0x2FA7, "\u{9577}"), (0x2FA8, "\u{9580}"),
    (0x2FA9, "\u{961c}"), (0x2FAA, "\u{96b6}"), (0x2FAB, "\u{96b9}"), (0x2FAC, "\u{96e8}"),
    (0x2FAD, "\u{9751}"), (0x2FAE, "\u{975e}"), (0x2FAF, "\u{9762}"), (0x2FB0, "\u{9769}"),
    (0x2FB1, "\u{97cb}"), (0x2FB2, "\u{97ed}"), (0x2FB3, "\u{97f3}"), (0x2FB4, "\u{9801}"),
    (0x2FB5, "\u{98a8}"), (0x2FB6, "\u{98db}"), (0x2FB7, "\u{98df}"), (0x2FB8, "\u{9996}"),
    (0x2FB9, "\u{9999}"), (0x2FBA, "\u{99ac}"), (0x2FBB, "\u{9aa8}"), (0x2FBC, "\u{9ad8}"),
    (0x2FBD, "\u{9adf}"), (0x2FBE, "\u{9b25}"), (0x2FBF, "\u{9b2f}"), (0x2FC0, "\u{9b32}"),
    (0x2FC1, "\u{9b3c}"), (0x2FC2, "\u{9b5a}"), (0x2FC3, "\u{9ce5}"), (0x2FC4, "\u{9e75}"),
    (0x2FC5, "\u{9e7f}"), (0x2FC6, "\u{9ea5}"), (0x2FC7, "\u{9ebb}"), (0x2FC8, "\u{9ec3}"),
    (0x2FC9, "\u{9ecd}"), (0x2FCA, "\u{9ed1}"), (0x2FCB, "\u{9ef9}"), (0x2FCC, "\u{9efd}"),
    (0x2FCD, "\u{9f0e}"), (0x2FCE, "\u{9f13}"), (0x2FCF, "\u{9f20}"), (0x2FD0, "\u{9f3b}"),
    (0x2FD1, "\u{9f4a}"), (0x2FD2, "\u{9f52}"), (0x2FD3, "\u{9f8d}"), (0x2FD4, "\u{9f9c}"),
    (0x2FD5, "\u{9fa0}"), (0x3000, " "), (0x3036, "\u{3012}"), (0x3038, "\u{5341}"),
    (0x3039, "\u{5344}"), (0x303A, "\u{5345}"), (0x304C, "\u{304b}\u{3099}"),
    (0x304E, "\u{304d}\u{3099}"), (0x3050, "\u{304f}\u{3099}"), (0x3052, "\u{3051}\u{3099}"),
    (0x3054, "\u{3053}\u{3099}"), (0x3056, "\u{3055}\u{3099}"), (0x3058, "\u{3057}\u{3099}"),
    (0x305A, "\u{3059}\u{3099}"), (0x305C, "\u{305b}\u{3099}"), (0x305E, "\u{305d}\u{3099}"),
    (0x3060, "\u{305f}\u{3099}"), (0x3062, "\u{3061}\u{3099}"), (0x3065, "\u{3064}\u{3099}"),
    (0x3067, "\u{3066}\u{3099}"), (0x3069, "\u{3068}\u{3099}"), (0x3070, "\u{306f}\u{3099}"),
    (0x3071, "\u{306f}\u{309a}"), (0x3073, "\u{3072}\u{3099}"), (0x3074, "\u{3072}\u{309a}"),
    (0x3076, "\u{3075}\u{3099}"), (0x3077, "\u{3075}\u{309a}"), (0x3079, "\u{3078}\u{3099}"),
    (0x307A, "\u{3078}\u{309a}"), (0x307C, "\u{307b}\u{3099}"), (0x307D, "\u{307b}\u{309a}"),
    (0x3094, "\u{3046}\u{3099}"), (0x309B, " \u{3099}"), (0x309C, " \u{309a}"),
    (0x309E, "\u{309d}\u{3099}"), (0x309F, "\u{3088}\u{308a}"), (0x30AC, "\u{30ab}\u{3099}"),
    (0x30AE, "\u{30ad}\u{3099}"), (0x30B0, "\u{30af}\u{3099}"), (0x30B2, "\u{30b1}\u{3099}"),
    (0x30B4, "\u{30b3}\u{3099}"), (0x30B6, "\u{30b5}\u{3099}"), (0x30B8, "\u{30b7}\u{3099}"),
    (0x30BA, "\u{30b9}\u{3099}"), (0x30BC, "\u{30bb}\u{3099}"), (0x30BE, "\u{30bd}\u{3099}"),
    (0x30C0, "\u{30bf}\u{3099}"), (0x30C2, "\u{30c1}\u{3099}"), (0x30C5, "\u{30c4}\u{3099}"),
    (0x30C7, "\u{30c6}\u{3099}"), (0x30C9, "\u{30c8}\u{3099}"), (0x30D0, "\u{30cf}\u{3099}"),
    (0x30D1, "\u{30cf}\u{309a}"), (0x30D3, "\u{30d2}\u{3099}"), (0x30D4, "\u{30d2}\u{309a}"),
    (0x30D6, "\u{30d5}\u{3099}"), (0x30D7, "\u{30d5}\u{309a}"), (0x30D9, "\u{30d8}\u{3099}"),
    (0x30DA, "\u{30d8}\u{309a}"), (0x30DC, "\u{30db}\u{3099}"), (0x30DD, "\u{30db}\u{309a}"),
    (0x30F4, "\u{30a6}\u{3099}"), (0x30F7, "\u{30ef}\u{3099}"), (0x30F8, "\u{30f0}\u{3099}"),
    (0x30F9, "\u{30f1}\u{3099}"), (0x30FA, "\u{30f2}\u{3099}"), (0x30FE, "\u{30fd}\u{3099}"),
    (0x30FF, "\u{30b3}\u{30c8}"), (0x3131, "\u{1100}"), (0x3132, "\u{1101}"),
    (0x3133, "\u{11aa}"), (0x3134, "\u{1102}"), (0x3135, "\u{11ac}"), (0x3136, "\u{11ad}"),
    (0x3137, "\u{1103}"), (0x3138, "\u{1104}"), (0x3139, "\u{1105}"), (0x313A, "\u{11b0}"),
    (0x313B, "\u{11b1}"), (0x313C, "\u{11b2}"), (0x313D, "\u{11b3}"), (0x313E, "\u{11b4}"),
    (0x313F, "\u{11b5}"), (0x3140, "\u{111a}"), (0x3141, "\u{1106}"), (0x3142, "\u{1107}"),
    (0x3143, "\u{1108}"), (0x3144, "\u{1121}"), (0x3145, "\u{1109}"), (0x3146, "\u{110a}"),
    (0x3147, "\u{110b}"), (0x3148, "\u{110c}"), (0x3149, "\u{110d}"), (0x314A, "\u{110e}"),
    (0x314B, "\u{110f}"), (0x314C, "\u{1110}"), (0x314D, "\u{1111}"), (0x314E, "\u{1112}"),
    (0x314F, "\u{1161}"), (0x3150, "\u{1162}"), (0x3151, "\u{1163}"), (0x3152, "\u{1164}"),
    (0x3153, "\u{1165}"), (0x3154, "\u{1166}"), (0x3155, "\u{1167}"), (0x3156, "\u{1168}"),
    (0x3157, "\u{1169}"), (0x3158, "\u{116a}"), (0x3159, "\u{116b}"), (0x315A, "\u{116c}"),
    (0x315B, "\u{116d}"), (0x315C, "\u{116e}"), (0x315D, "\u{116f}"), (0x315E, "\u{1170}"),
    (0x315F, "\u{1171}"), (0x3160, "\u{1172}"), (0x3161, "\u{1173}"), (0x3162, "\u{1174}"),
    (0x3163, "\u{1175}"), (0x3164, "\u{1160}"), (0x3165, "\u{1114}"), (0x3166, "\u{1115}"),
    (0x3167, "\u{11c7}"), (0x3168, "\u{11c8}"), (0x3169, "\u{11cc}"), (0x316A, "\u{11ce}"),
    (0x316B, "\u{11d3}"), (0x316C, "\u{11d7}"), (0x316D, "\u{11d9}"), (0x316E, "\u{111c}"),
    (0x316F, "\u{11dd}"), (0x3170, "\u{11df}"), (0x3171, "\u{111d}"), (0x3172, "\u{111e}"),
    (0x3173, "\u{1120}"), (0x3174, "\u{1122}"), (0x3175, "\u{1123}"), (0x3176, "\u{1127}"),
    (0x3177, "\u{1129}"), (0x3178, "\u{112b}"), (0x3179, "\u{112c}"), (0x317A, "\u{112d}"),
    (0x317B, "\u{112e}"), (0x317C, "\u{112f}"), (0x317D, "\u{1132}"), (0x317E, "\u{1136}"),
    (0x317F, "\u{1140}"), (0x3180, "\u{1147}"), (0x3181, "\u{114c}"), (0x3182, "\u{11f1}"),
    (0x3183, "\u{11f2}"), (0x3184, "\u{1157}"), (0x3185, "\u{1158}"), (0x3186, "\u{1159}"),
    (0x3187, "\u{1184}"), (0x3188, "\u{1185}"), (0x3189, "\u{1188}"), (0x318A, "\u{1191}"),
    (0x318B, "\u{1192}"), (0x318C, "\u{1194}"), (0x318D, "\u{119e}"), (0x318E, "\u{11a1}"),
    (0x3192, "\u{4e00}"), (0x3193, "\u{4e8c}"), (0x3194, "\u{4e09}"), (0x3195, "\u{56db}"),
    (0x3196, "\u{4e0a}"), (0x3197, "\u{4e2d}"), (0x3198, "\u{4e0b}"), (0x3199, "\u{7532}"),
    (0x319A, "\u{4e59}"), (0x319B, "\u{4e19}"), (0x319C, "\u{4e01}"), (0x319D, "\u{5929}"),
    (0x319E, "\u{5730}"), (0x319F, "\u{4eba}"), (0x3200, "(\u{1100})"), (0x3201, "(\u{1102})"),
    (0x3202, "(\u{1103})"), (0x3203, "(\u{1105})"), (0x3204, "(\u{1106})"),
    (0x3205, "(\u{1107})"), (0x3206, "(\u{1109})"), (0x3207, "(\u{110b})"),
    (0x3208, "(\u{110c})"), (0x3209, "(\u{110e})"), (0x320A, "(\u{110f})"),
    (0x320B, "(\u{1110})"), (0x320C, "(\u{1111})"), (0x320D, "(\u{1112})"),
    (0x320E, "(\u{1100}\u{1161})"), (0x320F, "(\u{1102}\u{1161})"),
    (0x3210, "(\u{1103}\u{1161})"), (0x3211, "(\u{1105}\u{1161})"),
    (0x3212, "(\u{1106}\u{1161})"), (0x3213, "(\u{1107}\u{1161})"),
    (0x3214, "(\u{1109}\u{1161})"), (0x3215, "(\u{110b}\u{1161})"),
    (0x3216, "(\u{110c}\u{1161})"), (0x3217, "(\u{110e}\u{1161})"),
    (0x3218, "(\u{110f}\u{1161})"), (0x3219, "(\u{1110}\u{1161})"),
    (0x321A, "(\u{1111}\u{1161})"), (0x321B, "(\u{1112}\u{1161})"),
    (0x321C, "(\u{110c}\u{116e})"), (0x321D, "(\u{110b}\u{1169}\u{110c}\u{1165}\u{11ab})"),
    (0x321E, "(\u{110b}\u{1169}\u{1112}\u{116e})"), (0x3220, "(\u{4e00})"),
    (0x3221, "(\u{4e8c})"), (0x3222, "(\u{4e09})"), (0x3223, "(\u{56db})"),
    (0x3224, "(\u{4e94})"), (0x3225, "(\u{516d})"), (0x3226, "(\u{4e03})"),
    (0x3227, "(\u{516b})"), (0x3228, "(\u{4e5d})"), (0x3229, "(\u{5341})"),
    (0x322A, "(\u{6708})"), (0x322B, "(\u{706b})"), (0x322C, "(\u{6c34})"),
    (0x322D, "(\u{6728})"), (0x322E, "(\u{91d1})"), (0x322F, "(\u{571f})"),
    (0x3230, "(\u{65e5})"), (0x3231, "(\u{682a})"), (0x3232, "(\u{6709})"),
    (0x3233, "(\u{793e})"), (0x3234, "(\u{540d})"), (0x3235, "(\u{7279})"),
    (0x3236, "(\u{8ca1})"), (0x3237, "(\u{795d})"), (0x3238, "(\u{52b4})"),
    (0x3239, "(\u{4ee3})"), (0x323A, "(\u{547c})"), (0x323B, "(\u{5b66})"),
    (0x323C, "(\u{76e3})"), (0x323D, "(\u{4f01})"), (0x323E, "(\u{8cc7})"),
    (0x323F, "(\u{5354})"), (0x3240, "(\u{796d})"), (0x3241, "(\u{4f11})"),
    (0x3242, "(\u{81ea})"), (0x3243, "(\u{81f3})"), (0x3244, "\u{554f}"), (0x3245, "\u{5e7c}"),
    (0x3246, "\u{6587}"), (0x3247, "\u{7b8f}"), (0x3250, "PTE"), (0x3251, "21"),
    (0x3252, "22"), (0x3253, "23"), (0x3254, "24"), (0x3255, "25"), (0x3256, "26"),
    (0x3257, "27"), (0x3258, "28"), (0x3259, "29"), (0x325A, "30"), (0x325B, "31"),
    (0x325C, "32"), (0x325D, "33"), (0x325E, "34"), (0x325F, "35"), (0x3260, "\u{1100}"),
    (0x3261, "\u{1102}"), (0x3262, "\u{1103}"), (0x3263, "\u{1105}"), (0x3264, "\u{1106}"),
    (0x3265, "\u{1107}"), (0x3266, "\u{1109}"), (0x3267, "\u{110b}"), (0x3268, "\u{110c}"),
    (0x3269, "\u{110e}"), (0x326A, "\u{110f}"), (0x326B, "\u{1110}"), (0x326C, "\u{1111}"),
    (0x326D, "\u{1112}"), (0x326E, "\u{1100}\u{1161}"), (0x326F, "\u{1102}\u{1161}"),
    (0x3270, "\u{1103}\u{1161}"), (0x3271, "\u{1105}\u{1161}"), (0x3272, "\u{1106}\u{1161}"),
    (0x3273, "\u{1107}\u{1161}"), (0x3274, "\u{1109}\u{1161}"), (0x3275, "\u{110b}\u{1161}"),
    (0x3276, "\u{110c}\u{1161}"), (0x3277, "\u{110e}\u{1161}"), (0x3278, "\u{110f}\u{1161}"),
    (0x3279, "\u{1110}\u{1161}"), (0x327A, "\u{1111}\u{1161}"), (0x327B, "\u{1112}\u{1161}"),
    (0x327C, "\u{110e}\u{1161}\u{11b7}\u{1100}\u{1169}"),
    (0x327D, "\u{110c}\u{116e}\u{110b}\u{1174}"), (0x327E, "\u{110b}\u{116e}"),
    (0x3280, "\u{4e00}"), (0x3281, "\u{4e8c}"), (0x3282, "\u{4e09}"), (0x3283, "\u{56db}"),
    (0x3284, "\u{4e94}"), (0x3285, "\u{516d}"), (0x3286, "\u{4e03}"), (0x3287, "\u{516b}"),
    (0x3288, "\u{4e5d}"), (0x3289, "\u{5341}"), (0x328A, "\u{6708}"), (0x328B, "\u{706b}"),
    (0x328C, "\u{6c34}"), (0x328D, "\u{6728}"), (0x328E, "\u{91d1}"), (0x328F, "\u{571f}"),
    (0x3290, "\u{65e5}"), (0x3291, "\u{682a}"), (0x3292, "\u{6709}"), (0x3293, "\u{793e}"),
    (0x3294, "\u{540d}"), (0x3295, "\u{7279}"), (0x3296, "\u{8ca1}"), (0x3297, "\u{795d}"),
    (0x3298, "\u{52b4}"), (0x3299, "\u{79d8}"), (0x329A, "\u{7537}"), (0x329B, "\u{5973}"),
    (0x329C, "\u{9069}"), (0x329D, "\u{512a}"), (0x329E, "\u{5370}"), (0x329F, "\u{6ce8}"),
    (0x32A0, "\u{9805}"), (0x32A1, "\u{4f11}"), (0x32A2, "\u{5199}"), (0x32A3, "\u{6b63}"),
    (0x32A4, "\u{4e0a}"), (0x32A5, "\u{4e2d}"), (0x32A6, "\u{4e0b}"), (0x32A7, "\u{5de6}"),
    (0x32A8, "\u{53f3}"), (0x32A9, "\u{533b}"), (0x32AA, "\u{5b97}"), (0x32AB, "\u{5b66}"),
    (0x32AC, "\u{76e3}"), (0x32AD, "\u{4f01}"), (0x32AE, "\u{8cc7}"), (0x32AF, "\u{5354}"),
    (0x32B0, "\u{591c}"), (0x32B1, "36"), (0x32B2, "37"), (0x32B3, "38"), (0x32B4, "39"),
    (0x32B5, "40"), (0x32B6, "41"), (0x32B7, "42"), (0x32B8, "43"), (0x32B9, "44"),
    (0x32BA, "45"), (0x32BB, "46"), (0x32BC, "47"), (0x32BD, "48"), (0x32BE, "49"),
    (0x32BF, "50"), (0x32C0, "1\u{6708}"), (0x32C1, "2\u{6708}"), (0x32C2, "3\u{6708}"),
    (0x32C3, "4\u{6708}"), (0x32C4, "5\u{6708}"), (0x32C5, "6\u{6708}"), (0x32C6, "7\u{6708}"),
    (0x32C7, "8\u{6708}"), (0x32C8, "9\u{6708}"), (0x32C9, "10\u{6708}"),
    (0x32CA, "11\u{6708}"), (0x32CB, "12\u{6708}"), (0x32CC, "Hg"), (0x32CD, "erg"),
    (0x32CE, "eV"), (0x32CF, "LTD"), (0x32D0, "\u{30a2}"), (0x32D1, "\u{30a4}"),
    (0x32D2, "\u{30a6}"), (0x32D3, "\u{30a8}"), (0x32D4, "\u{30aa}"), (0x32D5, "\u{30ab}"),
    (0x32D6, "\u{30ad}"), (0x32D7, "\u{30af}"), (0x32D8, "\u{30b1}"), (0x32D9, "\u{30b3}"),
    (0x32DA, "\u{30b5}"), (0x32DB, "\u{30b7}"), (0x32DC, "\u{30b9}"), (0x32DD, "\u{30bb}"),
    (0x32DE, "\u{30bd}"), (0x32DF, "\u{30bf}"), (0x32E0, "\u{30c1}"), (0x32E1, "\u{30c4}"),
    (0x32E2, "\u{30c6}"), (0x32E3, "\u{30c8}"), (0x32E4, "\u{30ca}"), (0x32E5, "\u{30cb}"),
    (0x32E6, "\u{30cc}"), (0x32E7, "\u{30cd}"), (0x32E8, "\u{30ce}"), (0x32E9, "\u{30cf}"),
    (0x32EA, "\u{30d2}"), (0x32EB, "\u{30d5}"), (0x32EC, "\u{30d8}"), (0x32ED, "\u{30db}"),
    (0x32EE, "\u{30de}"), (0x32EF, "\u{30df}"), (0x32F0, "\u{30e0}"), (0x32F1, "\u{30e1}"),
    (0x32F2, "\u{30e2}"), (0x32F3, "\u{30e4}"), (0x32F4, "\u{30e6}"), (0x32F5, "\u{30e8}"),
    (0x32F6, "\u{30e9}"), (0x32F7, "\u{30ea}"), (0x32F8, "\u{30eb}"), (0x32F9, "\u{30ec}"),
    (0x32FA, "\u{30ed}"), (0x32FB, "\u{30ef}"), (0x32FC, "\u{30f0}"), (0x32FD, "\u{30f1}"),
    (0x32FE, "\u{30f2}"), (0x32FF, "\u{4ee4}\u{548c}"),
    (0x3300, "\u{30a2}\u{30cf}\u{309a}\u{30fc}\u{30c8}"),
    (0x3301, "\u{30a2}\u{30eb}\u{30d5}\u{30a1}"),
    (0x3302, "\u{30a2}\u{30f3}\u{30d8}\u{309a}\u{30a2}"), (0x3303, "\u{30a2}\u{30fc}\u{30eb}"),
    (0x3304, "\u{30a4}\u{30cb}\u{30f3}\u{30af}\u{3099}"), (0x3305, "\u{30a4}\u{30f3}\u{30c1}"),
    (0x3306, "\u{30a6}\u{30a9}\u{30f3}"),
    (0x3307, "\u{30a8}\u{30b9}\u{30af}\u{30fc}\u{30c8}\u{3099}"),
    (0x3308, "\u{30a8}\u{30fc}\u{30ab}\u{30fc}"), (0x3309, "\u{30aa}\u{30f3}\u{30b9}"),
    (0x330A, "\u{30aa}\u{30fc}\u{30e0}"), (0x330B, "\u{30ab}\u{30a4}\u{30ea}"),
    (0x330C, "\u{30ab}\u{30e9}\u{30c3}\u{30c8}"), (0x330D, "\u{30ab}\u{30ed}\u{30ea}\u{30fc}"),
    (0x330E, "\u{30ab}\u{3099}\u{30ed}\u{30f3}"), (0x330F, "\u{30ab}\u{3099}\u{30f3}\u{30de}"),
    (0x3310, "\u{30ad}\u{3099}\u{30ab}\u{3099}"), (0x3311, "\u{30ad}\u{3099}\u{30cb}\u{30fc}"),
    (0x3312, "\u{30ad}\u{30e5}\u{30ea}\u{30fc}"),
    (0x3313, "\u{30ad}\u{3099}\u{30eb}\u{30bf}\u{3099}\u{30fc}"), (0x3314, "\u{30ad}\u{30ed}"),
    (0x3315, "\u{30ad}\u{30ed}\u{30af}\u{3099}\u{30e9}\u{30e0}"),
    (0x3316, "\u{30ad}\u{30ed}\u{30e1}\u{30fc}\u{30c8}\u{30eb}"),
    (0x3317, "\u{30ad}\u{30ed}\u{30ef}\u{30c3}\u{30c8}"),
    (0x3318, "\u{30af}\u{3099}\u{30e9}\u{30e0}"),
    (0x3319, "\u{30af}\u{3099}\u{30e9}\u{30e0}\u{30c8}\u{30f3}"),
    (0x331A, "\u{30af}\u{30eb}\u{30bb}\u{3099}\u{30a4}\u{30ed}"),
    (0x331B, "\u{30af}\u{30ed}\u{30fc}\u{30cd}"), (0x331C, "\u{30b1}\u{30fc}\u{30b9}"),
    (0x331D, "\u{30b3}\u{30eb}\u{30ca}"), (0x331E, "\u{30b3}\u{30fc}\u{30db}\u{309a}"),
    (0x331F, "\u{30b5}\u{30a4}\u{30af}\u{30eb}"),
    (0x3320, "\u{30b5}\u{30f3}\u{30c1}\u{30fc}\u{30e0}"),
    (0x3321, "\u{30b7}\u{30ea}\u{30f3}\u{30af}\u{3099}"), (0x3322, "\u{30bb}\u{30f3}\u{30c1}"),
    (0x3323, "\u{30bb}\u{30f3}\u{30c8}"), (0x3324, "\u{30bf}\u{3099}\u{30fc}\u{30b9}"),
    (0x3325, "\u{30c6}\u{3099}\u{30b7}"), (0x3326, "\u{30c8}\u{3099}\u{30eb}"),
    (0x3327, "\u{30c8}\u{30f3}"), (0x3328, "\u{30ca}\u{30ce}"),
    (0x3329, "\u{30ce}\u{30c3}\u{30c8}"), (0x332A, "\u{30cf}\u{30a4}\u{30c4}"),
    (0x332B, "\u{30cf}\u{309a}\u{30fc}\u{30bb}\u{30f3}\u{30c8}"),
    (0x332C, "\u{30cf}\u{309a}\u{30fc}\u{30c4}"),
    (0x332D, "\u{30cf}\u{3099}\u{30fc}\u{30ec}\u{30eb}"),
    (0x332E, "\u{30d2}\u{309a}\u{30a2}\u{30b9}\u{30c8}\u{30eb}"),
    (0x332F, "\u{30d2}\u{309a}\u{30af}\u{30eb}"), (0x3330, "\u{30d2}\u{309a}\u{30b3}"),
    (0x3331, "\u{30d2}\u{3099}\u{30eb}"),
    (0x3332, "\u{30d5}\u{30a1}\u{30e9}\u{30c3}\u{30c8}\u{3099}"),
    (0x3333, "\u{30d5}\u{30a3}\u{30fc}\u{30c8}"),
    (0x3334, "\u{30d5}\u{3099}\u{30c3}\u{30b7}\u{30a7}\u{30eb}"),
    (0x3335, "\u{30d5}\u{30e9}\u{30f3}"), (0x3336, "\u{30d8}\u{30af}\u{30bf}\u{30fc}\u{30eb}"),
    (0x3337, "\u{30d8}\u{309a}\u{30bd}"), (0x3338, "\u{30d8}\u{309a}\u{30cb}\u{30d2}"),
    (0x3339, "\u{30d8}\u{30eb}\u{30c4}"), (0x333A, "\u{30d8}\u{309a}\u{30f3}\u{30b9}"),
    (0x333B, "\u{30d8}\u{309a}\u{30fc}\u{30b7}\u{3099}"),
    (0x333C, "\u{30d8}\u{3099}\u{30fc}\u{30bf}"),
    (0x333D, "\u{30db}\u{309a}\u{30a4}\u{30f3}\u{30c8}"),
    (0x333E, "\u{30db}\u{3099}\u{30eb}\u{30c8}"), (0x333F, "\u{30db}\u{30f3}"),
    (0x3340, "\u{30db}\u{309a}\u{30f3}\u{30c8}\u{3099}"), (0x3341, "\u{30db}\u{30fc}\u{30eb}"),
    (0x3342, "\u{30db}\u{30fc}\u{30f3}"), (0x3343, "\u{30de}\u{30a4}\u{30af}\u{30ed}"),
    (0x3344, "\u{30de}\u{30a4}\u{30eb}"), (0x3345, "\u{30de}\u{30c3}\u{30cf}"),
    (0x3346, "\u{30de}\u{30eb}\u{30af}"), (0x3347, "\u{30de}\u{30f3}\u{30b7}\u{30e7}\u{30f3}"),
    (0x3348, "\u{30df}\u{30af}\u{30ed}\u{30f3}"), (0x3349, "\u{30df}\u{30ea}"),
    (0x334A, "\u{30df}\u{30ea}\u{30cf}\u{3099}\u{30fc}\u{30eb}"),
    (0x334B, "\u{30e1}\u{30ab}\u{3099}"), (0x334C, "\u{30e1}\u{30ab}\u{3099}\u{30c8}\u{30f3}"),
    (0x334D, "\u{30e1}\u{30fc}\u{30c8}\u{30eb}"), (0x334E, "\u{30e4}\u{30fc}\u{30c8}\u{3099}"),
    (0x334F, "\u{30e4}\u{30fc}\u{30eb}"), (0x3350, "\u{30e6}\u{30a2}\u{30f3}"),
    (0x3351, "\u{30ea}\u{30c3}\u{30c8}\u{30eb}"), (0x3352, "\u{30ea}\u{30e9}"),
    (0x3353, "\u{30eb}\u{30d2}\u{309a}\u{30fc}"),
    (0x3354, "\u{30eb}\u{30fc}\u{30d5}\u{3099}\u{30eb}"), (0x3355, "\u{30ec}\u{30e0}"),
    (0x3356, "\u{30ec}\u{30f3}\u{30c8}\u{30b1}\u{3099}\u{30f3}"),
    (0x3357, "\u{30ef}\u{30c3}\u{30c8}"), (0x3358, "0\u{70b9}"), (0x3359, "1\u{70b9}"),
    (0x335A, "2\u{70b9}"), (0x335B, "3\u{70b9}"), (0x335C, "4\u{70b9}"), (0x335D, "5\u{70b9}"),
    (0x335E, "6\u{70b9}"), (0x335F, "7\u{70b9}"), (0x3360, "8\u{70b9}"), (0x3361, "9\u{70b9}"),
    (0x3362, "10\u{70b9}"), (0x3363, "11\u{70b9}"), (0x3364, "12\u{70b9}"),
    (0x3365, "13\u{70b9}"), (0x3366, "14\u{70b9}"), (0x3367, "15\u{70b9}"),
    (0x3368, "16\u{70b9}"), (0x3369, "17\u{70b9}"), (0x336A, "18\u{70b9}"),
    (0x336B, "19\u{70b9}"), (0x336C, "20\u{70b9}"), (0x336D, "21\u{70b9}"),
    (0x336E, "22\u{70b9}"), (0x336F, "23\u{70b9}"), (0x3370, "24\u{70b9}"), (0x3371, "hPa"),
    (0x3372, "da"), (0x3373, "AU"), (0x3374, "bar"), (0x3375, "oV"), (0x3376, "pc"),
    (0x3377, "dm"), (0x3378, "dm2"), (0x3379, "dm3"), (0x337A, "IU"),
    (0x337B, "\u{5e73}\u{6210}"), (0x337C, "\u{662d}\u{548c}"), (0x337D, "\u{5927}\u{6b63}"),
    (0x337E, "\u{660e}\u{6cbb}"), (0x337F, "\u{682a}\u{5f0f}\u{4f1a}\u{793e}"), (0x3380, "pA"),
    (0x3381, "nA"), (0x3382, "\u{3bc}A"), (0x3383, "mA"), (0x3384, "kA"), (0x3385, "KB"),
    (0x3386, "MB"), (0x3387, "GB"), (0x3388, "cal"), (0x3389, "kcal"), (0x338A, "pF"),
    (0x338B, "nF"), (0x338C, "\u{3bc}F"), (0x338D, "\u{3bc}g"), (0x338E, "mg"), (0x338F, "kg"),
    (0x3390, "Hz"), (0x3391, "kHz"), (0x3392, "MHz"), (0x3393, "GHz"), (0x3394, "THz"),
    (0x3395, "\u{3bc}l"), (0x3396, "ml"), (0x3397, "dl"), (0x3398, "kl"), (0x3399, "fm"),
    (0x339A, "nm"), (0x339B, "\u{3bc}m"), (0x339C, "mm"), (0x339D, "cm"), (0x339E, "km"),
    (0x339F, "mm2"), (0x33A0, "cm2"), (0x33A1, "m2"), (0x33A2, "km2"), (0x33A3, "mm3"),
    (0x33A4, "cm3"), (0x33A5, "m3"), (0x33A6, "km3"), (0x33A7, "m\u{2215}s"),
    (0x33A8, "m\u{2215}s2"), (0x33A9, "Pa"), (0x33AA, "kPa"), (0x33AB, "MPa"), (0x33AC, "GPa"),
    (0x33AD, "rad"), (0x33AE, "rad\u{2215}s"), (0x33AF, "rad\u{2215}s2"), (0x33B0, "ps"),
    (0x33B1, "ns"), (0x33B2, "\u{3bc}s"), (0x33B3, "ms"), (0x33B4, "pV"), (0x33B5, "nV"),
    (0x33B6, "\u{3bc}V"), (0x33B7, "mV"), (0x33B8, "kV"), (0x33B9, "MV"), (0x33BA, "pW"),
    (0x33BB, "nW"), (0x33BC, "\u{3bc}W"), (0x33BD, "mW"), (0x33BE, "kW"), (0x33BF, "MW"),
    (0x33C0, "k\u{3a9}"), (0x33C1, "M\u{3a9}"), (0x33C2, "a.m."), (0x33C3, "Bq"),
    (0x33C4, "cc"), (0x33C5, "cd"), (0x33C6, "C\u{2215}kg"), (0x33C7, "Co."), (0x33C8, "dB"),
    (0x33C9, "Gy"), (0x33CA, "ha"), (0x33CB, "HP"), (0x33CC, "in"), (0x33CD, "KK"),
    (0x33CE, "KM"), (0x33CF, "kt"), (0x33D0, "lm"), (0x33D1, "ln"), (0x33D2, "log"),
    (0x33D3, "lx"), (0x33D4, "mb"), (0x33D5, "mil"), (0x33D6, "mol"), (0x33D7, "PH"),
    (0x33D8, "p.m."), (0x33D9, "PPM"), (0x33DA, "PR"), (0x33DB, "sr"), (0x33DC, "Sv"),
    (0x33DD, "Wb"), (0x33DE, "V\u{2215}m"), (0x33DF, "A\u{2215}m"), (0x33E0, "1\u{65e5}"),
    (0x33E1, "2\u{65e5}"), (0x33E2, "3\u{65e5}"), (0x33E3, "4\u{65e5}"), (0x33E4, "5\u{65e5}"),
    (0x33E5, "6\u{65e5}"), (0x33E6, "7\u{65e5}"), (0x33E7, "8\u{65e5}"), (0x33E8, "9\u{65e5}"),
    (0x33E9, "10\u{65e5}"), (0x33EA, "11\u{65e5}"), (0x33EB, "12\u{65e5}"),
    (0x33EC, "13\u{65e5}"), (0x33ED, "14\u{65e5}"), (0x33EE, "15\u{65e5}"),
    (0x33EF, "16\u{65e5}"), (0x33F0, "17\u{65e5}"), (0x33F1, "18\u{65e5}"),
    (0x33F2, "19\u{65e5}"), (0x33F3, "20\u{65e5}"), (0x33F4, "21\u{65e5}"),
    (0x33F5, "22\u{65e5}"), (0x33F6, "23\u{65e5}"), (0x33F7, "24\u{65e5}"),
    (0x33F8, "25\u{65e5}"), (0x33F9, "26\u{65e5}"), (0x33FA, "27\u{65e5}"),
    (0x33FB, "28\u{65e5}"), (0x33FC, "29\u{65e5}"), (0x33FD, "30\u{65e5}"),
    (0x33FE, "31\u{65e5}"), (0x33FF, "gal"), (0xA69C, "\u{44a}"), (0xA69D, "\u{44c}"),
    (0xA770, "\u{a76f}"), (0xA7F2, "C"), (0xA7F3, "F"), (0xA7F4, "Q"), (0xA7F8, "\u{126}"),
    (0xA7F9, "\u{153}"), (0xAB5C, "\u{a727}"), (0xAB5D, "\u{ab37}"), (0xAB5E, "\u{26b}"),
    (0xAB5F, "\u{ab52}"), (0xAB69, "\u{28d}"), (0xF900, "\u{8c48}"), (0xF901, "\u{66f4}"),
    (0xF902, "\u{8eca}"), (0xF903, "\u{8cc8}"), (0xF904, "\u{6ed1}"), (0xF905, "\u{4e32}"),
    (0xF906, "\u{53e5}"), (0xF907, "\u{9f9c}"), (0xF908, "\u{9f9c}"), (0xF909, "\u{5951}"),
    (0xF90A, "\u{91d1}"), (0xF90B, "\u{5587}"), (0xF90C, "\u{5948}"), (0xF90D, "\u{61f6}"),
    (0xF90E, "\u{7669}"), (0xF90F, "\u{7f85}"), (0xF910, "\u{863f}"), (0xF911, "\u{87ba}"),
    (0xF912, "\u{88f8}"), (0xF913, "\u{908f}"), (0xF914, "\u{6a02}"), (0xF915, "\u{6d1b}"),
    (0xF916, "\u{70d9}"), (0xF917, "\u{73de}"), (0xF918, "\u{843d}"), (0xF919, "\u{916a}"),
    (0xF91A, "\u{99f1}"), (0xF91B, "\u{4e82}"), (0xF91C, "\u{5375}"), (0xF91D, "\u{6b04}"),
    (0xF91E, "\u{721b}"), (0xF91F, "\u{862d}"), (0xF920, "\u{9e1e}"), (0xF921, "\u{5d50}"),
    (0xF922, "\u{6feb}"), (0xF923, "\u{85cd}"), (0xF924, "\u{8964}"), (0xF925, "\u{62c9}"),
    (0xF926, "\u{81d8}"), (0xF927, "\u{881f}"), (0xF928, "\u{5eca}"), (0xF929, "\u{6717}"),
    (0xF92A, "\u{6d6a}"), (0xF92B, "\u{72fc}"), (0xF92C, "\u{90ce}"), (0xF92D, "\u{4f86}"),
    (0xF92E, "\u{51b7}"), (0xF92F, "\u{52de}"), (0xF930, "\u{64c4}"), (0xF931, "\u{6ad3}"),
    (0xF932, "\u{7210}"), (0xF933, "\u{76e7}"), (0xF934, "\u{8001}"), (0xF935, "\u{8606}"),
    (0xF936, "\u{865c}"), (0xF937, "\u{8def}"), (0xF938, "\u{9732}"), (0xF939, "\u{9b6f}"),
    (0xF93A, "\u{9dfa}"), (0xF93B, "\u{788c}"), (0xF93C, "\u{797f}"), (0xF93D, "\u{7da0}"),
    (0xF93E, "\u{83c9}"), (0xF93F, "\u{9304}"), (0xF940, "\u{9e7f}"), (0xF941, "\u{8ad6}"),
    (0xF942, "\u{58df}"), (0xF943, "\u{5f04}"), (0xF944, "\u{7c60}"), (0xF945, "\u{807e}"),
    (0xF946, "\u{7262}"), (0xF947, "\u{78ca}"), (0xF948, "\u{8cc2}"), (0xF949, "\u{96f7}"),
    (0xF94A, "\u{58d8}"), (0xF94B, "\u{5c62}"), (0xF94C, "\u{6a13}"), (0xF94D, "\u{6dda}"),
    (0xF94E, "\u{6f0f}"), (0xF94F, "\u{7d2f}"), (0xF950, "\u{7e37}"), (0xF951, "\u{964b}"),
    (0xF952, "\u{52d2}"), (0xF953, "\u{808b}"), (0xF954, "\u{51dc}"), (0xF955, "\u{51cc}"),
    (0xF956, "\u{7a1c}"), (0xF957, "\u{7dbe}"), (0xF958, "\u{83f1}"), (0xF959, "\u{9675}"),
    (0xF95A, "\u{8b80}"), (0xF95B, "\u{62cf}"), (0xF95C, "\u{6a02}"), (0xF95D, "\u{8afe}"),
    (0xF95E, "\u{4e39}"), (0xF95F, "\u{5be7}"), (0xF960, "\u{6012}"), (0xF961, "\u{7387}"),
    (0xF962, "\u{7570}"), (0xF963, "\u{5317}"), (0xF964, "\u{78fb}"), (0xF965, "\u{4fbf}"),
    (0xF966, "\u{5fa9}"), (0xF967, "\u{4e0d}"), (0xF968, "\u{6ccc}"), (0xF969, "\u{6578}"),
    (0xF96A, "\u{7d22}"), (0xF96B, "\u{53c3}"), (0xF96C, "\u{585e}"), (0xF96D, "\u{7701}"),
    (0xF96E, "\u{8449}"), (0xF96F, "\u{8aaa}"), (0xF970, "\u{6bba}"), (0xF971, "\u{8fb0}"),
    (0xF972, "\u{6c88}"), (0xF973, "\u{62fe}"), (0xF974, "\u{82e5}"), (0xF975, "\u{63a0}"),
    (0xF976, "\u{7565}"), (0xF977, "\u{4eae}"), (0xF978, "\u{5169}"), (0xF979, "\u{51c9}"),
    (0xF97A, "\u{6881}"), (0xF97B, "\u{7ce7}"), (0xF97C, "\u{826f}"), (0xF97D, "\u{8ad2}"),
    (0xF97E, "\u{91cf}"), (0xF97F, "\u{52f5}"), (0xF980, "\u{5442}"), (0xF981, "\u{5973}"),
    (0xF982, "\u{5eec}"), (0xF983, "\u{65c5}"), (0xF984, "\u{6ffe}"), (0xF985, "\u{792a}"),
    (0xF986, "\u{95ad}"), (0xF987, "\u{9a6a}"), (0xF988, "\u{9e97}"), (0xF989, "\u{9ece}"),
    (0xF98A, "\u{529b}"), (0xF98B, "\u{66c6}"), (0xF98C, "\u{6b77}"), (0xF98D, "\u{8f62}"),
    (0xF98E, "\u{5e74}"), (0xF98F, "\u{6190}"), (0xF990, "\u{6200}"), (0xF991, "\u{649a}"),
    (0xF992, "\u{6f23}"), (0xF993, "\u{7149}"), (0xF994, "\u{7489}"), (0xF995, "\u{79ca}"),
    (0xF996, "\u{7df4}"), (0xF997, "\u{806f}"), (0xF998, "\u{8f26}"), (0xF999, "\u{84ee}"),
    (0xF99A, "\u{9023}"), (0xF99B, "\u{934a}"), (0xF99C, "\u{5217}"), (0xF99D, "\u{52a3}"),
    (0xF99E, "\u{54bd}"), (0xF99F, "\u{70c8}"), (0xF9A0, "\u{88c2}"), (0xF9A1, "\u{8aaa}"),
    (0xF9A2, "\u{5ec9}"), (0xF9A3, "\u{5ff5}"), (0xF9A4, "\u{637b}"), (0xF9A5, "\u{6bae}"),
    (0xF9A6, "\u{7c3e}"), (0xF9A7, "\u{7375}"), (0xF9A8, "\u{4ee4}"), (0xF9A9, "\u{56f9}"),
    (0xF9AA, "\u{5be7}"), (0xF9AB, "\u{5dba}"), (0xF9AC, "\u{601c}"), (0xF9AD, "\u{73b2}"),
    (0xF9AE, "\u{7469}"), (0xF9AF, "\u{7f9a}"), (0xF9B0, "\u{8046}"), (0xF9B1, "\u{9234}"),
    (0xF9B2, "\u{96f6}"), (0xF9B3, "\u{9748}"), (0xF9B4, "\u{9818}"), (0xF9B5, "\u{4f8b}"),
    (0xF9B6, "\u{79ae}"), (0xF9B7, "\u{91b4}"), (0xF9B8, "\u{96b8}"), (0xF9B9, "\u{60e1}"),
    (0xF9BA, "\u{4e86}"), (0xF9BB, "\u{50da}"), (0xF9BC, "\u{5bee}"), (0xF9BD, "\u{5c3f}"),
    (0xF9BE, "\u{6599}"), (0xF9BF, "\u{6a02}"), (0xF9C0, "\u{71ce}"), (0xF9C1, "\u{7642}"),
    (0xF9C2, "\u{84fc}"), (0xF9C3, "\u{907c}"), (0xF9C4, "\u{9f8d}"), (0xF9C5, "\u{6688}"),
    (0xF9C6, "\u{962e}"), (0xF9C7, "\u{5289}"), (0xF9C8, "\u{677b}"), (0xF9C9, "\u{67f3}"),
    (0xF9CA, "\u{6d41}"), (0xF9CB, "\u{6e9c}"), (0xF9CC, "\u{7409}"), (0xF9CD, "\u{7559}"),
    (0xF9CE, "\u{786b}"), (0xF9CF, "\u{7d10}"), (0xF9D0, "\u{985e}"), (0xF9D1, "\u{516d}"),
    (0xF9D2, "\u{622e}"), (0xF9D3, "\u{9678}"), (0xF9D4, "\u{502b}"), (0xF9D5, "\u{5d19}"),
    (0xF9D6, "\u{6dea}"), (0xF9D7, "\u{8f2a}"), (0xF9D8, "\u{5f8b}"), (0xF9D9, "\u{6144}"),
    (0xF9DA, "\u{6817}"), (0xF9DB, "\u{7387}"), (0xF9DC, "\u{9686}"), (0xF9DD, "\u{5229}"),
    (0xF9DE, "\u{540f}"), (0xF9DF, "\u{5c65}"), (0xF9E0, "\u{6613}"), (0xF9E1, "\u{674e}"),
    (0xF9E2, "\u{68a8}"), (0xF9E3, "\u{6ce5}"), (0xF9E4, "\u{7406}"), (0xF9E5, "\u{75e2}"),
    (0xF9E6, "\u{7f79}"), (0xF9E7, "\u{88cf}"), (0xF9E8, "\u{88e1}"), (0xF9E9, "\u{91cc}"),
    (0xF9EA, "\u{96e2}"), (0xF9EB, "\u{533f}"), (0xF9EC, "\u{6eba}"), (0xF9ED, "\u{541d}"),
    (0xF9EE, "\u{71d0}"), (0xF9EF, "\u{7498}"), (0xF9F0, "\u{85fa}"), (0xF9F1, "\u{96a3}"),
    (0xF9F2, "\u{9c57}"), (0xF9F3, "\u{9e9f}"), (0xF9F4, "\u{6797}"), (0xF9F5, "\u{6dcb}"),
    (0xF9F6, "\u{81e8}"), (0xF9F7, "\u{7acb}"), (0xF9F8, "\u{7b20}"), (0xF9F9, "\u{7c92}"),
    (0xF9FA, "\u{72c0}"), (0xF9FB, "\u{7099}"), (0xF9FC, "\u{8b58}"), (0xF9FD, "\u{4ec0}"),
    (0xF9FE, "\u{8336}"), (0xF9FF, "\u{523a}"), (0xFA00, "\u{5207}"), (0xFA01, "\u{5ea6}"),
    (0xFA02, "\u{62d3}"), (0xFA03, "\u{7cd6}"), (0xFA04, "\u{5b85}"), (0xFA05, "\u{6d1e}"),
    (0xFA06, "\u{66b4}"), (0xFA07, "\u{8f3b}"), (0xFA08, "\u{884c}"), (0xFA09, "\u{964d}"),
    (0xFA0A, "\u{898b}"), (0xFA0B, "\u{5ed3}"), (0xFA0C, "\u{5140}"), (0xFA0D, "\u{55c0}"),
    (0xFA10, "\u{585a}"), (0xFA12, "\u{6674}"), (0xFA15, "\u{51de}"), (0xFA16, "\u{732a}"),
    (0xFA17, "\u{76ca}"), (0xFA18, "\u{793c}"), (0xFA19, "\u{795e}"), (0xFA1A, "\u{7965}"),
    (0xFA1B, "\u{798f}"), (0xFA1C, "\u{9756}"), (0xFA1D, "\u{7cbe}"), (0xFA1E, "\u{7fbd}"),
    (0xFA20, "\u{8612}"), (0xFA22, "\u{8af8}"), (0xFA25, "\u{9038}"), (0xFA26, "\u{90fd}"),
    (0xFA2A, "\u{98ef}"), (0xFA2B, "\u{98fc}"), (0xFA2C, "\u{9928}"), (0xFA2D, "\u{9db4}"),
    (0xFA2E, "\u{90de}"), (0xFA2F, "\u{96b7}"), (0xFA30, "\u{4fae}"), (0xFA31, "\u{50e7}"),
    (0xFA32, "\u{514d}"), (0xFA33, "\u{52c9}"), (0xFA34, "\u{52e4}"), (0xFA35, "\u{5351}"),
    (0xFA36, "\u{559d}"), (0xFA37, "\u{5606}"), (0xFA38, "\u{5668}"), (0xFA39, "\u{5840}"),
    (0xFA3A, "\u{58a8}"), (0xFA3B, "\u{5c64}"), (0xFA3C, "\u{5c6e}"), (0xFA3D, "\u{6094}"),
    (0xFA3E, "\u{6168}"), (0xFA3F, "\u{618e}"), (0xFA40, "\u{61f2}"), (0xFA41, "\u{654f}"),
    (0xFA42, "\u{65e2}"), (0xFA43, "\u{6691}"), (0xFA44, "\u{6885}"), (0xFA45, "\u{6d77}"),
    (0xFA46, "\u{6e1a}"), (0xFA47, "\u{6f22}"), (0xFA48, "\u{716e}"), (0xFA49, "\u{722b}"),
    (0xFA4A, "\u{7422}"), (0xFA4B, "\u{7891}"), (0xFA4C, "\u{793e}"), (0xFA4D, "\u{7949}"),
    (0xFA4E, "\u{7948}"), (0xFA4F, "\u{7950}"), (0xFA50, "\u{7956}"), (0xFA51, "\u{795d}"),
    (0xFA52, "\u{798d}"), (0xFA53, "\u{798e}"), (0xFA54, "\u{7a40}"), (0xFA55, "\u{7a81}"),
    (0xFA56, "\u{7bc0}"), (0xFA57, "\u{7df4}"), (0xFA58, "\u{7e09}"), (0xFA59, "\u{7e41}"),
    (0xFA5A, "\u{7f72}"), (0xFA5B, "\u{8005}"), (0xFA5C, "\u{81ed}"), (0xFA5D, "\u{8279}"),
    (0xFA5E, "\u{8279}"), (0xFA5F, "\u{8457}"), (0xFA60, "\u{8910}"), (0xFA61, "\u{8996}"),
    (0xFA62, "\u{8b01}"), (0xFA63, "\u{8b39}"), (0xFA64, "\u{8cd3}"), (0xFA65, "\u{8d08}"),
    (0xFA66, "\u{8fb6}"), (0xFA67, "\u{9038}"), (0xFA68, "\u{96e3}"), (0xFA69, "\u{97ff}"),
    (0xFA6A, "\u{983b}"), (0xFA6B, "\u{6075}"), (0xFA6C, "\u{242ee}"), (0xFA6D, "\u{8218}"),
    (0xFA70, "\u{4e26}"), (0xFA71, "\u{51b5}"), (0xFA72, "\u{5168}"), (0xFA73, "\u{4f80}"),
    (0xFA74, "\u{5145}"), (0xFA75, "\u{5180}"), (0xFA76, "\u{52c7}"), (0xFA77, "\u{52fa}"),
    (0xFA78, "\u{559d}"), (0xFA79, "\u{5555}"), (0xFA7A, "\u{5599}"), (0xFA7B, "\u{55e2}"),
    (0xFA7C, "\u{585a}"), (0xFA7D, "\u{58b3}"), (0xFA7E, "\u{5944}"), (0xFA7F, "\u{5954}"),
    (0xFA80, "\u{5a62}"), (0xFA81, "\u{5b28}"), (0xFA82, "\u{5ed2}"), (0xFA83, "\u{5ed9}"),
    (0xFA84, "\u{5f69}"), (0xFA85, "\u{5fad}"), (0xFA86, "\u{60d8}"), (0xFA87, "\u{614e}"),
    (0xFA88, "\u{6108}"), (0xFA89, "\u{618e}"), (0xFA8A, "\u{6160}"), (0xFA8B, "\u{61f2}"),
    (0xFA8C, "\u{6234}"), (0xFA8D, "\u{63c4}"), (0xFA8E, "\u{641c}"), (0xFA8F, "\u{6452}"),
    (0xFA90, "\u{6556}"), (0xFA91, "\u{6674}"), (0xFA92, "\u{6717}"), (0xFA93, "\u{671b}"),
    (0xFA94, "\u{6756}"), (0xFA95, "\u{6b79}"), (0xFA96, "\u{6bba}"), (0xFA97, "\u{6d41}"),
    (0xFA98, "\u{6edb}"), (0xFA99, "\u{6ecb}"), (0xFA9A, "\u{6f22}"), (0xFA9B, "\u{701e}"),
    (0xFA9C, "\u{716e}"), (0xFA9D, "\u{77a7}"), (0xFA9E, "\u{7235}"), (0xFA9F, "\u{72af}"),
    (0xFAA0, "\u{732a}"), (0xFAA1, "\u{7471}"), (0xFAA2, "\u{7506}"), (0xFAA3, "\u{753b}"),
    (0xFAA4, "\u{761d}"), (0xFAA5, "\u{761f}"), (0xFAA6, "\u{76ca}"), (0xFAA7, "\u{76db}"),
    (0xFAA8, "\u{76f4}"), (0xFAA9, "\u{774a}"), (0xFAAA, "\u{7740}"), (0xFAAB, "\u{78cc}"),
    (0xFAAC, "\u{7ab1}"), (0xFAAD, "\u{7bc0}"), (0xFAAE, "\u{7c7b}"), (0xFAAF, "\u{7d5b}"),
    (0xFAB0, "\u{7df4}"), (0xFAB1, "\u{7f3e}"), (0xFAB2, "\u{8005}"), (0xFAB3, "\u{8352}"),
    (0xFAB4, "\u{83ef}"), (0xFAB5, "\u{8779}"), (0xFAB6, "\u{8941}"), (0xFAB7, "\u{8986}"),
    (0xFAB8, "\u{8996}"), (0xFAB9, "\u{8abf}"), (0xFABA, "\u{8af8}"), (0xFABB, "\u{8acb}"),
    (0xFABC, "\u{8b01}"), (0xFABD, "\u{8afe}"), (0xFABE, "\u{8aed}"), (0xFABF, "\u{8b39}"),
    (0xFAC0, "\u{8b8a}"), (0xFAC1, "\u{8d08}"), (0xFAC2, "\u{8f38}"), (0xFAC3, "\u{9072}"),
    (0xFAC4, "\u{9199}"), (0xFAC5, "\u{9276}"), (0xFAC6, "\u{967c}"), (0xFAC7, "\u{96e3}"),
    (0xFAC8, "\u{9756}"), (0xFAC9, "\u{97db}"), (0xFACA, "\u{97ff}"), (0xFACB, "\u{980b}"),
    (0xFACC, "\u{983b}"), (0xFACD, "\u{9b12}"), (0xFACE, "\u{9f9c}"), (0xFACF, "\u{2284a}"),
    (0xFAD0, "\u{22844}"), (0xFAD1, "\u{233d5}"), (0xFAD2, "\u{3b9d}"), (0xFAD3, "\u{4018}"),
    (0xFAD4, "\u{4039}"), (0xFAD5, "\u{25249}"), (0xFAD6, "\u{25cd0}"), (0xFAD7, "\u{27ed3}"),
    (0xFAD8, "\u{9f43}"), (0xFAD9, "\u{9f8e}"), (0xFB00, "ff"), (0xFB01, "fi"), (0xFB02, "fl"),
    (0xFB03, "ffi"), (0xFB04, "ffl"), (0xFB05, "st"), (0xFB06, "st"),
    (0xFB13, "\u{574}\u{576}"), (0xFB14, "\u{574}\u{565}"), (0xFB15, "\u{574}\u{56b}"),
    (0xFB16, "\u{57e}\u{576}"), (0xFB17, "\u{574}\u{56d}"), (0xFB1D, "\u{5d9}\u{5b4}"),
    (0xFB1F, "\u{5f2}\u{5b7}"), (0xFB20, "\u{5e2}"), (0xFB21, "\u{5d0}"), (0xFB22, "\u{5d3}"),
    (0xFB23, "\u{5d4}"), (0xFB24, "\u{5db}"), (0xFB25, "\u{5dc}"), (0xFB26, "\u{5dd}"),
    (0xFB27, "\u{5e8}"), (0xFB28, "\u{5ea}"), (0xFB29, "+"), (0xFB2A, "\u{5e9}\u{5c1}"),
    (0xFB2B, "\u{5e9}\u{5c2}"), (0xFB2C, "\u{5e9}\u{5bc}\u{5c1}"),
    (0xFB2D, "\u{5e9}\u{5bc}\u{5c2}"), (0xFB2E, "\u{5d0}\u{5b7}"), (0xFB2F, "\u{5d0}\u{5b8}"),
    (0xFB30, "\u{5d0}\u{5bc}"), (0xFB31, "\u{5d1}\u{5bc}"), (0xFB32, "\u{5d2}\u{5bc}"),
    (0xFB33, "\u{5d3}\u{5bc}"), (0xFB34, "\u{5d4}\u{5bc}"), (0xFB35, "\u{5d5}\u{5bc}"),
    (0xFB36, "\u{5d6}\u{5bc}"), (0xFB38, "\u{5d8}\u{5bc}"), (0xFB39, "\u{5d9}\u{5bc}"),
    (0xFB3A, "\u{5da}\u{5bc}"), (0xFB3B, "\u{5db}\u{5bc}"), (0xFB3C, "\u{5dc}\u{5bc}"),
    (0xFB3E, "\u{5de}\u{5bc}"), (0xFB40, "\u{5e0}\u{5bc}"), (0xFB41, "\u{5e1}\u{5bc}"),
    (0xFB43, "\u{5e3}\u{5bc}"), (0xFB44, "\u{5e4}\u{5bc}"), (0xFB46, "\u{5e6}\u{5bc}"),
    (0xFB47, "\u{5e7}\u{5bc}"), (0xFB48, "\u{5e8}\u{5bc}"), (0xFB49, "\u{5e9}\u{5bc}"),
    (0xFB4A, "\u{5ea}\u{5bc}"), (0xFB4B, "\u{5d5}\u{5b9}"), (0xFB4C, "\u{5d1}\u{5bf}"),
    (0xFB4D, "\u{5db}\u{5bf}"), (0xFB4E, "\u{5e4}\u{5bf}"), (0xFB4F, "\u{5d0}\u{5dc}"),
    (0xFB50, "\u{671}"), (0xFB51, "\u{671}"), (0xFB52, "\u{67b}"), (0xFB53, "\u{67b}"),
    (0xFB54, "\u{67b}"), (0xFB55, "\u{67b}"), (0xFB56, "\u{67e}"), (0xFB57, "\u{67e}"),
    (0xFB58, "\u{67e}"), (0xFB59, "\u{67e}"), (0xFB5A, "\u{680}"), (0xFB5B, "\u{680}"),
    (0xFB5C, "\u{680}"), (0xFB5D, "\u{680}"), (0xFB5E, "\u{67a}"), (0xFB5F, "\u{67a}"),
    (0xFB60, "\u{67a}"), (0xFB61, "\u{67a}"), (0xFB62, "\u{67f}"), (0xFB63, "\u{67f}"),
    (0xFB64, "\u{67f}"), (0xFB65, "\u{67f}"), (0xFB66, "\u{679}"), (0xFB67, "\u{679}"),
    (0xFB68, "\u{679}"), (0xFB69, "\u{679}"), (0xFB6A, "\u{6a4}"), (0xFB6B, "\u{6a4}"),
    (0xFB6C, "\u{6a4}"), (0xFB6D, "\u{6a4}"), (0xFB6E, "\u{6a6}"), (0xFB6F, "\u{6a6}"),
    (0xFB70, "\u{6a6}"), (0xFB71, "\u{6a6}"), (0xFB72, "\u{684}"), (0xFB73, "\u{684}"),
    (0xFB74, "\u{684}"), (0xFB75, "\u{684}"), (0xFB76, "\u{683}"), (0xFB77, "\u{683}"),
    (0xFB78, "\u{683}"), (0xFB79, "\u{683}"), (0xFB7A, "\u{686}"), (0xFB7B, "\u{686}"),
    (0xFB7C, "\u{686}"), (0xFB7D, "\u{686}"), (0xFB7E, "\u{687}"), (0xFB7F, "\u{687}"),
    (0xFB80, "\u{687}"), (0xFB81, "\u{687}"), (0xFB82, "\u{68d}"), (0xFB83, "\u{68d}"),
    (0xFB84, "\u{68c}"), (0xFB85, "\u{68c}"), (0xFB86, "\u{68e}"), (0xFB87, "\u{68e}"),
    (0xFB88, "\u{688}"), (0xFB89, "\u{688}"), (0xFB8A, "\u{698}"), (0xFB8B, "\u{698}"),
    (0xFB8C, "\u{691}"), (0xFB8D, "\u{691}"), (0xFB8E, "\u{6a9}"), (0xFB8F, "\u{6a9}"),
    (0xFB90, "\u{6a9}"), (0xFB91, "\u{6a9}"), (0xFB92, "\u{6af}"), (0xFB93, "\u{6af}"),
    (0xFB94, "\u{6af}"), (0xFB95, "\u{6af}"), (0xFB96, "\u{6b3}"), (0xFB97, "\u{6b3}"),
    (0xFB98, "\u{6b3}"), (0xFB99, "\u{6b3}"), (0xFB9A, "\u{6b1}"), (0xFB9B, "\u{6b1}"),
    (0xFB9C, "\u{6b1}"), (0xFB9D, "\u{6b1}"), (0xFB9E, "\u{6ba}"), (0xFB9F, "\u{6ba}"),
    (0xFBA0, "\u{6bb}"), (0xFBA1, "\u{6bb}"), (0xFBA2, "\u{6bb}"), (0xFBA3, "\u{6bb}"),
    (0xFBA4, "\u{6d5}\u{654}"), (0xFBA5, "\u{6d5}\u{654}"), (0xFBA6, "\u{6c1}"),
    (0xFBA7, "\u{6c1}"), (0xFBA8, "\u{6c1}"), (0xFBA9, "\u{6c1}"), (0xFBAA, "\u{6be}"),
    (0xFBAB, "\u{6be}"), (0xFBAC, "\u{6be}"), (0xFBAD, "\u{6be}"), (0xFBAE, "\u{6d2}"),
    (0xFBAF, "\u{6d2}"), (0xFBB0, "\u{6d2}\u{654}"), (0xFBB1, "\u{6d2}\u{654}"),
    (0xFBD3, "\u{6ad}"), (0xFBD4, "\u{6ad}"), (0xFBD5, "\u{6ad}"), (0xFBD6, "\u{6ad}"),
    (0xFBD7, "\u{6c7}"), (0xFBD8, "\u{6c7}"), (0xFBD9, "\u{6c6}"), (0xFBDA, "\u{6c6}"),
    (0xFBDB, "\u{6c8}"), (0xFBDC, "\u{6c8}"), (0xFBDD, "\u{6c7}\u{674}"), (0xFBDE, "\u{6cb}"),
    (0xFBDF, "\u{6cb}"), (0xFBE0, "\u{6c5}"), (0xFBE1, "\u{6c5}"), (0xFBE2, "\u{6c9}"),
    (0xFBE3, "\u{6c9}"), (0xFBE4, "\u{6d0}"), (0xFBE5, "\u{6d0}"), (0xFBE6, "\u{6d0}"),
    (0xFBE7, "\u{6d0}"), (0xFBE8, "\u{649}"), (0xFBE9, "\u{649}"),
    (0xFBEA, "\u{64a}\u{654}\u{627}"), (0xFBEB, "\u{64a}\u{654}\u{627}"),
    (0xFBEC, "\u{64a}\u{654}\u{6d5}"), (0xFBED, "\u{64a}\u{654}\u{6d5}"),
    (0xFBEE, "\u{64a}\u{654}\u{648}"), (0xFBEF, "\u{64a}\u{654}\u{648}"),
    (0xFBF0, "\u{64a}\u{654}\u{6c7}"), (0xFBF1, "\u{64a}\u{654}\u{6c7}"),
    (0xFBF2, "\u{64a}\u{654}\u{6c6}"), (0xFBF3, "\u{64a}\u{654}\u{6c6}"),
    (0xFBF4, "\u{64a}\u{654}\u{6c8}"), (0xFBF5, "\u{64a}\u{654}\u{6c8}"),
    (0xFBF6, "\u{64a}\u{654}\u{6d0}"), (0xFBF7, "\u{64a}\u{654}\u{6d0}"),
    (0xFBF8, "\u{64a}\u{654}\u{6d0}"), (0xFBF9, "\u{64a}\u{654}\u{649}"),
    (0xFBFA, "\u{64a}\u{654}\u{649}"), (0xFBFB, "\u{64a}\u{654}\u{649}"), (0xFBFC, "\u{6cc}"),
    (0xFBFD, "\u{6cc}"), (0xFBFE, "\u{6cc}"), (0xFBFF, "\u{6cc}"),
    (0xFC00, "\u{64a}\u{654}\u{62c}"), (0xFC01, "\u{64a}\u{654}\u{62d}"),
    (0xFC02, "\u{64a}\u{654}\u{645}"), (0xFC03, "\u{64a}\u{654}\u{649}"),
    (0xFC04, "\u{64a}\u{654}\u{64a}"), (0xFC05, "\u{628}\u{62c}"), (0xFC06, "\u{628}\u{62d}"),
    (0xFC07, "\u{628}\u{62e}"), (0xFC08, "\u{628}\u{645}"), (0xFC09, "\u{628}\u{649}"),
    (0xFC0A, "\u{628}\u{64a}"), (0xFC0B, "\u{62a}\u{62c}"), (0xFC0C, "\u{62a}\u{62d}"),
    (0xFC0D, "\u{62a}\u{62e}"), (0xFC0E, "\u{62a}\u{645}"), (0xFC0F, "\u{62a}\u{649}"),
    (0xFC10, "\u{62a}\u{64a}"), (0xFC11, "\u{62b}\u{62c}"), (0xFC12, "\u{62b}\u{645}"),
    (0xFC13, "\u{62b}\u{649}"), (0xFC14, "\u{62b}\u{64a}"), (0xFC15, "\u{62c}\u{62d}"),
    (0xFC16, "\u{62c}\u{645}"), (0xFC17, "\u{62d}\u{62c}"), (0xFC18, "\u{62d}\u{645}"),
    (0xFC19, "\u{62e}\u{62c}"), (0xFC1A, "\u{62e}\u{62d}"), (0xFC1B, "\u{62e}\u{645}"),
    (0xFC1C, "\u{633}\u{62c}"), (0xFC1D, "\u{633}\u{62d}"), (0xFC1E, "\u{633}\u{62e}"),
    (0xFC1F, "\u{633}\u{645}"), (0xFC20, "\u{635}\u{62d}"), (0xFC21, "\u{635}\u{645}"),
    (0xFC22, "\u{636}\u{62c}"), (0xFC23, "\u{636}\u{62d}"), (0xFC24, "\u{636}\u{62e}"),
    (0xFC25, "\u{636}\u{645}"), (0xFC26, "\u{637}\u{62d}"), (0xFC27, "\u{637}\u{645}"),
    (0xFC28, "\u{638}\u{645}"), (0xFC29, "\u{639}\u{62c}"), (0xFC2A, "\u{639}\u{645}"),
    (0xFC2B, "\u{63a}\u{62c}"), (0xFC2C, "\u{63a}\u{645}"), (0xFC2D, "\u{641}\u{62c}"),
    (0xFC2E, "\u{641}\u{62d}"), (0xFC2F, "\u{641}\u{62e}"), (0xFC30, "\u{641}\u{645}"),
    (0xFC31, "\u{641}\u{649}"), (0xFC32, "\u{641}\u{64a}"), (0xFC33, "\u{642}\u{62d}"),
    (0xFC34, "\u{642}\u{645}"), (0xFC35, "\u{642}\u{649}"), (0xFC36, "\u{642}\u{64a}"),
    (0xFC37, "\u{643}\u{627}"), (0xFC38, "\u{643}\u{62c}"), (0xFC39, "\u{643}\u{62d}"),
    (0xFC3A, "\u{643}\u{62e}"), (0xFC3B, "\u{643}\u{644}"), (0xFC3C, "\u{643}\u{645}"),
    (0xFC3D, "\u{643}\u{649}"), (0xFC3E, "\u{643}\u{64a}"), (0xFC3F, "\u{644}\u{62c}"),
    (0xFC40, "\u{644}\u{62d}"), (0xFC41, "\u{644}\u{62e}"), (0xFC42, "\u{644}\u{645}"),
    (0xFC43, "\u{644}\u{649}"), (0xFC44, "\u{644}\u{64a}"), (0xFC45, "\u{645}\u{62c}"),
    (0xFC46, "\u{645}\u{62d}"), (0xFC47, "\u{645}\u{62e}"), (0xFC48, "\u{645}\u{645}"),
    (0xFC49, "\u{645}\u{649}"), (0xFC4A, "\u{645}\u{64a}"), (0xFC4B, "\u{646}\u{62c}"),
    (0xFC4C, "\u{646}\u{62d}"), (0xFC4D, "\u{646}\u{62e}"), (0xFC4E, "\u{646}\u{645}"),
    (0xFC4F, "\u{646}\u{649}"), (0xFC50, "\u{646}\u{64a}"), (0xFC51, "\u{647}\u{62c}"),
    (0xFC52, "\u{647}\u{645}"), (0xFC53, "\u{647}\u{649}"), (0xFC54, "\u{647}\u{64a}"),
    (0xFC55, "\u{64a}\u{62c}"), (0xFC56, "\u{64a}\u{62d}"), (0xFC57, "\u{64a}\u{62e}"),
    (0xFC58, "\u{64a}\u{645}"), (0xFC59, "\u{64a}\u{649}"), (0xFC5A, "\u{64a}\u{64a}"),
    (0xFC5B, "\u{630}\u{670}"), (0xFC5C, "\u{631}\u{670}"), (0xFC5D, "\u{649}\u{670}"),
    (0xFC5E, " \u{64c}\u{651}"), (0xFC5F, " \u{64d}\u{651}"), (0xFC60, " \u{64e}\u{651}"),
    (0xFC61, " \u{64f}\u{651}"), (0xFC62, " \u{650}\u{651}"), (0xFC63, " \u{651}\u{670}"),
    (0xFC64, "\u{64a}\u{654}\u{631}"), (0xFC65, "\u{64a}\u{654}\u{632}"),
    (0xFC66, "\u{64a}\u{654}\u{645}"), (0xFC67, "\u{64a}\u{654}\u{646}"),
    (0xFC68, "\u{64a}\u{654}\u{649}"), (0xFC69, "\u{64a}\u{654}\u{64a}"),
    (0xFC6A, "\u{628}\u{631}"), (0xFC6B, "\u{628}\u{632}"), (0xFC6C, "\u{628}\u{645}"),
    (0xFC6D, "\u{628}\u{646}"), (0xFC6E, "\u{628}\u{649}"), (0xFC6F, "\u{628}\u{64a}"),
    (0xFC70, "\u{62a}\u{631}"), (0xFC71, "\u{62a}\u{632}"), (0xFC72, "\u{62a}\u{645}"),
    (0xFC73, "\u{62a}\u{646}"), (0xFC74, "\u{62a}\u{649}"), (0xFC75, "\u{62a}\u{64a}"),
    (0xFC76, "\u{62b}\u{631}"), (0xFC77, "\u{62b}\u{632}"), (0xFC78, "\u{62b}\u{645}"),
    (0xFC79, "\u{62b}\u{646}"), (0xFC7A, "\u{62b}\u{649}"), (0xFC7B, "\u{62b}\u{64a}"),
    (0xFC7C, "\u{641}\u{649}"), (0xFC7D, "\u{641}\u{64a}"), (0xFC7E, "\u{642}\u{649}"),
    (0xFC7F, "\u{642}\u{64a}"), (0xFC80, "\u{643}\u{627}"), (0xFC81, "\u{643}\u{644}"),
    (0xFC82, "\u{643}\u{645}"), (0xFC83, "\u{643}\u{649}"), (0xFC84, "\u{643}\u{64a}"),
    (0xFC85, "\u{644}\u{645}"), (0xFC86, "\u{644}\u{649}"), (0xFC87, "\u{644}\u{64a}"),
    (0xFC88, "\u{645}\u{627}"), (0xFC89, "\u{645}\u{645}"), (0xFC8A, "\u{646}\u{631}"),
    (0xFC8B, "\u{646}\u{632}"), (0xFC8C, "\u{646}\u{645}"), (0xFC8D, "\u{646}\u{646}"),
    (0xFC8E, "\u{646}\u{649}"), (0xFC8F, "\u{646}\u{64a}"), (0xFC90, "\u{649}\u{670}"),
    (0xFC91, "\u{64a}\u{631}"), (0xFC92, "\u{64a}\u{632}"), (0xFC93, "\u{64a}\u{645}"),
    (0xFC94, "\u{64a}\u{646}"), (0xFC95, "\u{64a}\u{649}"), (0xFC96, "\u{64a}\u{64a}"),
    (0xFC97, "\u{64a}\u{654}\u{62c}"), (0xFC98, "\u{64a}\u{654}\u{62d}"),
    (0xFC99, "\u{64a}\u{654}\u{62e}"), (0xFC9A, "\u{64a}\u{654}\u{645}"),
    (0xFC9B, "\u{64a}\u{654}\u{647}"), (0xFC9C, "\u{628}\u{62c}"), (0xFC9D, "\u{628}\u{62d}"),
    (0xFC9E, "\u{628}\u{62e}"), (0xFC9F, "\u{628}\u{645}"), (0xFCA0, "\u{628}\u{647}"),
    (0xFCA1, "\u{62a}\u{62c}"), (0xFCA2, "\u{62a}\u{62d}"), (0xFCA3, "\u{62a}\u{62e}"),
    (0xFCA4, "\u{62a}\u{645}"), (0xFCA5, "\u{62a}\u{647}"), (0xFCA6, "\u{62b}\u{645}"),
    (0xFCA7, "\u{62c}\u{62d}"), (0xFCA8, "\u{62c}\u{645}"), (0xFCA9, "\u{62d}\u{62c}"),
    (0xFCAA, "\u{62d}\u{645}"), (0xFCAB, "\u{62e}\u{62c}"), (0xFCAC, "\u{62e}\u{645}"),
    (0xFCAD, "\u{633}\u{62c}"), (0xFCAE, "\u{633}\u{62d}"), (0xFCAF, "\u{633}\u{62e}"),
    (0xFCB0, "\u{633}\u{645}"), (0xFCB1, "\u{635}\u{62d}"), (0xFCB2, "\u{635}\u{62e}"),
    (0xFCB3, "\u{635}\u{645}"), (0xFCB4, "\u{636}\u{62c}"), (0xFCB5, "\u{636}\u{62d}"),
    (0xFCB6, "\u{636}\u{62e}"), (0xFCB7, "\u{636}\u{645}"), (0xFCB8, "\u{637}\u{62d}"),
    (0xFCB9, "\u{638}\u{645}"), (0xFCBA, "\u{639}\u{62c}"), (0xFCBB, "\u{639}\u{645}"),
    (0xFCBC, "\u{63a}\u{62c}"), (0xFCBD, "\u{63a}\u{645}"), (0xFCBE, "\u{641}\u{62c}"),
    (0xFCBF, "\u{641}\u{62d}"), (0xFCC0, "\u{641}\u{62e}"), (0xFCC1, "\u{641}\u{645}"),
    (0xFCC2, "\u{642}\u{62d}"), (0xFCC3, "\u{642}\u{645}"), (0xFCC4, "\u{643}\u{62c}"),
    (0xFCC5, "\u{643}\u{62d}"), (0xFCC6, "\u{643}\u{62e}"), (0xFCC7, "\u{643}\u{644}"),
    (0xFCC8, "\u{643}\u{645}"), (0xFCC9, "\u{644}\u{62c}"), (0xFCCA, "\u{644}\u{62d}"),
    (0xFCCB, "\u{644}\u{62e}"), (0xFCCC, "\u{644}\u{645}"), (0xFCCD, "\u{644}\u{647}"),
    (0xFCCE, "\u{645}\u{62c}"), (0xFCCF, "\u{645}\u{62d}"), (0xFCD0, "\u{645}\u{62e}"),
    (0xFCD1, "\u{645}\u{645}"), (0xFCD2, "\u{646}\u{62c}"), (0xFCD3, "\u{646}\u{62d}"),
    (0xFCD4, "\u{646}\u{62e}"), (0xFCD5, "\u{646}\u{645}"), (0xFCD6, "\u{646}\u{647}"),
    (0xFCD7, "\u{647}\u{62c}"), (0xFCD8, "\u{647}\u{645}"), (0xFCD9, "\u{647}\u{670}"),
    (0xFCDA, "\u{64a}\u{62c}"), (0xFCDB, "\u{64a}\u{62d}"), (0xFCDC, "\u{64a}\u{62e}"),
    (0xFCDD, "\u{64a}\u{645}"), (0xFCDE, "\u{64a}\u{647}"), (0xFCDF, "\u{64a}\u{654}\u{645}"),
    (0xFCE0, "\u{64a}\u{654}\u{647}"), (0xFCE1, "\u{628}\u{645}"), (0xFCE2, "\u{628}\u{647}"),
    (0xFCE3, "\u{62a}\u{645}"), (0xFCE4, "\u{62a}\u{647}"), (0xFCE5, "\u{62b}\u{645}"),
    (0xFCE6, "\u{62b}\u{647}"), (0xFCE7, "\u{633}\u{645}"), (0xFCE8, "\u{633}\u{647}"),
    (0xFCE9, "\u{634}\u{645}"), (0xFCEA, "\u{634}\u{647}"), (0xFCEB, "\u{643}\u{644}"),
    (0xFCEC, "\u{643}\u{645}"), (0xFCED, "\u{644}\u{645}"), (0xFCEE, "\u{646}\u{645}"),
    (0xFCEF, "\u{646}\u{647}"), (0xFCF0, "\u{64a}\u{645}"), (0xFCF1, "\u{64a}\u{647}"),
    (0xFCF2, "\u{640}\u{64e}\u{651}"), (0xFCF3, "\u{640}\u{64f}\u{651}"),
    (0xFCF4, "\u{640}\u{650}\u{651}"), (0xFCF5, "\u{637}\u{649}"), (0xFCF6, "\u{637}\u{64a}"),
    (0xFCF7, "\u{639}\u{649}"), (0xFCF8, "\u{639}\u{64a}"), (0xFCF9, "\u{63a}\u{649}"),
    (0xFCFA, "\u{63a}\u{64a}"), (0xFCFB, "\u{633}\u{649}"), (0xFCFC, "\u{633}\u{64a}"),
    (0xFCFD, "\u{634}\u{649}"), (0xFCFE, "\u{634}\u{64a}"), (0xFCFF, "\u{62d}\u{649}"),
    (0xFD00, "\u{62d}\u{64a}"), (0xFD01, "\u{62c}\u{649}"), (0xFD02, "\u{62c}\u{64a}"),
    (0xFD03, "\u{62e}\u{649}"), (0xFD04, "\u{62e}\u{64a}"), (0xFD05, "\u{635}\u{649}"),
    (0xFD06, "\u{635}\u{64a}"), (0xFD07, "\u{636}\u{649}"), (0xFD08, "\u{636}\u{64a}"),
    (0xFD09, "\u{634}\u{62c}"), (0xFD0A, "\u{634}\u{62d}"), (0xFD0B, "\u{634}\u{62e}"),
    (0xFD0C, "\u{634}\u{645}"), (0xFD0D, "\u{634}\u{631}"), (0xFD0E, "\u{633}\u{631}"),
    (0xFD0F, "\u{635}\u{631}"), (0xFD10, "\u{636}\u{631}"), (0xFD11, "\u{637}\u{649}"),
    (0xFD12, "\u{637}\u{64a}"), (0xFD13, "\u{639}\u{649}"), (0xFD14, "\u{639}\u{64a}"),
    (0xFD15, "\u{63a}\u{649}"), (0xFD16, "\u{63a}\u{64a}"), (0xFD17, "\u{633}\u{649}"),
    (0xFD18, "\u{633}\u{64a}"), (0xFD19, "\u{634}\u{649}"), (0xFD1A, "\u{634}\u{64a}"),
    (0xFD1B, "\u{62d}\u{649}"), (0xFD1C, "\u{62d}\u{64a}"), (0xFD1D, "\u{62c}\u{649}"),
    (0xFD1E, "\u{62c}\u{64a}"), (0xFD1F, "\u{62e}\u{649}"), (0xFD20, "\u{62e}\u{64a}"),
    (0xFD21, "\u{635}\u{649}"), (0xFD22, "\u{635}\u{64a}"), (0xFD23, "\u{636}\u{649}"),
    (0xFD24, "\u{636}\u{64a}"), (0xFD25, "\u{634}\u{62c}"), (0xFD26, "\u{634}\u{62d}"),
    (0xFD27, "\u{634}\u{62e}"), (0xFD28, "\u{634}\u{645}"), (0xFD29, "\u{634}\u{631}"),
    (0xFD2A, "\u{633}\u{631}"), (0xFD2B, "\u{635}\u{631}"), (0xFD2C, "\u{636}\u{631}"),
    (0xFD2D, "\u{634}\u{62c}"), (0xFD2E, "\u{634}\u{62d}"), (0xFD2F, "\u{634}\u{62e}"),
    (0xFD30, "\u{634}\u{645}"), (0xFD31, "\u{633}\u{647}"), (0xFD32, "\u{634}\u{647}"),
    (0xFD33, "\u{637}\u{645}"), (0xFD34, "\u{633}\u{62c}"), (0xFD35, "\u{633}\u{62d}"),
    (0xFD36, "\u{633}\u{62e}"), (0xFD37, "\u{634}\u{62c}"), (0xFD38, "\u{634}\u{62d}"),
    (0xFD39, "\u{634}\u{62e}"), (0xFD3A, "\u{637}\u{645}"), (0xFD3B, "\u{638}\u{645}"),
    (0xFD3C, "\u{627}\u{64b}"), (0xFD3D, "\u{627}\u{64b}"), (0xFD50, "\u{62a}\u{62c}\u{645}"),
    (0xFD51, "\u{62a}\u{62d}\u{62c}"), (0xFD52, "\u{62a}\u{62d}\u{62c}"),
    (0xFD53, "\u{62a}\u{62d}\u{645}"), (0xFD54, "\u{62a}\u{62e}\u{645}"),
    (0xFD55, "\u{62a}\u{645}\u{62c}"), (0xFD56, "\u{62a}\u{645}\u{62d}"),
    (0xFD57, "\u{62a}\u{645}\u{62e}"), (0xFD58, "\u{62c}\u{645}\u{62d}"),
    (0xFD59, "\u{62c}\u{645}\u{62d}"), (0xFD5A, "\u{62d}\u{645}\u{64a}"),
    (0xFD5B, "\u{62d}\u{645}\u{649}"), (0xFD5C, "\u{633}\u{62d}\u{62c}"),
    (0xFD5D, "\u{633}\u{62c}\u{62d}"), (0xFD5E, "\u{633}\u{62c}\u{649}"),
    (0xFD5F, "\u{633}\u{645}\u{62d}"), (0xFD60, "\u{633}\u{645}\u{62d}"),
    (0xFD61, "\u{633}\u{645}\u{62c}"), (0xFD62, "\u{633}\u{645}\u{645}"),
    (0xFD63, "\u{633}\u{645}\u{645}"), (0xFD64, "\u{635}\u{62d}\u{62d}"),
    (0xFD65, "\u{635}\u{62d}\u{62d}"), (0xFD66, "\u{635}\u{645}\u{645}"),
    (0xFD67, "\u{634}\u{62d}\u{645}"), (0xFD68, "\u{634}\u{62d}\u{645}"),
    (0xFD69, "\u{634}\u{62c}\u{64a}"), (0xFD6A, "\u{634}\u{645}\u{62e}"),
    (0xFD6B, "\u{634}\u{645}\u{62e}"), (0xFD6C, "\u{634}\u{645}\u{645}"),
    (0xFD6D, "\u{634}\u{645}\u{645}"), (0xFD6E, "\u{636}\u{62d}\u{649}"),
    (0xFD6F, "\u{636}\u{62e}\u{645}"), (0xFD70, "\u{636}\u{62e}\u{645}"),
    (0xFD71, "\u{637}\u{645}\u{62d}"), (0xFD72, "\u{637}\u{645}\u{62d}"),
    (0xFD73, "\u{637}\u{645}\u{645}"), (0xFD74, "\u{637}\u{645}\u{64a}"),
    (0xFD75, "\u{639}\u{62c}\u{645}"), (0xFD76, "\u{639}\u{645}\u{645}"),
    (0xFD77, "\u{639}\u{645}\u{645}"), (0xFD78, "\u{639}\u{645}\u{649}"),
    (0xFD79, "\u{63a}\u{645}\u{645}"), (0xFD7A, "\u{63a}\u{645}\u{64a}"),
    (0xFD7B, "\u{63a}\u{645}\u{649}"), (0xFD7C, "\u{641}\u{62e}\u{645}"),
    (0xFD7D, "\u{641}\u{62e}\u{645}"), (0xFD7E, "\u{642}\u{645}\u{62d}"),
    (0xFD7F, "\u{642}\u{645}\u{645}"), (0xFD80, "\u{644}\u{62d}\u{645}"),
    (0xFD81, "\u{644}\u{62d}\u{64a}"), (0xFD82, "\u{644}\u{62d}\u{649}"),
    (0xFD83, "\u{644}\u{62c}\u{62c}"), (0xFD84, "\u{644}\u{62c}\u{62c}"),
    (0xFD85, "\u{644}\u{62e}\u{645}"), (0xFD86, "\u{644}\u{62e}\u{645}"),
    (0xFD87, "\u{644}\u{645}\u{62d}"), (0xFD88, "\u{644}\u{645}\u{62d}"),
    (0xFD89, "\u{645}\u{62d}\u{62c}"), (0xFD8A, "\u{645}\u{62d}\u{645}"),
    (0xFD8B, "\u{645}\u{62d}\u{64a}"), (0xFD8C, "\u{645}\u{62c}\u{62d}"),
    (0xFD8D, "\u{645}\u{62c}\u{645}"), (0xFD8E, "\u{645}\u{62e}\u{62c}"),
    (0xFD8F, "\u{645}\u{62e}\u{645}"), (0xFD92, "\u{645}\u{62c}\u{62e}"),
    (0xFD93, "\u{647}\u{645}\u{62c}"), (0xFD94, "\u{647}\u{645}\u{645}"),
    (0xFD95, "\u{646}\u{62d}\u{645}"), (0xFD96, "\u{646}\u{62d}\u{649}"),
    (0xFD97, "\u{646}\u{62c}\u{645}"), (0xFD98, "\u{646}\u{62c}\u{645}"),
    (0xFD99, "\u{646}\u{62c}\u{649}"), (0xFD9A, "\u{646}\u{645}\u{64a}"),
    (0xFD9B, "\u{646}\u{645}\u{649}"), (0xFD9C, "\u{64a}\u{645}\u{645}"),
    (0xFD9D, "\u{64a}\u{645}\u{645}"), (0xFD9E, "\u{628}\u{62e}\u{64a}"),
    (0xFD9F, "\u{62a}\u{62c}\u{64a}"), (0xFDA0, "\u{62a}\u{62c}\u{649}"),
    (0xFDA1, "\u{62a}\u{62e}\u{64a}"), (0xFDA2, "\u{62a}\u{62e}\u{649}"),
    (0xFDA3, "\u{62a}\u{645}\u{64a}"), (0xFDA4, "\u{62a}\u{645}\u{649}"),
    (0xFDA5, "\u{62c}\u{645}\u{64a}"), (0xFDA6, "\u{62c}\u{62d}\u{649}"),
    (0xFDA7, "\u{62c}\u{645}\u{649}"), (0xFDA8, "\u{633}\u{62e}\u{649}"),
    (0xFDA9, "\u{635}\u{62d}\u{64a}"), (0xFDAA, "\u{634}\u{62d}\u{64a}"),
    (0xFDAB, "\u{636}\u{62d}\u{64a}"), (0xFDAC, "\u{644}\u{62c}\u{64a}"),
    (0xFDAD, "\u{644}\u{645}\u{64a}"), (0xFDAE, "\u{64a}\u{62d}\u{64a}"),
    (0xFDAF, "\u{64a}\u{62c}\u{64a}"), (0xFDB0, "\u{64a}\u{645}\u{64a}"),
    (0xFDB1, "\u{645}\u{645}\u{64a}"), (0xFDB2, "\u{642}\u{645}\u{64a}"),
    (0xFDB3, "\u{646}\u{62d}\u{64a}"), (0xFDB4, "\u{642}\u{645}\u{62d}"),
    (0xFDB5, "\u{644}\u{62d}\u{645}"), (0xFDB6, "\u{639}\u{645}\u{64a}"),
    (0xFDB7, "\u{643}\u{645}\u{64a}"), (0xFDB8, "\u{646}\u{62c}\u{62d}"),
    (0xFDB9, "\u{645}\u{62e}\u{64a}"), (0xFDBA, "\u{644}\u{62c}\u{645}"),
    (0xFDBB, "\u{643}\u{645}\u{645}"), (0xFDBC, "\u{644}\u{62c}\u{645}"),
    (0xFDBD, "\u{646}\u{62c}\u{62d}"), (0xFDBE, "\u{62c}\u{62d}\u{64a}"),
    (0xFDBF, "\u{62d}\u{62c}\u{64a}"), (0xFDC0, "\u{645}\u{62c}\u{64a}"),
    (0xFDC1, "\u{641}\u{645}\u{64a}"), (0xFDC2, "\u{628}\u{62d}\u{64a}"),
    (0xFDC3, "\u{643}\u{645}\u{645}"), (0xFDC4, "\u{639}\u{62c}\u{645}"),
    (0xFDC5, "\u{635}\u{645}\u{645}"), (0xFDC6, "\u{633}\u{62e}\u{64a}"),
    (0xFDC7, "\u{646}\u{62c}\u{64a}"), (0xFDF0, "\u{635}\u{644}\u{6d2}"),
    (0xFDF1, "\u{642}\u{644}\u{6d2}"), (0xFDF2, "\u{627}\u{644}\u{644}\u{647}"),
    (0xFDF3, "\u{627}\u{643}\u{628}\u{631}"), (0xFDF4, "\u{645}\u{62d}\u{645}\u{62f}"),
    (0xFDF5, "\u{635}\u{644}\u{639}\u{645}"), (0xFDF6, "\u{631}\u{633}\u{648}\u{644}"),
    (0xFDF7, "\u{639}\u{644}\u{64a}\u{647}"), (0xFDF8, "\u{648}\u{633}\u{644}\u{645}"),
    (0xFDF9, "\u{635}\u{644}\u{649}"),
    (0xFDFA, "\u{635}\u{644}\u{649} \u{627}\u{644}\u{644}\u{647} \u{639}\u{644}\u{64a}\u{647} \u{648}\u{633}\u{644}\u{645}"),
    (0xFDFB, "\u{62c}\u{644} \u{62c}\u{644}\u{627}\u{644}\u{647}"),
    (0xFDFC, "\u{631}\u{6cc}\u{627}\u{644}"), (0xFE10, ","), (0xFE11, "\u{3001}"),
    (0xFE12, "\u{3002}"), (0xFE13, ":"), (0xFE14, ";"), (0xFE15, "!"), (0xFE16, "?"),
    (0xFE17, "\u{3016}"), (0xFE18, "\u{3017}"), (0xFE19, "..."), (0xFE30, ".."),
    (0xFE31, "\u{2014}"), (0xFE32, "\u{2013}"), (0xFE33, "_"), (0xFE34, "_"), (0xFE35, "("),
    (0xFE36, ")"), (0xFE37, "{"), (0xFE38, "}"), (0xFE39, "\u{3014}"), (0xFE3A, "\u{3015}"),
    (0xFE3B, "\u{3010}"), (0xFE3C, "\u{3011}"), (0xFE3D, "\u{300a}"), (0xFE3E, "\u{300b}"),
    (0xFE3F, "\u{3008}"), (0xFE40, "\u{3009}"), (0xFE41, "\u{300c}"), (0xFE42, "\u{300d}"),
    (0xFE43, "\u{300e}"), (0xFE44, "\u{300f}"), (0xFE47, "["), (0xFE48, "]"),
    (0xFE49, " \u{305}"), (0xFE4A, " \u{305}"), (0xFE4B, " \u{305}"), (0xFE4C, " \u{305}"),
    (0xFE4D, "_"), (0xFE4E, "_"), (0xFE4F, "_"), (0xFE50, ","), (0xFE51, "\u{3001}"),
    (0xFE52, "."), (0xFE54, ";"), (0xFE55, ":"), (0xFE56, "?"), (0xFE57, "!"),
    (0xFE58, "\u{2014}"), (0xFE59, "("), (0xFE5A, ")"), (0xFE5B, "{"), (0xFE5C, "}"),
    (0xFE5D, "\u{3014}"), (0xFE5E, "\u{3015}"), (0xFE5F, "#"), (0xFE60, "&"), (0xFE61, "*"),
    (0xFE62, "+"), (0xFE63, "-"), (0xFE64, "<"), (0xFE65, ">"), (0xFE66, "="),
    (0xFE68, "\u{5c}"), (0xFE69, "$"), (0xFE6A, "%"), (0xFE6B, "@"), (0xFE70, " \u{64b}"),
    (0xFE71, "\u{640}\u{64b}"), (0xFE72, " \u{64c}"), (0xFE74, " \u{64d}"),
    (0xFE76, " \u{64e}"), (0xFE77, "\u{640}\u{64e}"), (0xFE78, " \u{64f}"),
    (0xFE79, "\u{640}\u{64f}"), (0xFE7A, " \u{650}"), (0xFE7B, "\u{640}\u{650}"),
    (0xFE7C, " \u{651}"), (0xFE7D, "\u{640}\u{651}"), (0xFE7E, " \u{652}"),
    (0xFE7F, "\u{640}\u{652}"), (0xFE80, "\u{621}"), (0xFE81, "\u{627}\u{653}"),
    (0xFE82, "\u{627}\u{653}"), (0xFE83, "\u{627}\u{654}"), (0xFE84, "\u{627}\u{654}"),
    (0xFE85, "\u{648}\u{654}"), (0xFE86, "\u{648}\u{654}"), (0xFE87, "\u{627}\u{655}"),
    (0xFE88, "\u{627}\u{655}"), (0xFE89, "\u{64a}\u{654}"), (0xFE8A, "\u{64a}\u{654}"),
    (0xFE8B, "\u{64a}\u{654}"), (0xFE8C, "\u{64a}\u{654}"), (0xFE8D, "\u{627}"),
    (0xFE8E, "\u{627}"), (0xFE8F, "\u{628}"), (0xFE90, "\u{628}"), (0xFE91, "\u{628}"),
    (0xFE92, "\u{628}"), (0xFE93, "\u{629}"), (0xFE94, "\u{629}"), (0xFE95, "\u{62a}"),
    (0xFE96, "\u{62a}"), (0xFE97, "\u{62a}"), (0xFE98, "\u{62a}"), (0xFE99, "\u{62b}"),
    (0xFE9A, "\u{62b}"), (0xFE9B, "\u{62b}"), (0xFE9C, "\u{62b}"), (0xFE9D, "\u{62c}"),
    (0xFE9E, "\u{62c}"), (0xFE9F, "\u{62c}"), (0xFEA0, "\u{62c}"), (0xFEA1, "\u{62d}"),
    (0xFEA2, "\u{62d}"), (0xFEA3, "\u{62d}"), (0xFEA4, "\u{62d}"), (0xFEA5, "\u{62e}"),
    (0xFEA6, "\u{62e}"), (0xFEA7, "\u{62e}"), (0xFEA8, "\u{62e}"), (0xFEA9, "\u{62f}"),
    (0xFEAA, "\u{62f}"), (0xFEAB, "\u{630}"), (0xFEAC, "\u{630}"), (0xFEAD, "\u{631}"),
    (0xFEAE, "\u{631}"), (0xFEAF, "\u{632}"), (0xFEB0, "\u{632}"), (0xFEB1, "\u{633}"),
    (0xFEB2, "\u{633}"), (0xFEB3, "\u{633}"), (0xFEB4, "\u{633}"), (0xFEB5, "\u{634}"),
    (0xFEB6, "\u{634}"), (0xFEB7, "\u{634}"), (0xFEB8, "\u{634}"), (0xFEB9, "\u{635}"),
    (0xFEBA, "\u{635}"), (0xFEBB, "\u{635}"), (0xFEBC, "\u{635}"), (0xFEBD, "\u{636}"),
    (0xFEBE, "\u{636}"), (0xFEBF, "\u{636}"), (0xFEC0, "\u{636}"), (0xFEC1, "\u{637}"),
    (0xFEC2, "\u{637}"), (0xFEC3, "\u{637}"), (0xFEC4, "\u{637}"), (0xFEC5, "\u{638}"),
    (0xFEC6, "\u{638}"), (0xFEC7, "\u{638}"), (0xFEC8, "\u{638}"), (0xFEC9, "\u{639}"),
    (0xFECA, "\u{639}"), (0xFECB, "\u{639}"), (0xFECC, "\u{639}"), (0xFECD, "\u{63a}"),
    (0xFECE, "\u{63a}"), (0xFECF, "\u{63a}"), (0xFED0, "\u{63a}"), (0xFED1, "\u{641}"),
    (0xFED2, "\u{641}"), (0xFED3, "\u{641}"), (0xFED4, "\u{641}"), (0xFED5, "\u{642}"),
    (0xFED6, "\u{642}"), (0xFED7, "\u{642}"), (0xFED8, "\u{642}"), (0xFED9, "\u{643}"),
    (0xFEDA, "\u{643}"), (0xFEDB, "\u{643}"), (0xFEDC, "\u{643}"), (0xFEDD, "\u{644}"),
    (0xFEDE, "\u{644}"), (0xFEDF, "\u{644}"), (0xFEE0, "\u{644}"), (0xFEE1, "\u{645}"),
    (0xFEE2, "\u{645}"), (0xFEE3, "\u{645}"), (0xFEE4, "\u{645}"), (0xFEE5, "\u{646}"),
    (0xFEE6, "\u{646}"), (0xFEE7, "\u{646}"), (0xFEE8, "\u{646}"), (0xFEE9, "\u{647}"),
    (0xFEEA, "\u{647}"), (0xFEEB, "\u{647}"), (0xFEEC, "\u{647}"), (0xFEED, "\u{648}"),
    (0xFEEE, "\u{648}"), (0xFEEF, "\u{649}"), (0xFEF0, "\u{649}"), (0xFEF1, "\u{64a}"),
    (0xFEF2, "\u{64a}"), (0xFEF3, "\u{64a}"), (0xFEF4, "\u{64a}"),
    (0xFEF5, "\u{644}\u{627}\u{653}"), (0xFEF6, "\u{644}\u{627}\u{653}"),
    (0xFEF7, "\u{644}\u{627}\u{654}"), (0xFEF8, "\u{644}\u{627}\u{654}"),
    (0xFEF9, "\u{644}\u{627}\u{655}"), (0xFEFA, "\u{644}\u{627}\u{655}"),
    (0xFEFB, "\u{644}\u{627}"), (0xFEFC, "\u{644}\u{627}"), (0xFF01, "!"), (0xFF02, "\u{22}"),
    (0xFF03, "#"), (0xFF04, "$"), (0xFF05, "%"), (0xFF06, "&"), (0xFF07, "'"), (0xFF08, "("),
    (0xFF09, ")"), (0xFF0A, "*"), (0xFF0B, "+"), (0xFF0C, ","), (0xFF0D, "-"), (0xFF0E, "."),
    (0xFF0F, "/"), (0xFF10, "0"), (0xFF11, "1"), (0xFF12, "2"), (0xFF13, "3"), (0xFF14, "4"),
    (0xFF15, "5"), (0xFF16, "6"), (0xFF17, "7"), (0xFF18, "8"), (0xFF19, "9"), (0xFF1A, ":"),
    (0xFF1B, ";"), (0xFF1C, "<"), (0xFF1D, "="), (0xFF1E, ">"), (0xFF1F, "?"), (0xFF20, "@"),
    (0xFF21, "A"), (0xFF22, "B"), (0xFF23, "C"), (0xFF24, "D"), (0xFF25, "E"), (0xFF26, "F"),
    (0xFF27, "G"), (0xFF28, "H"), (0xFF29, "I"), (0xFF2A, "J"), (0xFF2B, "K"), (0xFF2C, "L"),
    (0xFF2D, "M"), (0xFF2E, "N"), (0xFF2F, "O"), (0xFF30, "P"), (0xFF31, "Q"), (0xFF32, "R"),
    (0xFF33, "S"), (0xFF34, "T"), (0xFF35, "U"), (0xFF36, "V"), (0xFF37, "W"), (0xFF38, "X"),
    (0xFF39, "Y"), (0xFF3A, "Z"), (0xFF3B, "["), (0xFF3C, "\u{5c}"), (0xFF3D, "]"),
    (0xFF3E, "^"), (0xFF3F, "_"), (0xFF40, "`"), (0xFF41, "a"), (0xFF42, "b"), (0xFF43, "c"),
    (0xFF44, "d"), (0xFF45, "e"), (0xFF46, "f"), (0xFF47, "g"), (0xFF48, "h"), (0xFF49, "i"),
    (0xFF4A, "j"), (0xFF4B, "k"), (0xFF4C, "l"), (0xFF4D, "m"), (0xFF4E, "n"), (0xFF4F, "o"),
    (0xFF50, "p"), (0xFF51, "q"), (0xFF52, "r"), (0xFF53, "s"), (0xFF54, "t"), (0xFF55, "u"),
    (0xFF56, "v"), (0xFF57, "w"), (0xFF58, "x"), (0xFF59, "y"), (0xFF5A, "z"), (0xFF5B, "{"),
    (0xFF5C, "|"), (0xFF5D, "}"), (0xFF5E, "~"), (0xFF5F, "\u{2985}"), (0xFF60, "\u{2986}"),
    (0xFF61, "\u{3002}"), (0xFF62, "\u{300c}"), (0xFF63, "\u{300d}"), (0xFF64, "\u{3001}"),
    (0xFF65, "\u{30fb}"), (0xFF66, "\u{30f2}"), (0xFF67, "\u{30a1}"), (0xFF68, "\u{30a3}"),
    (0xFF69, "\u{30a5}"), (0xFF6A, "\u{30a7}"), (0xFF6B, "\u{30a9}"), (0xFF6C, "\u{30e3}"),
    (0xFF6D, "\u{30e5}"), (0xFF6E, "\u{30e7}"), (0xFF6F, "\u{30c3}"), (0xFF70, "\u{30fc}"),
    (0xFF71, "\u{30a2}"), (0xFF72, "\u{30a4}"), (0xFF73, "\u{30a6}"), (0xFF74, "\u{30a8}"),
    (0xFF75, "\u{30aa}"), (0xFF76, "\u{30ab}"), (0xFF77, "\u{30ad}"), (0xFF78, "\u{30af}"),
    (0xFF79, "\u{30b1}"), (0xFF7A, "\u{30b3}"), (0xFF7B, "\u{30b5}"), (0xFF7C, "\u{30b7}"),
    (0xFF7D, "\u{30b9}"), (0xFF7E, "\u{30bb}"), (0xFF7F, "\u{30bd}"), (0xFF80, "\u{30bf}"),
    (0xFF81, "\u{30c1}"), (0xFF82, "\u{30c4}"), (0xFF83, "\u{30c6}"), (0xFF84, "\u{30c8}"),
    (0xFF85, "\u{30ca}"), (0xFF86, "\u{30cb}"), (0xFF87, "\u{30cc}"), (0xFF88, "\u{30cd}"),
    (0xFF89, "\u{30ce}"), (0xFF8A, "\u{30cf}"), (0xFF8B, "\u{30d2}"), (0xFF8C, "\u{30d5}"),
    (0xFF8D, "\u{30d8}"), (0xFF8E, "\u{30db}"), (0xFF8F, "\u{30de}"), (0xFF90, "\u{30df}"),
    (0xFF91, "\u{30e0}"), (0xFF92, "\u{30e1}"), (0xFF93, "\u{30e2}"), (0xFF94, "\u{30e4}"),
    (0xFF95, "\u{30e6}"), (0xFF96, "\u{30e8}"), (0xFF97, "\u{30e9}"), (0xFF98, "\u{30ea}"),
    (0xFF99, "\u{30eb}"), (0xFF9A, "\u{30ec}"), (0xFF9B, "\u{30ed}"), (0xFF9C, "\u{30ef}"),
    (0xFF9D, "\u{30f3}"), (0xFF9E, "\u{3099}"), (0xFF9F, "\u{309a}"), (0xFFA0, "\u{1160}"),
    (0xFFA1, "\u{1100}"), (0xFFA2, "\u{1101}"), (0xFFA3, "\u{11aa}"), (0xFFA4, "\u{1102}"),
    (0xFFA5, "\u{11ac}"), (0xFFA6, "\u{11ad}"), (0xFFA7, "\u{1103}"), (0xFFA8, "\u{1104}"),
    (0xFFA9, "\u{1105}"), (0xFFAA, "\u{11b0}"), (0xFFAB, "\u{11b1}"), (0xFFAC, "\u{11b2}"),
    (0xFFAD, "\u{11b3}"), (0xFFAE, "\u{11b4}"), (0xFFAF, "\u{11b5}"), (0xFFB0, "\u{111a}"),
    (0xFFB1, "\u{1106}"), (0xFFB2, "\u{1107}"), (0xFFB3, "\u{1108}"), (0xFFB4, "\u{1121}"),
    (0xFFB5, "\u{1109}"), (0xFFB6, "\u{110a}"), (0xFFB7, "\u{110b}"), (0xFFB8, "\u{110c}"),
    (0xFFB9, "\u{110d}"), (0xFFBA, "\u{110e}"), (0xFFBB, "\u{110f}"), (0xFFBC, "\u{1110}"),
    (0xFFBD, "\u{1111}"), (0xFFBE, "\u{1112}"), (0xFFC2, "\u{1161}"), (0xFFC3, "\u{1162}"),
    (0xFFC4, "\u{1163}"), (0xFFC5, "\u{1164}"), (0xFFC6, "\u{1165}"), (0xFFC7, "\u{1166}"),
    (0xFFCA, "\u{1167}"), (0xFFCB, "\u{1168}"), (0xFFCC, "\u{1169}"), (0xFFCD, "\u{116a}"),
    (0xFFCE, "\u{116b}"), (0xFFCF, "\u{116c}"), (0xFFD2, "\u{116d}"), (0xFFD3, "\u{116e}"),
    (0xFFD4, "\u{116f}"), (0xFFD5, "\u{1170}"), (0xFFD6, "\u{1171}"), (0xFFD7, "\u{1172}"),
    (0xFFDA, "\u{1173}"), (0xFFDB, "\u{1174}"), (0xFFDC, "\u{1175}"), (0xFFE0, "\u{a2}"),
    (0xFFE1, "\u{a3}"), (0xFFE2, "\u{ac}"), (0xFFE3, " \u{304}"), (0xFFE4, "\u{a6}"),
    (0xFFE5, "\u{a5}"), (0xFFE6, "\u{20a9}"), (0xFFE8, "\u{2502}"), (0xFFE9, "\u{2190}"),
    (0xFFEA, "\u{2191}"), (0xFFEB, "\u{2192}"), (0xFFEC, "\u{2193}"), (0xFFED, "\u{25a0}"),
    (0xFFEE, "\u{25cb}"), (0x10781, "\u{2d0}"), (0x10782, "\u{2d1}"), (0x10783, "\u{e6}"),
    (0x10784, "\u{299}"), (0x10785, "\u{253}"), (0x10787, "\u{2a3}"), (0x10788, "\u{ab66}"),
    (0x10789, "\u{2a5}"), (0x1078A, "\u{2a4}"), (0x1078B, "\u{256}"), (0x1078C, "\u{257}"),
    (0x1078D, "\u{1d91}"), (0x1078E, "\u{258}"), (0x1078F, "\u{25e}"), (0x10790, "\u{2a9}"),
    (0x10791, "\u{264}"), (0x10792, "\u{262}"), (0x10793, "\u{260}"), (0x10794, "\u{29b}"),
    (0x10795, "\u{127}"), (0x10796, "\u{29c}"), (0x10797, "\u{267}"), (0x10798, "\u{284}"),
    (0x10799, "\u{2aa}"), (0x1079A, "\u{2ab}"), (0x1079B, "\u{26c}"), (0x1079C, "\u{1df04}"),
    (0x1079D, "\u{a78e}"), (0x1079E, "\u{26e}"), (0x1079F, "\u{1df05}"), (0x107A0, "\u{28e}"),
    (0x107A1, "\u{1df06}"), (0x107A2, "\u{f8}"), (0x107A3, "\u{276}"), (0x107A4, "\u{277}"),
    (0x107A5, "q"), (0x107A6, "\u{27a}"), (0x107A7, "\u{1df08}"), (0x107A8, "\u{27d}"),
    (0x107A9, "\u{27e}"), (0x107AA, "\u{280}"), (0x107AB, "\u{2a8}"), (0x107AC, "\u{2a6}"),
    (0x107AD, "\u{ab67}"), (0x107AE, "\u{2a7}"), (0x107AF, "\u{288}"), (0x107B0, "\u{2c71}"),
    (0x107B2, "\u{28f}"), (0x107B3, "\u{2a1}"), (0x107B4, "\u{2a2}"), (0x107B5, "\u{298}"),
    (0x107B6, "\u{1c0}"), (0x107B7, "\u{1c1}"), (0x107B8, "\u{1c2}"), (0x107B9, "\u{1df0a}"),
    (0x107BA, "\u{1df1e}"), (0x1109A, "\u{11099}\u{110ba}"), (0x1109C, "\u{1109b}\u{110ba}"),
    (0x110AB, "\u{110a5}\u{110ba}"), (0x1112E, "\u{11131}\u{11127}"),
    (0x1112F, "\u{11132}\u{11127}"), (0x1134B, "\u{11347}\u{1133e}"),
    (0x1134C, "\u{11347}\u{11357}"), (0x114BB, "\u{114b9}\u{114ba}"),
    (0x114BC, "\u{114b9}\u{114b0}"), (0x114BE, "\u{114b9}\u{114bd}"),
    (0x115BA, "\u{115b8}\u{115af}"), (0x115BB, "\u{115b9}\u{115af}"),
    (0x11938, "\u{11935}\u{11930}"), (0x1D15E, "\u{1d157}\u{1d165}"),
    (0x1D15F, "\u{1d158}\u{1d165}"), (0x1D160, "\u{1d158}\u{1d165}\u{1d16e}"),
    (0x1D161, "\u{1d158}\u{1d165}\u{1d16f}"), (0x1D162, "\u{1d158}\u{1d165}\u{1d170}"),
    (0x1D163, "\u{1d158}\u{1d165}\u{1d171}"), (0x1D164, "\u{1d158}\u{1d165}\u{1d172}"),
    (0x1D1BB, "\u{1d1b9}\u{1d165}"), (0x1D1BC, "\u{1d1ba}\u{1d165}"),
    (0x1D1BD, "\u{1d1b9}\u{1d165}\u{1d16e}"), (0x1D1BE, "\u{1d1ba}\u{1d165}\u{1d16e}"),
    (0x1D1BF, "\u{1d1b9}\u{1d165}\u{1d16f}"), (0x1D1C0, "\u{1d1ba}\u{1d165}\u{1d16f}"),
    (0x1D400, "A"), (0x1D401, "B"), (0x1D402, "C"), (0x1D403, "D"), (0x1D404, "E"),
    (0x1D405, "F"), (0x1D406, "G"), (0x1D407, "H"), (0x1D408, "I"), (0x1D409, "J"),
    (0x1D40A, "K"), (0x1D40B, "L"), (0x1D40C, "M"), (0x1D40D, "N"), (0x1D40E, "O"),
    (0x1D40F, "P"), (0x1D410, "Q"), (0x1D411, "R"), (0x1D412, "S"), (0x1D413, "T"),
    (0x1D414, "U"), (0x1D415, "V"), (0x1D416, "W"), (0x1D417, "X"), (0x1D418, "Y"),
    (0x1D419, "Z"), (0x1D41A, "a"), (0x1D41B, "b"), (0x1D41C, "c"), (0x1D41D, "d"),
    (0x1D41E, "e"), (0x1D41F, "f"), (0x1D420, "g"), (0x1D421, "h"), (0x1D422, "i"),
    (0x1D423, "j"), (0x1D424, "k"), (0x1D425, "l"), (0x1D426, "m"), (0x1D427, "n"),
    (0x1D428, "o"), (0x1D429, "p"), (0x1D42A, "q"), (0x1D42B, "r"), (0x1D42C, "s"),
    (0x1D42D, "t"), (0x1D42E, "u"), (0x1D42F, "v"), (0x1D430, "w"), (0x1D431, "x"),
    (0x1D432, "y"), (0x1D433, "z"), (0x1D434, "A"), (0x1D435, "B"), (0x1D436, "C"),
    (0x1D437, "D"), (0x1D438, "E"), (0x1D439, "F"), (0x1D43A, "G"), (0x1D43B, "H"),
    (0x1D43C, "I"), (0x1D43D, "J"), (0x1D43E, "K"), (0x1D43F, "L"), (0x1D440, "M"),
    (0x1D441, "N"), (0x1D442, "O"), (0x1D443, "P"), (0x1D444, "Q"), (0x1D445, "R"),
    (0x1D446, "S"), (0x1D447, "T"), (0x1D448, "U"), (0x1D449, "V"), (0x1D44A, "W"),
    (0x1D44B, "X"), (0x1D44C, "Y"), (0x1D44D, "Z"), (0x1D44E, "a"), (0x1D44F, "b"),
    (0x1D450, "c"), (0x1D451, "d"), (0x1D452, "e"), (0x1D453, "f"), (0x1D454, "g"),
    (0x1D456, "i"), (0x1D457, "j"), (0x1D458, "k"), (0x1D459, "l"), (0x1D45A, "m"),
    (0x1D45B, "n"), (0x1D45C, "o"), (0x1D45D, "p"), (0x1D45E, "q"), (0x1D45F, "r"),
    (0x1D460, "s"), (0x1D461, "t"), (0x1D462, "u"), (0x1D463, "v"), (0x1D464, "w"),
    (0x1D465, "x"), (0x1D466, "y"), (0x1D467, "z"), (0x1D468, "A"), (0x1D469, "B"),
    (0x1D46A, "C"), (0x1D46B, "D"), (0x1D46C, "E"), (0x1D46D, "F"), (0x1D46E, "G"),
    (0x1D46F, "H"), (0x1D470, "I"), (0x1D471, "J"), (0x1D472, "K"), (0x1D473, "L"),
    (0x1D474, "M"), (0x1D475, "N"), (0x1D476, "O"), (0x1D477, "P"), (0x1D478, "Q"),
    (0x1D479, "R"), (0x1D47A, "S"), (0x1D47B, "T"), (0x1D47C, "U"), (0x1D47D, "V"),
    (0x1D47E, "W"), (0x1D47F, "X"), (0x1D480, "Y"), (0x1D481, "Z"), (0x1D482, "a"),
    (0x1D483, "b"), (0x1D484, "c"), (0x1D485, "d"), (0x1D486, "e"), (0x1D487, "f"),
    (0x1D488, "g"), (0x1D489, "h"), (0x1D48A, "i"), (0x1D48B, "j"), (0x1D48C, "k"),
    (0x1D48D, "l"), (0x1D48E, "m"), (0x1D48F, "n"), (0x1D490, "o"), (0x1D491, "p"),
    (0x1D492, "q"), (0x1D493, "r"), (0x1D494, "s"), (0x1D495, "t"), (0x1D496, "u"),
    (0x1D497, "v"), (0x1D498, "w"), (0x1D499, "x"), (0x1D49A, "y"), (0x1D49B, "z"),
    (0x1D49C, "A"), (0x1D49E, "C"), (0x1D49F, "D"), (0x1D4A2, "G"), (0x1D4A5, "J"),
    (0x1D4A6, "K"), (0x1D4A9, "N"), (0x1D4AA, "O"), (0x1D4AB, "P"), (0x1D4AC, "Q"),
    (0x1D4AE, "S"), (0x1D4AF, "T"), (0x1D4B0, "U"), (0x1D4B1, "V"), (0x1D4B2, "W"),
    (0x1D4B3, "X"), (0x1D4B4, "Y"), (0x1D4B5, "Z"), (0x1D4B6, "a"), (0x1D4B7, "b"),
    (0x1D4B8, "c"), (0x1D4B9, "d"), (0x1D4BB, "f"), (0x1D4BD, "h"), (0x1D4BE, "i"),
    (0x1D4BF, "j"), (0x1D4C0, "k"), (0x1D4C1, "l"), (0x1D4C2, "m"), (0x1D4C3, "n"),
    (0x1D4C5, "p"), (0x1D4C6, "q"), (0x1D4C7, "r"), (0x1D4C8, "s"), (0x1D4C9, "t"),
    (0x1D4CA, "u"), (0x1D4CB, "v"), (0x1D4CC, "w"), (0x1D4CD, "x"), (0x1D4CE, "y"),
    (0x1D4CF, "z"), (0x1D4D0, "A"), (0x1D4D1, "B"), (0x1D4D2, "C"), (0x1D4D3, "D"),
    (0x1D4D4, "E"), (0x1D4D5, "F"), (0x1D4D6, "G"), (0x1D4D7, "H"), (0x1D4D8, "I"),
    (0x1D4D9, "J"), (0x1D4DA, "K"), (0x1D4DB, "L"), (0x1D4DC, "M"), (0x1D4DD, "N"),
    (0x1D4DE, "O"), (0x1D4DF, "P"), (0x1D4E0, "Q"), (0x1D4E1, "R"), (0x1D4E2, "S"),
    (0x1D4E3, "T"), (0x1D4E4, "U"), (0x1D4E5, "V"), (0x1D4E6, "W"), (0x1D4E7, "X"),
    (0x1D4E8, "Y"), (0x1D4E9, "Z"), (0x1D4EA, "a"), (0x1D4EB, "b"), (0x1D4EC, "c"),
    (0x1D4ED, "d"), (0x1D4EE, "e"), (0x1D4EF, "f"), (0x1D4F0, "g"), (0x1D4F1, "h"),
    (0x1D4F2, "i"), (0x1D4F3, "j"), (0x1D4F4, "k"), (0x1D4F5, "l"), (0x1D4F6, "m"),
    (0x1D4F7, "n"), (0x1D4F8, "o"), (0x1D4F9, "p"), (0x1D4FA, "q"), (0x1D4FB, "r"),
    (0x1D4FC, "s"), (0x1D4FD, "t"), (0x1D4FE, "u"), (0x1D4FF, "v"), (0x1D500, "w"),
    (0x1D501, "x"), (0x1D502, "y"), (0x1D503, "z"), (0x1D504, "A"), (0x1D505, "B"),
    (0x1D507, "D"), (0x1D508, "E"), (0x1D509, "F"), (0x1D50A, "G"), (0x1D50D, "J"),
    (0x1D50E, "K"), (0x1D50F, "L"), (0x1D510, "M"), (0x1D511, "N"), (0x1D512, "O"),
    (0x1D513, "P"), (0x1D514, "Q"), (0x1D516, "S"), (0x1D517, "T"), (0x1D518, "U"),
    (0x1D519, "V"), (0x1D51A, "W"), (0x1D51B, "X"), (0x1D51C, "Y"), (0x1D51E, "a"),
    (0x1D51F, "b"), (0x1D520, "c"), (0x1D521, "d"), (0x1D522, "e"), (0x1D523, "f"),
    (0x1D524, "g"), (0x1D525, "h"), (0x1D526, "i"), (0x1D527, "j"), (0x1D528, "k"),
    (0x1D529, "l"), (0x1D52A, "m"), (0x1D52B, "n"), (0x1D52C, "o"), (0x1D52D, "p"),
    (0x1D52E, "q"), (0x1D52F, "r"), (0x1D530, "s"), (0x1D531, "t"), (0x1D532, "u"),
    (0x1D533, "v"), (0x1D534, "w"), (0x1D535, "x"), (0x1D536, "y"), (0x1D537, "z"),
    (0x1D538, "A"), (0x1D539, "B"), (0x1D53B, "D"), (0x1D53C, "E"), (0x1D53D, "F"),
    (0x1D53E, "G"), (0x1D540, "I"), (0x1D541, "J"), (0x1D542, "K"), (0x1D543, "L"),
    (0x1D544, "M"), (0x1D546, "O"), (0x1D54A, "S"), (0x1D54B, "T"), (0x1D54C, "U"),
    (0x1D54D, "V"), (0x1D54E, "W"), (0x1D54F, "X"), (0x1D550, "Y"), (0x1D552, "a"),
    (0x1D553, "b"), (0x1D554, "c"), (0x1D555, "d"), (0x1D556, "e"), (0x1D557, "f"),
    (0x1D558, "g"), (0x1D559, "h"), (0x1D55A, "i"), (0x1D55B, "j"), (0x1D55C, "k"),
    (0x1D55D, "l"), (0x1D55E, "m"), (0x1D55F, "n"), (0x1D560, "o"), (0x1D561, "p"),
    (0x1D562, "q"), (0x1D563, "r"), (0x1D564, "s"), (0x1D565, "t"), (0x1D566, "u"),
    (0x1D567, "v"), (0x1D568, "w"), (0x1D569, "x"), (0x1D56A, "y"), (0x1D56B, "z"),
    (0x1D56C, "A"), (0x1D56D, "B"), (0x1D56E, "C"), (0x1D56F, "D"), (0x1D570, "E"),
    (0x1D571, "F"), (0x1D572, "G"), (0x1D573, "H"), (0x1D574, "I"), (0x1D575, "J"),
    (0x1D576, "K"), (0x1D577, "L"), (0x1D578, "M"), (0x1D579, "N"), (0x1D57A, "O"),
    (0x1D57B, "P"), (0x1D57C, "Q"), (0x1D57D, "R"), (0x1D57E, "S"), (0x1D57F, "T"),
    (0x1D580, "U"), (0x1D581, "V"), (0x1D582, "W"), (0x1D583, "X"), (0x1D584, "Y"),
    (0x1D585, "Z"), (0x1D586, "a"), (0x1D587, "b"), (0x1D588, "c"), (0x1D589, "d"),
    (0x1D58A, "e"), (0x1D58B, "f"), (0x1D58C, "g"), (0x1D58D, "h"), (0x1D58E, "i"),
    (0x1D58F, "j"), (0x1D590, "k"), (0x1D591, "l"), (0x1D592, "m"), (0x1D593, "n"),
    (0x1D594, "o"), (0x1D595, "p"), (0x1D596, "q"), (0x1D597, "r"), (0x1D598, "s"),
    (0x1D599, "t"), (0x1D59A, "u"), (0x1D59B, "v"), (0x1D59C, "w"), (0x1D59D, "x"),
    (0x1D59E, "y"), (0x1D59F, "z"), (0x1D5A0, "A"), (0x1D5A1, "B"), (0x1D5A2, "C"),
    (0x1D5A3, "D"), (0x1D5A4, "E"), (0x1D5A5, "F"), (0x1D5A6, "G"), (0x1D5A7, "H"),
    (0x1D5A8, "I"), (0x1D5A9, "J"), (0x1D5AA, "K"), (0x1D5AB, "L"), (0x1D5AC, "M"),
    (0x1D5AD, "N"), (0x1D5AE, "O"), (0x1D5AF, "P"), (0x1D5B0, "Q"), (0x1D5B1, "R"),
    (0x1D5B2, "S"), (0x1D5B3, "T"), (0x1D5B4, "U"), (0x1D5B5, "V"), (0x1D5B6, "W"),
    (0x1D5B7, "X"), (0x1D5B8, "Y"), (0x1D5B9, "Z"), (0x1D5BA, "a"), (0x1D5BB, "b"),
    (0x1D5BC, "c"), (0x1D5BD, "d"), (0x1D5BE, "e"), (0x1D5BF, "f"), (0x1D5C0, "g"),
    (0x1D5C1, "h"), (0x1D5C2, "i"), (0x1D5C3, "j"), (0x1D5C4, "k"), (0x1D5C5, "l"),
    (0x1D5C6, "m"), (0x1D5C7, "n"), (0x1D5C8, "o"), (0x1D5C9, "p"), (0x1D5CA, "q"),
    (0x1D5CB, "r"), (0x1D5CC, "s"), (0x1D5CD, "t"), (0x1D5CE, "u"), (0x1D5CF, "v"),
    (0x1D5D0, "w"), (0x1D5D1, "x"), (0x1D5D2, "y"), (0x1D5D3, "z"), (0x1D5D4, "A"),
    (0x1D5D5, "B"), (0x1D5D6, "C"), (0x1D5D7, "D"), (0x1D5D8, "E"), (0x1D5D9, "F"),
    (0x1D5DA, "G"), (0x1D5DB, "H"), (0x1D5DC, "I"), (0x1D5DD, "J"), (0x1D5DE, "K"),
    (0x1D5DF, "L"), (0x1D5E0, "M"), (0x1D5E1, "N"), (0x1D5E2, "O"), (0x1D5E3, "P"),
    (0x1D5E4, "Q"), (0x1D5E5, "R"), (0x1D5E6, "S"), (0x1D5E7, "T"), (0x1D5E8, "U"),
    (0x1D5E9, "V"), (0x1D5EA, "W"), (0x1D5EB, "X"), (0x1D5EC, "Y"), (0x1D5ED, "Z"),
    (0x1D5EE, "a"), (0x1D5EF, "b"), (0x1D5F0, "c"), (0x1D5F1, "d"), (0x1D5F2, "e"),
    (0x1D5F3, "f"), (0x1D5F4, "g"), (0x1D5F5, "h"), (0x1D5F6, "i"), (0x1D5F7, "j"),
    (0x1D5F8, "k"), (0x1D5F9, "l"), (0x1D5FA, "m"), (0x1D5FB, "n"), (0x1D5FC, "o"),
    (0x1D5FD, "p"), (0x1D5FE, "q"), (0x1D5FF, "r"), (0x1D600, "s"), (0x1D601, "t"),
    (0x1D602, "u"), (0x1D603, "v"), (0x1D604, "w"), (0x1D605, "x"), (0x1D606, "y"),
    (0x1D607, "z"), (0x1D608, "A"), (0x1D609, "B"), (0x1D60A, "C"), (0x1D60B, "D"),
    (0x1D60C, "E"), (0x1D60D, "F"), (0x1D60E, "G"), (0x1D60F, "H"), (0x1D610, "I"),
    (0x1D611, "J"), (0x1D612, "K"), (0x1D613, "L"), (0x1D614, "M"), (0x1D615, "N"),
    (0x1D616, "O"), (0x1D617, "P"), (0x1D618, "Q"), (0x1D619, "R"), (0x1D61A, "S"),
    (0x1D61B, "T"), (0x1D61C, "U"), (0x1D61D, "V"), (0x1D61E, "W"), (0x1D61F, "X"),
    (0x1D620, "Y"), (0x1D621, "Z"), (0x1D622, "a"), (0x1D623, "b"), (0x1D624, "c"),
    (0x1D625, "d"), (0x1D626, "e"), (0x1D627, "f"), (0x1D628, "g"), (0x1D629, "h"),
    (0x1D62A, "i"), (0x1D62B, "j"), (0x1D62C, "k"), (0x1D62D, "l"), (0x1D62E, "m"),
    (0x1D62F, "n"), (0x1D630, "o"), (0x1D631, "p"), (0x1D632, "q"), (0x1D633, "r"),
    (0x1D634, "s"), (0x1D635, "t"), (0x1D636, "u"), (0x1D637, "v"), (0x1D638, "w"),
    (0x1D639, "x"), (0x1D63A, "y"), (0x1D63B, "z"), (0x1D63C, "A"), (0x1D63D, "B"),
    (0x1D63E, "C"), (0x1D63F, "D"), (0x1D640, "E"), (0x1D641, "F"), (0x1D642, "G"),
    (0x1D643, "H"), (0x1D644, "I"), (0x1D645, "J"), (0x1D646, "K"), (0x1D647, "L"),
    (0x1D648, "M"), (0x1D649, "N"), (0x1D64A, "O"), (0x1D64B, "P"), (0x1D64C, "Q"),
    (0x1D64D, "R"), (0x1D64E, "S"), (0x1D64F, "T"), (0x1D650, "U"), (0x1D651, "V"),
    (0x1D652, "W"), (0x1D653, "X"), (0x1D654, "Y"), (0x1D655, "Z"), (0x1D656, "a"),
    (0x1D657, "b"), (0x1D658, "c"), (0x1D659, "d"), (0x1D65A, "e"), (0x1D65B, "f"),
    (0x1D65C, "g"), (0x1D65D, "h"), (0x1D65E, "i"), (0x1D65F, "j"), (0x1D660, "k"),
    (0x1D661, "l"), (0x1D662, "m"), (0x1D663, "n"), (0x1D664, "o"), (0x1D665, "p"),
    (0x1D666, "q"), (0x1D667, "r"), (0x1D668, "s"), (0x1D669, "t"), (0x1D66A, "u"),
    (0x1D66B, "v"), (0x1D66C, "w"), (0x1D66D, "x"), (0x1D66E, "y"), (0x1D66F, "z"),
    (0x1D670, "A"), (0x1D671, "B"), (0x1D672, "C"), (0x1D673, "D"), (0x1D674, "E"),
    (0x1D675, "F"), (0x1D676, "G"), (0x1D677, "H"), (0x1D678, "I"), (0x1D679, "J"),
    (0x1D67A, "K"), (0x1D67B, "L"), (0x1D67C, "M"), (0x1D67D, "N"), (0x1D67E, "O"),
    (0x1D67F, "P"), (0x1D680, "Q"), (0x1D681, "R"), (0x1D682, "S"), (0x1D683, "T"),
    (0x1D684, "U"), (0x1D685, "V"), (0x1D686, "W"), (0x1D687, "X"), (0x1D688, "Y"),
    (0x1D689, "Z"), (0x1D68A, "a"), (0x1D68B, "b"), (0x1D68C, "c"), (0x1D68D, "d"),
    (0x1D68E, "e"), (0x1D68F, "f"), (0x1D690, "g"), (0x1D691, "h"), (0x1D692, "i"),
    (0x1D693, "j"), (0x1D694, "k"), (0x1D695, "l"), (0x1D696, "m"), (0x1D697, "n"),
    (0x1D698, "o"), (0x1D699, "p"), (0x1D69A, "q"), (0x1D69B, "r"), (0x1D69C, "s"),
    (0x1D69D, "t"), (0x1D69E, "u"), (0x1D69F, "v"), (0x1D6A0, "w"), (0x1D6A1, "x"),
    (0x1D6A2, "y"), (0x1D6A3, "z"), (0x1D6A4, "\u{131}"), (0x1D6A5, "\u{237}"),
    (0x1D6A8, "\u{391}"), (0x1D6A9, "\u{392}"), (0x1D6AA, "\u{393}"), (0x1D6AB, "\u{394}"),
    (0x1D6AC, "\u{395}"), (0x1D6AD, "\u{396}"), (0x1D6AE, "\u{397}"), (0x1D6AF, "\u{398}"),
    (0x1D6B0, "\u{399}"), (0x1D6B1, "\u{39a}"), (0x1D6B2, "\u{39b}"), (0x1D6B3, "\u{39c}"),
    (0x1D6B4, "\u{39d}"), (0x1D6B5, "\u{39e}"), (0x1D6B6, "\u{39f}"), (0x1D6B7, "\u{3a0}"),
    (0x1D6B8, "\u{3a1}"), (0x1D6B9, "\u{398}"), (0x1D6BA, "\u{3a3}"), (0x1D6BB, "\u{3a4}"),
    (0x1D6BC, "\u{3a5}"), (0x1D6BD, "\u{3a6}"), (0x1D6BE, "\u{3a7}"), (0x1D6BF, "\u{3a8}"),
    (0x1D6C0, "\u{3a9}"), (0x1D6C1, "\u{2207}"), (0x1D6C2, "\u{3b1}"), (0x1D6C3, "\u{3b2}"),
    (0x1D6C4, "\u{3b3}"), (0x1D6C5, "\u{3b4}"), (0x1D6C6, "\u{3b5}"), (0x1D6C7, "\u{3b6}"),
    (0x1D6C8, "\u{3b7}"), (0x1D6C9, "\u{3b8}"), (0x1D6CA, "\u{3b9}"), (0x1D6CB, "\u{3ba}"),
    (0x1D6CC, "\u{3bb}"), (0x1D6CD, "\u{3bc}"), (0x1D6CE, "\u{3bd}"), (0x1D6CF, "\u{3be}"),
    (0x1D6D0, "\u{3bf}"), (0x1D6D1, "\u{3c0}"), (0x1D6D2, "\u{3c1}"), (0x1D6D3, "\u{3c2}"),
    (0x1D6D4, "\u{3c3}"), (0x1D6D5, "\u{3c4}"), (0x1D6D6, "\u{3c5}"), (0x1D6D7, "\u{3c6}"),
    (0x1D6D8, "\u{3c7}"), (0x1D6D9, "\u{3c8}"), (0x1D6DA, "\u{3c9}"), (0x1D6DB, "\u{2202}"),
    (0x1D6DC, "\u{3b5}"), (0x1D6DD, "\u{3b8}"), (0x1D6DE, "\u{3ba}"), (0x1D6DF, "\u{3c6}"),
    (0x1D6E0, "\u{3c1}"), (0x1D6E1, "\u{3c0}"), (0x1D6E2, "\u{391}"), (0x1D6E3, "\u{392}"),
    (0x1D6E4, "\u{393}"), (0x1D6E5, "\u{394}"), (0x1D6E6, "\u{395}"), (0x1D6E7, "\u{396}"),
    (0x1D6E8, "\u{397}"), (0x1D6E9, "\u{398}"), (0x1D6EA, "\u{399}"), (0x1D6EB, "\u{39a}"),
    (0x1D6EC, "\u{39b}"), (0x1D6ED, "\u{39c}"), (0x1D6EE, "\u{39d}"), (0x1D6EF, "\u{39e}"),
    (0x1D6F0, "\u{39f}"), (0x1D6F1, "\u{3a0}"), (0x1D6F2, "\u{3a1}"), (0x1D6F3, "\u{398}"),
    (0x1D6F4, "\u{3a3}"), (0x1D6F5, "\u{3a4}"), (0x1D6F6, "\u{3a5}"), (0x1D6F7, "\u{3a6}"),
    (0x1D6F8, "\u{3a7}"), (0x1D6F9, "\u{3a8}"), (0x1D6FA, "\u{3a9}"), (0x1D6FB, "\u{2207}"),
    (0x1D6FC, "\u{3b1}"), (0x1D6FD, "\u{3b2}"), (0x1D6FE, "\u{3b3}"), (0x1D6FF, "\u{3b4}"),
    (0x1D700, "\u{3b5}"), (0x1D701, "\u{3b6}"), (0x1D702, "\u{3b7}"), (0x1D703, "\u{3b8}"),
    (0x1D704, "\u{3b9}"), (0x1D705, "\u{3ba}"), (0x1D706, "\u{3bb}"), (0x1D707, "\u{3bc}"),
    (0x1D708, "\u{3bd}"), (0x1D709, "\u{3be}"), (0x1D70A, "\u{3bf}"), (0x1D70B, "\u{3c0}"),
    (0x1D70C, "\u{3c1}"), (0x1D70D, "\u{3c2}"), (0x1D70E, "\u{3c3}"), (0x1D70F, "\u{3c4}"),
    (0x1D710, "\u{3c5}"), (0x1D711, "\u{3c6}"), (0x1D712, "\u{3c7}"), (0x1D713, "\u{3c8}"),
    (0x1D714, "\u{3c9}"), (0x1D715, "\u{2202}"), (0x1D716, "\u{3b5}"), (0x1D717, "\u{3b8}"),
    (0x1D718, "\u{3ba}"), (0x1D719, "\u{3c6}"), (0x1D71A, "\u{3c1}"), (0x1D71B, "\u{3c0}"),
    (0x1D71C, "\u{391}"), (0x1D71D, "\u{392}"), (0x1D71E, "\u{393}"), (0x1D71F, "\u{394}"),
    (0x1D720, "\u{395}"), (0x1D721, "\u{396}"), (0x1D722, "\u{397}"), (0x1D723, "\u{398}"),
    (0x1D724, "\u{399}"), (0x1D725, "\u{39a}"), (0x1D726, "\u{39b}"), (0x1D727, "\u{39c}"),
    (0x1D728, "\u{39d}"), (0x1D729, "\u{39e}"), (0x1D72A, "\u{39f}"), (0x1D72B, "\u{3a0}"),
    (0x1D72C, "\u{3a1}"), (0x1D72D, "\u{398}"), (0x1D72E, "\u{3a3}"), (0x1D72F, "\u{3a4}"),
    (0x1D730, "\u{3a5}"), (0x1D731, "\u{3a6}"), (0x1D732, "\u{3a7}"), (0x1D733, "\u{3a8}"),
    (0x1D734, "\u{3a9}"), (0x1D735, "\u{2207}"), (0x1D736, "\u{3b1}"), (0x1D737, "\u{3b2}"),
    (0x1D738, "\u{3b3}"), (0x1D739, "\u{3b4}"), (0x1D73A, "\u{3b5}"), (0x1D73B, "\u{3b6}"),
    (0x1D73C, "\u{3b7}"), (0x1D73D, "\u{3b8}"), (0x1D73E, "\u{3b9}"), (0x1D73F, "\u{3ba}"),
    (0x1D740, "\u{3bb}"), (0x1D741, "\u{3bc}"), (0x1D742, "\u{3bd}"), (0x1D743, "\u{3be}"),
    (0x1D744, "\u{3bf}"), (0x1D745, "\u{3c0}"), (0x1D746, "\u{3c1}"), (0x1D747, "\u{3c2}"),
    (0x1D748, "\u{3c3}"), (0x1D749, "\u{3c4}"), (0x1D74A, "\u{3c5}"), (0x1D74B, "\u{3c6}"),
    (0x1D74C, "\u{3c7}"), (0x1D74D, "\u{3c8}"), (0x1D74E, "\u{3c9}"), (0x1D74F, "\u{2202}"),
    (0x1D750, "\u{3b5}"), (0x1D751, "\u{3b8}"), (0x1D752, "\u{3ba}"), (0x1D753, "\u{3c6}"),
    (0x1D754, "\u{3c1}"), (0x1D755, "\u{3c0}"), (0x1D756, "\u{391}"), (0x1D757, "\u{392}"),
    (0x1D758, "\u{393}"), (0x1D759, "\u{394}"), (0x1D75A, "\u{395}"), (0x1D75B, "\u{396}"),
    (0x1D75C, "\u{397}"), (0x1D75D, "\u{398}"), (0x1D75E, "\u{399}"), (0x1D75F, "\u{39a}"),
    (0x1D760, "\u{39b}"), (0x1D761, "\u{39c}"), (0x1D762, "\u{39d}"), (0x1D763, "\u{39e}"),
    (0x1D764, "\u{39f}"), (0x1D765, "\u{3a0}"), (0x1D766, "\u{3a1}"), (0x1D767, "\u{398}"),
    (0x1D768, "\u{3a3}"), (0x1D769, "\u{3a4}"), (0x1D76A, "\u{3a5}"), (0x1D76B, "\u{3a6}"),
    (0x1D76C, "\u{3a7}"), (0x1D76D, "\u{3a8}"), (0x1D76E, "\u{3a9}"), (0x1D76F, "\u{2207}"),
    (0x1D770, "\u{3b1}"), (0x1D771, "\u{3b2}"), (0x1D772, "\u{3b3}"), (0x1D773, "\u{3b4}"),
    (0x1D774, "\u{3b5}"), (0x1D775, "\u{3b6}"), (0x1D776, "\u{3b7}"), (0x1D777, "\u{3b8}"),
    (0x1D778, "\u{3b9}"), (0x1D779, "\u{3ba}"), (0x1D77A, "\u{3bb}"), (0x1D77B, "\u{3bc}"),
    (0x1D77C, "\u{3bd}"), (0x1D77D, "\u{3be}"), (0x1D77E, "\u{3bf}"), (0x1D77F, "\u{3c0}"),
    (0x1D780, "\u{3c1}"), (0x1D781, "\u{3c2}"), (0x1D782, "\u{3c3}"), (0x1D783, "\u{3c4}"),
    (0x1D784, "\u{3c5}"), (0x1D785, "\u{3c6}"), (0x1D786, "\u{3c7}"), (0x1D787, "\u{3c8}"),
    (0x1D788, "\u{3c9}"), (0x1D789, "\u{2202}"), (0x1D78A, "\u{3b5}"), (0x1D78B, "\u{3b8}"),
    (0x1D78C, "\u{3ba}"), (0x1D78D, "\u{3c6}"), (0x1D78E, "\u{3c1}"), (0x1D78F, "\u{3c0}"),
    (0x1D790, "\u{391}"), (0x1D791, "\u{392}"), (0x1D792, "\u{393}"), (0x1D793, "\u{394}"),
    (0x1D794, "\u{395}"), (0x1D795, "\u{396}"), (0x1D796, "\u{397}"), (0x1D797, "\u{398}"),
    (0x1D798, "\u{399}"), (0x1D799, "\u{39a}"), (0x1D79A, "\u{39b}"), (0x1D79B, "\u{39c}"),
    (0x1D79C, "\u{39d}"), (0x1D79D, "\u{39e}"), (0x1D79E, "\u{39f}"), (0x1D79F, "\u{3a0}"),
    (0x1D7A0, "\u{3a1}"), (0x1D7A1, "\u{398}"), (0x1D7A2, "\u{3a3}"), (0x1D7A3, "\u{3a4}"),
    (0x1D7A4, "\u{3a5}"), (0x1D7A5, "\u{3a6}"), (0x1D7A6, "\u{3a7}"), (0x1D7A7, "\u{3a8}"),
    (0x1D7A8, "\u{3a9}"), (0x1D7A9, "\u{2207}"), (0x1D7AA, "\u{3b1}"), (0x1D7AB, "\u{3b2}"),
    (0x1D7AC, "\u{3b3}"), (0x1D7AD, "\u{3b4}"), (0x1D7AE, "\u{3b5}"), (0x1D7AF, "\u{3b6}"),
    (0x1D7B0, "\u{3b7}"), (0x1D7B1, "\u{3b8}"), (0x1D7B2, "\u{3b9}"), (0x1D7B3, "\u{3ba}"),
    (0x1D7B4, "\u{3bb}"), (0x1D7B5, "\u{3bc}"), (0x1D7B6, "\u{3bd}"), (0x1D7B7, "\u{3be}"),
    (0x1D7B8, "\u{3bf}"), (0x1D7B9, "\u{3c0}"), (0x1D7BA, "\u{3c1}"), (0x1D7BB, "\u{3c2}"),
    (0x1D7BC, "\u{3c3}"), (0x1D7BD, "\u{3c4}"), (0x1D7BE, "\u{3c5}"), (0x1D7BF, "\u{3c6}"),
    (0x1D7C0, "\u{3c7}"), (0x1D7C1, "\u{3c8}"), (0x1D7C2, "\u{3c9}"), (0x1D7C3, "\u{2202}"),
    (0x1D7C4, "\u{3b5}"), (0x1D7C5, "\u{3b8}"), (0x1D7C6, "\u{3ba}"), (0x1D7C7, "\u{3c6}"),
    (0x1D7C8, "\u{3c1}"), (0x1D7C9, "\u{3c0}"), (0x1D7CA, "\u{3dc}"), (0x1D7CB, "\u{3dd}"),
    (0x1D7CE, "0"), (0x1D7CF, "1"), (0x1D7D0, "2"), (0x1D7D1, "3"), (0x1D7D2, "4"),
    (0x1D7D3, "5"), (0x1D7D4, "6"), (0x1D7D5, "7"), (0x1D7D6, "8"), (0x1D7D7, "9"),
    (0x1D7D8, "0"), (0x1D7D9, "1"), (0x1D7DA, "2"), (0x1D7DB, "3"), (0x1D7DC, "4"),
    (0x1D7DD, "5"), (0x1D7DE, "6"), (0x1D7DF, "7"), (0x1D7E0, "8"), (0x1D7E1, "9"),
    (0x1D7E2, "0"), (0x1D7E3, "1"), (0x1D7E4, "2"), (0x1D7E5, "3"), (0x1D7E6, "4"),
    (0x1D7E7, "5"), (0x1D7E8, "6"), (0x1D7E9, "7"), (0x1D7EA, "8"), (0x1D7EB, "9"),
    (0x1D7EC, "0"), (0x1D7ED, "1"), (0x1D7EE, "2"), (0x1D7EF, "3"), (0x1D7F0, "4"),
    (0x1D7F1, "5"), (0x1D7F2, "6"), (0x1D7F3, "7"), (0x1D7F4, "8"), (0x1D7F5, "9"),
    (0x1D7F6, "0"), (0x1D7F7, "1"), (0x1D7F8, "2"), (0x1D7F9, "3"), (0x1D7FA, "4"),
    (0x1D7FB, "5"), (0x1D7FC, "6"), (0x1D7FD, "7"), (0x1D7FE, "8"), (0x1D7FF, "9"),
    (0x1E030, "\u{430}"), (0x1E031, "\u{431}"), (0x1E032, "\u{432}"), (0x1E033, "\u{433}"),
    (0x1E034, "\u{434}"), (0x1E035, "\u{435}"), (0x1E036, "\u{436}"), (0x1E037, "\u{437}"),
    (0x1E038, "\u{438}"), (0x1E039, "\u{43a}"), (0x1E03A, "\u{43b}"), (0x1E03B, "\u{43c}"),
    (0x1E03C, "\u{43e}"), (0x1E03D, "\u{43f}"), (0x1E03E, "\u{440}"), (0x1E03F, "\u{441}"),
    (0x1E040, "\u{442}"), (0x1E041, "\u{443}"), (0x1E042, "\u{444}"), (0x1E043, "\u{445}"),
    (0x1E044, "\u{446}"), (0x1E045, "\u{447}"), (0x1E046, "\u{448}"), (0x1E047, "\u{44b}"),
    (0x1E048, "\u{44d}"), (0x1E049, "\u{44e}"), (0x1E04A, "\u{a689}"), (0x1E04B, "\u{4d9}"),
    (0x1E04C, "\u{456}"), (0x1E04D, "\u{458}"), (0x1E04E, "\u{4e9}"), (0x1E04F, "\u{4af}"),
    (0x1E050, "\u{4cf}"), (0x1E051, "\u{430}"), (0x1E052, "\u{431}"), (0x1E053, "\u{432}"),
    (0x1E054, "\u{433}"), (0x1E055, "\u{434}"), (0x1E056, "\u{435}"), (0x1E057, "\u{436}"),
    (0x1E058, "\u{437}"), (0x1E059, "\u{438}"), (0x1E05A, "\u{43a}"), (0x1E05B, "\u{43b}"),
    (0x1E05C, "\u{43e}"), (0x1E05D, "\u{43f}"), (0x1E05E, "\u{441}"), (0x1E05F, "\u{443}"),
    (0x1E060, "\u{444}"), (0x1E061, "\u{445}"), (0x1E062, "\u{446}"), (0x1E063, "\u{447}"),
    (0x1E064, "\u{448}"), (0x1E065, "\u{44a}"), (0x1E066, "\u{44b}"), (0x1E067, "\u{491}"),
    (0x1E068, "\u{456}"), (0x1E069, "\u{455}"), (0x1E06A, "\u{45f}"), (0x1E06B, "\u{4ab}"),
    (0x1E06C, "\u{a651}"), (0x1E06D, "\u{4b1}"), (0x1EE00, "\u{627}"), (0x1EE01, "\u{628}"),
    (0x1EE02, "\u{62c}"), (0x1EE03, "\u{62f}"), (0x1EE05, "\u{648}"), (0x1EE06, "\u{632}"),
    (0x1EE07, "\u{62d}"), (0x1EE08, "\u{637}"), (0x1EE09, "\u{64a}"), (0x1EE0A, "\u{643}"),
    (0x1EE0B, "\u{644}"), (0x1EE0C, "\u{645}"), (0x1EE0D, "\u{646}"), (0x1EE0E, "\u{633}"),
    (0x1EE0F, "\u{639}"), (0x1EE10, "\u{641}"), (0x1EE11, "\u{635}"), (0x1EE12, "\u{642}"),
    (0x1EE13, "\u{631}"), (0x1EE14, "\u{634}"), (0x1EE15, "\u{62a}"), (0x1EE16, "\u{62b}"),
    (0x1EE17, "\u{62e}"), (0x1EE18, "\u{630}"), (0x1EE19, "\u{636}"), (0x1EE1A, "\u{638}"),
    (0x1EE1B, "\u{63a}"), (0x1EE1C, "\u{66e}"), (0x1EE1D, "\u{6ba}"), (0x1EE1E, "\u{6a1}"),
    (0x1EE1F, "\u{66f}"), (0x1EE21, "\u{628}"), (0x1EE22, "\u{62c}"), (0x1EE24, "\u{647}"),
    (0x1EE27, "\u{62d}"), (0x1EE29, "\u{64a}"), (0x1EE2A, "\u{643}"), (0x1EE2B, "\u{644}"),
    (0x1EE2C, "\u{645}"), (0x1EE2D, "\u{646}"), (0x1EE2E, "\u{633}"), (0x1EE2F, "\u{639}"),
    (0x1EE30, "\u{641}"), (0x1EE31, "\u{635}"), (0x1EE32, "\u{642}"), (0x1EE34, "\u{634}"),
    (0x1EE35, "\u{62a}"), (0x1EE36, "\u{62b}"), (0x1EE37, "\u{62e}"), (0x1EE39, "\u{636}"),
    (0x1EE3B, "\u{63a}"), (0x1EE42, "\u{62c}"), (0x1EE47, "\u{62d}"), (0x1EE49, "\u{64a}"),
    (0x1EE4B, "\u{644}"), (0x1EE4D, "\u{646}"), (0x1EE4E, "\u{633}"), (0x1EE4F, "\u{639}"),
    (0x1EE51, "\u{635}"), (0x1EE52, "\u{642}"), (0x1EE54, "\u{634}"), (0x1EE57, "\u{62e}"),
    (0x1EE59, "\u{636}"), (0x1EE5B, "\u{63a}"), (0x1EE5D, "\u{6ba}"), (0x1EE5F, "\u{66f}"),
    (0x1EE61, "\u{628}"), (0x1EE62, "\u{62c}"), (0x1EE64, "\u{647}"), (0x1EE67, "\u{62d}"),
    (0x1EE68, "\u{637}"), (0x1EE69, "\u{64a}"), (0x1EE6A, "\u{643}"), (0x1EE6C, "\u{645}"),
    (0x1EE6D, "\u{646}"), (0x1EE6E, "\u{633}"), (0x1EE6F, "\u{639}"), (0x1EE70, "\u{641}"),
    (0x1EE71, "\u{635}"), (0x1EE72, "\u{642}"), (0x1EE74, "\u{634}"), (0x1EE75, "\u{62a}"),
    (0x1EE76, "\u{62b}"), (0x1EE77, "\u{62e}"), (0x1EE79, "\u{636}"), (0x1EE7A, "\u{638}"),
    (0x1EE7B, "\u{63a}"), (0x1EE7C, "\u{66e}"), (0x1EE7E, "\u{6a1}"), (0x1EE80, "\u{627}"),
    (0x1EE81, "\u{628}"), (0x1EE82, "\u{62c}"), (0x1EE83, "\u{62f}"), (0x1EE84, "\u{647}"),
    (0x1EE85, "\u{648}"), (0x1EE86, "\u{632}"), (0x1EE87, "\u{62d}"), (0x1EE88, "\u{637}"),
    (0x1EE89, "\u{64a}"), (0x1EE8B, "\u{644}"), (0x1EE8C, "\u{645}"), (0x1EE8D, "\u{646}"),
    (0x1EE8E, "\u{633}"), (0x1EE8F, "\u{639}"), (0x1EE90, "\u{641}"), (0x1EE91, "\u{635}"),
    (0x1EE92, "\u{642}"), (0x1EE93, "\u{631}"), (0x1EE94, "\u{634}"), (0x1EE95, "\u{62a}"),
    (0x1EE96, "\u{62b}"), (0x1EE97, "\u{62e}"), (0x1EE98, "\u{630}"), (0x1EE99, "\u{636}"),
    (0x1EE9A, "\u{638}"), (0x1EE9B, "\u{63a}"), (0x1EEA1, "\u{628}"), (0x1EEA2, "\u{62c}"),
    (0x1EEA3, "\u{62f}"), (0x1EEA5, "\u{648}"), (0x1EEA6, "\u{632}"), (0x1EEA7, "\u{62d}"),
    (0x1EEA8, "\u{637}"), (0x1EEA9, "\u{64a}"), (0x1EEAB, "\u{644}"), (0x1EEAC, "\u{645}"),
    (0x1EEAD, "\u{646}"), (0x1EEAE, "\u{633}"), (0x1EEAF, "\u{639}"), (0x1EEB0, "\u{641}"),
    (0x1EEB1, "\u{635}"), (0x1EEB2, "\u{642}"), (0x1EEB3, "\u{631}"), (0x1EEB4, "\u{634}"),
    (0x1EEB5, "\u{62a}"), (0x1EEB6, "\u{62b}"), (0x1EEB7, "\u{62e}"), (0x1EEB8, "\u{630}"),
    (0x1EEB9, "\u{636}"), (0x1EEBA, "\u{638}"), (0x1EEBB, "\u{63a}"), (0x1F100, "0."),
    (0x1F101, "0,"), (0x1F102, "1,"), (0x1F103, "2,"), (0x1F104, "3,"), (0x1F105, "4,"),
    (0x1F106, "5,"), (0x1F107, "6,"), (0x1F108, "7,"), (0x1F109, "8,"), (0x1F10A, "9,"),
    (0x1F110, "(A)"), (0x1F111, "(B)"), (0x1F112, "(C)"), (0x1F113, "(D)"), (0x1F114, "(E)"),
    (0x1F115, "(F)"), (0x1F116, "(G)"), (0x1F117, "(H)"), (0x1F118, "(I)"), (0x1F119, "(J)"),
    (0x1F11A, "(K)"), (0x1F11B, "(L)"), (0x1F11C, "(M)"), (0x1F11D, "(N)"), (0x1F11E, "(O)"),
    (0x1F11F, "(P)"), (0x1F120, "(Q)"), (0x1F121, "(R)"), (0x1F122, "(S)"), (0x1F123, "(T)"),
    (0x1F124, "(U)"), (0x1F125, "(V)"), (0x1F126, "(W)"), (0x1F127, "(X)"), (0x1F128, "(Y)"),
    (0x1F129, "(Z)"), (0x1F12A, "\u{3014}S\u{3015}"), (0x1F12B, "C"), (0x1F12C, "R"),
    (0x1F12D, "CD"), (0x1F12E, "WZ"), (0x1F130, "A"), (0x1F131, "B"), (0x1F132, "C"),
    (0x1F133, "D"), (0x1F134, "E"), (0x1F135, "F"), (0x1F136, "G"), (0x1F137, "H"),
    (0x1F138, "I"), (0x1F139, "J"), (0x1F13A, "K"), (0x1F13B, "L"), (0x1F13C, "M"),
    (0x1F13D, "N"), (0x1F13E, "O"), (0x1F13F, "P"), (0x1F140, "Q"), (0x1F141, "R"),
    (0x1F142, "S"), (0x1F143, "T"), (0x1F144, "U"), (0x1F145, "V"), (0x1F146, "W"),
    (0x1F147, "X"), (0x1F148, "Y"), (0x1F149, "Z"), (0x1F14A, "HV"), (0x1F14B, "MV"),
    (0x1F14C, "SD"), (0x1F14D, "SS"), (0x1F14E, "PPV"), (0x1F14F, "WC"), (0x1F16A, "MC"),
    (0x1F16B, "MD"), (0x1F16C, "MR"), (0x1F190, "DJ"), (0x1F200, "\u{307b}\u{304b}"),
    (0x1F201, "\u{30b3}\u{30b3}"), (0x1F202, "\u{30b5}"), (0x1F210, "\u{624b}"),
    (0x1F211, "\u{5b57}"), (0x1F212, "\u{53cc}"), (0x1F213, "\u{30c6}\u{3099}"),
    (0x1F214, "\u{4e8c}"), (0x1F215, "\u{591a}"), (0x1F216, "\u{89e3}"), (0x1F217, "\u{5929}"),
    (0x1F218, "\u{4ea4}"), (0x1F219, "\u{6620}"), (0x1F21A, "\u{7121}"), (0x1F21B, "\u{6599}"),
    (0x1F21C, "\u{524d}"), (0x1F21D, "\u{5f8c}"), (0x1F21E, "\u{518d}"), (0x1F21F, "\u{65b0}"),
    (0x1F220, "\u{521d}"), (0x1F221, "\u{7d42}"), (0x1F222, "\u{751f}"), (0x1F223, "\u{8ca9}"),
    (0x1F224, "\u{58f0}"), (0x1F225, "\u{5439}"), (0x1F226, "\u{6f14}"), (0x1F227, "\u{6295}"),
    (0x1F228, "\u{6355}"), (0x1F229, "\u{4e00}"), (0x1F22A, "\u{4e09}"), (0x1F22B, "\u{904a}"),
    (0x1F22C, "\u{5de6}"), (0x1F22D, "\u{4e2d}"), (0x1F22E, "\u{53f3}"), (0x1F22F, "\u{6307}"),
    (0x1F230, "\u{8d70}"), (0x1F231, "\u{6253}"), (0x1F232, "\u{7981}"), (0x1F233, "\u{7a7a}"),
    (0x1F234, "\u{5408}"), (0x1F235, "\u{6e80}"), (0x1F236, "\u{6709}"), (0x1F237, "\u{6708}"),
    (0x1F238, "\u{7533}"), (0x1F239, "\u{5272}"), (0x1F23A, "\u{55b6}"), (0x1F23B, "\u{914d}"),
    (0x1F240, "\u{3014}\u{672c}\u{3015}"), (0x1F241, "\u{3014}\u{4e09}\u{3015}"),
    (0x1F242, "\u{3014}\u{4e8c}\u{3015}"), (0x1F243, "\u{3014}\u{5b89}\u{3015}"),
    (0x1F244, "\u{3014}\u{70b9}\u{3015}"), (0x1F245, "\u{3014}\u{6253}\u{3015}"),
    (0x1F246, "\u{3014}\u{76d7}\u{3015}"), (0x1F247, "\u{3014}\u{52dd}\u{3015}"),
    (0x1F248, "\u{3014}\u{6557}\u{3015}"), (0x1F250, "\u{5f97}"), (0x1F251, "\u{53ef}"),
    (0x1FBF0, "0"), (0x1FBF1, "1"), (0x1FBF2, "2"), (0x1FBF3, "3"), (0x1FBF4, "4"),
    (0x1FBF5, "5"), (0x1FBF6, "6"), (0x1FBF7, "7"), (0x1FBF8, "8"), (0x1FBF9, "9"),
    (0x2F800, "\u{4e3d}"), (0x2F801, "\u{4e38}"), (0x2F802, "\u{4e41}"),
    (0x2F803, "\u{20122}"), (0x2F804, "\u{4f60}"), (0x2F805, "\u{4fae}"),
    (0x2F806, "\u{4fbb}"), (0x2F807, "\u{5002}"), (0x2F808, "\u{507a}"), (0x2F809, "\u{5099}"),
    (0x2F80A, "\u{50e7}"), (0x2F80B, "\u{50cf}"), (0x2F80C, "\u{349e}"),
    (0x2F80D, "\u{2063a}"), (0x2F80E, "\u{514d}"), (0x2F80F, "\u{5154}"),
    (0x2F810, "\u{5164}"), (0x2F811, "\u{5177}"), (0x2F812, "\u{2051c}"),
    (0x2F813, "\u{34b9}"), (0x2F814, "\u{5167}"), (0x2F815, "\u{518d}"),
    (0x2F816, "\u{2054b}"), (0x2F817, "\u{5197}"), (0x2F818, "\u{51a4}"),
    (0x2F819, "\u{4ecc}"), (0x2F81A, "\u{51ac}"), (0x2F81B, "\u{51b5}"),
    (0x2F81C, "\u{291df}"), (0x2F81D, "\u{51f5}"), (0x2F81E, "\u{5203}"),
    (0x2F81F, "\u{34df}"), (0x2F820, "\u{523b}"), (0x2F821, "\u{5246}"), (0x2F822, "\u{5272}"),
    (0x2F823, "\u{5277}"), (0x2F824, "\u{3515}"), (0x2F825, "\u{52c7}"), (0x2F826, "\u{52c9}"),
    (0x2F827, "\u{52e4}"), (0x2F828, "\u{52fa}"), (0x2F829, "\u{5305}"), (0x2F82A, "\u{5306}"),
    (0x2F82B, "\u{5317}"), (0x2F82C, "\u{5349}"), (0x2F82D, "\u{5351}"), (0x2F82E, "\u{535a}"),
    (0x2F82F, "\u{5373}"), (0x2F830, "\u{537d}"), (0x2F831, "\u{537f}"), (0x2F832, "\u{537f}"),
    (0x2F833, "\u{537f}"), (0x2F834, "\u{20a2c}"), (0x2F835, "\u{7070}"),
    (0x2F836, "\u{53ca}"), (0x2F837, "\u{53df}"), (0x2F838, "\u{20b63}"),
    (0x2F839, "\u{53eb}"), (0x2F83A, "\u{53f1}"), (0x2F83B, "\u{5406}"), (0x2F83C, "\u{549e}"),
    (0x2F83D, "\u{5438}"), (0x2F83E, "\u{5448}"), (0x2F83F, "\u{5468}"), (0x2F840, "\u{54a2}"),
    (0x2F841, "\u{54f6}"), (0x2F842, "\u{5510}"), (0x2F843, "\u{5553}"), (0x2F844, "\u{5563}"),
    (0x2F845, "\u{5584}"), (0x2F846, "\u{5584}"), (0x2F847, "\u{5599}"), (0x2F848, "\u{55ab}"),
    (0x2F849, "\u{55b3}"), (0x2F84A, "\u{55c2}"), (0x2F84B, "\u{5716}"), (0x2F84C, "\u{5606}"),
    (0x2F84D, "\u{5717}"), (0x2F84E, "\u{5651}"), (0x2F84F, "\u{5674}"), (0x2F850, "\u{5207}"),
    (0x2F851, "\u{58ee}"), (0x2F852, "\u{57ce}"), (0x2F853, "\u{57f4}"), (0x2F854, "\u{580d}"),
    (0x2F855, "\u{578b}"), (0x2F856, "\u{5832}"), (0x2F857, "\u{5831}"), (0x2F858, "\u{58ac}"),
    (0x2F859, "\u{214e4}"), (0x2F85A, "\u{58f2}"), (0x2F85B, "\u{58f7}"),
    (0x2F85C, "\u{5906}"), (0x2F85D, "\u{591a}"), (0x2F85E, "\u{5922}"), (0x2F85F, "\u{5962}"),
    (0x2F860, "\u{216a8}"), (0x2F861, "\u{216ea}"), (0x2F862, "\u{59ec}"),
    (0x2F863, "\u{5a1b}"), (0x2F864, "\u{5a27}"), (0x2F865, "\u{59d8}"), (0x2F866, "\u{5a66}"),
    (0x2F867, "\u{36ee}"), (0x2F868, "\u{36fc}"), (0x2F869, "\u{5b08}"), (0x2F86A, "\u{5b3e}"),
    (0x2F86B, "\u{5b3e}"), (0x2F86C, "\u{219c8}"), (0x2F86D, "\u{5bc3}"),
    (0x2F86E, "\u{5bd8}"), (0x2F86F, "\u{5be7}"), (0x2F870, "\u{5bf3}"),
    (0x2F871, "\u{21b18}"), (0x2F872, "\u{5bff}"), (0x2F873, "\u{5c06}"),
    (0x2F874, "\u{5f53}"), (0x2F875, "\u{5c22}"), (0x2F876, "\u{3781}"), (0x2F877, "\u{5c60}"),
    (0x2F878, "\u{5c6e}"), (0x2F879, "\u{5cc0}"), (0x2F87A, "\u{5c8d}"),
    (0x2F87B, "\u{21de4}"), (0x2F87C, "\u{5d43}"), (0x2F87D, "\u{21de6}"),
    (0x2F87E, "\u{5d6e}"), (0x2F87F, "\u{5d6b}"), (0x2F880, "\u{5d7c}"), (0x2F881, "\u{5de1}"),
    (0x2F882, "\u{5de2}"), (0x2F883, "\u{382f}"), (0x2F884, "\u{5dfd}"), (0x2F885, "\u{5e28}"),
    (0x2F886, "\u{5e3d}"), (0x2F887, "\u{5e69}"), (0x2F888, "\u{3862}"),
    (0x2F889, "\u{22183}"), (0x2F88A, "\u{387c}"), (0x2F88B, "\u{5eb0}"),
    (0x2F88C, "\u{5eb3}"), (0x2F88D, "\u{5eb6}"), (0x2F88E, "\u{5eca}"),
    (0x2F88F, "\u{2a392}"), (0x2F890, "\u{5efe}"), (0x2F891, "\u{22331}"),
    (0x2F892, "\u{22331}"), (0x2F893, "\u{8201}"), (0x2F894, "\u{5f22}"),
    (0x2F895, "\u{5f22}"), (0x2F896, "\u{38c7}"), (0x2F897, "\u{232b8}"),
    (0x2F898, "\u{261da}"), (0x2F899, "\u{5f62}"), (0x2F89A, "\u{5f6b}"),
    (0x2F89B, "\u{38e3}"), (0x2F89C, "\u{5f9a}"), (0x2F89D, "\u{5fcd}"), (0x2F89E, "\u{5fd7}"),
    (0x2F89F, "\u{5ff9}"), (0x2F8A0, "\u{6081}"), (0x2F8A1, "\u{393a}"), (0x2F8A2, "\u{391c}"),
    (0x2F8A3, "\u{6094}"), (0x2F8A4, "\u{226d4}"), (0x2F8A5, "\u{60c7}"),
    (0x2F8A6, "\u{6148}"), (0x2F8A7, "\u{614c}"), (0x2F8A8, "\u{614e}"), (0x2F8A9, "\u{614c}"),
    (0x2F8AA, "\u{617a}"), (0x2F8AB, "\u{618e}"), (0x2F8AC, "\u{61b2}"), (0x2F8AD, "\u{61a4}"),
    (0x2F8AE, "\u{61af}"), (0x2F8AF, "\u{61de}"), (0x2F8B0, "\u{61f2}"), (0x2F8B1, "\u{61f6}"),
    (0x2F8B2, "\u{6210}"), (0x2F8B3, "\u{621b}"), (0x2F8B4, "\u{625d}"), (0x2F8B5, "\u{62b1}"),
    (0x2F8B6, "\u{62d4}"), (0x2F8B7, "\u{6350}"), (0x2F8B8, "\u{22b0c}"),
    (0x2F8B9, "\u{633d}"), (0x2F8BA, "\u{62fc}"), (0x2F8BB, "\u{6368}"), (0x2F8BC, "\u{6383}"),
    (0x2F8BD, "\u{63e4}"), (0x2F8BE, "\u{22bf1}"), (0x2F8BF, "\u{6422}"),
    (0x2F8C0, "\u{63c5}"), (0x2F8C1, "\u{63a9}"), (0x2F8C2, "\u{3a2e}"), (0x2F8C3, "\u{6469}"),
    (0x2F8C4, "\u{647e}"), (0x2F8C5, "\u{649d}"), (0x2F8C6, "\u{6477}"), (0x2F8C7, "\u{3a6c}"),
    (0x2F8C8, "\u{654f}"), (0x2F8C9, "\u{656c}"), (0x2F8CA, "\u{2300a}"),
    (0x2F8CB, "\u{65e3}"), (0x2F8CC, "\u{66f8}"), (0x2F8CD, "\u{6649}"), (0x2F8CE, "\u{3b19}"),
    (0x2F8CF, "\u{6691}"), (0x2F8D0, "\u{3b08}"), (0x2F8D1, "\u{3ae4}"), (0x2F8D2, "\u{5192}"),
    (0x2F8D3, "\u{5195}"), (0x2F8D4, "\u{6700}"), (0x2F8D5, "\u{669c}"), (0x2F8D6, "\u{80ad}"),
    (0x2F8D7, "\u{43d9}"), (0x2F8D8, "\u{6717}"), (0x2F8D9, "\u{671b}"), (0x2F8DA, "\u{6721}"),
    (0x2F8DB, "\u{675e}"), (0x2F8DC, "\u{6753}"), (0x2F8DD, "\u{233c3}"),
    (0x2F8DE, "\u{3b49}"), (0x2F8DF, "\u{67fa}"), (0x2F8E0, "\u{6785}"), (0x2F8E1, "\u{6852}"),
    (0x2F8E2, "\u{6885}"), (0x2F8E3, "\u{2346d}"), (0x2F8E4, "\u{688e}"),
    (0x2F8E5, "\u{681f}"), (0x2F8E6, "\u{6914}"), (0x2F8E7, "\u{3b9d}"), (0x2F8E8, "\u{6942}"),
    (0x2F8E9, "\u{69a3}"), (0x2F8EA, "\u{69ea}"), (0x2F8EB, "\u{6aa8}"),
    (0x2F8EC, "\u{236a3}"), (0x2F8ED, "\u{6adb}"), (0x2F8EE, "\u{3c18}"),
    (0x2F8EF, "\u{6b21}"), (0x2F8F0, "\u{238a7}"), (0x2F8F1, "\u{6b54}"),
    (0x2F8F2, "\u{3c4e}"), (0x2F8F3, "\u{6b72}"), (0x2F8F4, "\u{6b9f}"), (0x2F8F5, "\u{6bba}"),
    (0x2F8F6, "\u{6bbb}"), (0x2F8F7, "\u{23a8d}"), (0x2F8F8, "\u{21d0b}"),
    (0x2F8F9, "\u{23afa}"), (0x2F8FA, "\u{6c4e}"), (0x2F8FB, "\u{23cbc}"),
    (0x2F8FC, "\u{6cbf}"), (0x2F8FD, "\u{6ccd}"), (0x2F8FE, "\u{6c67}"), (0x2F8FF, "\u{6d16}"),
    (0x2F900, "\u{6d3e}"), (0x2F901, "\u{6d77}"), (0x2F902, "\u{6d41}"), (0x2F903, "\u{6d69}"),
    (0x2F904, "\u{6d78}"), (0x2F905, "\u{6d85}"), (0x2F906, "\u{23d1e}"),
    (0x2F907, "\u{6d34}"), (0x2F908, "\u{6e2f}"), (0x2F909, "\u{6e6e}"), (0x2F90A, "\u{3d33}"),
    (0x2F90B, "\u{6ecb}"), (0x2F90C, "\u{6ec7}"), (0x2F90D, "\u{23ed1}"),
    (0x2F90E, "\u{6df9}"), (0x2F90F, "\u{6f6e}"), (0x2F910, "\u{23f5e}"),
    (0x2F911, "\u{23f8e}"), (0x2F912, "\u{6fc6}"), (0x2F913, "\u{7039}"),
    (0x2F914, "\u{701e}"), (0x2F915, "\u{701b}"), (0x2F916, "\u{3d96}"), (0x2F917, "\u{704a}"),
    (0x2F918, "\u{707d}"), (0x2F919, "\u{7077}"), (0x2F91A, "\u{70ad}"),
    (0x2F91B, "\u{20525}"), (0x2F91C, "\u{7145}"), (0x2F91D, "\u{24263}"),
    (0x2F91E, "\u{719c}"), (0x2F91F, "\u{243ab}"), (0x2F920, "\u{7228}"),
    (0x2F921, "\u{7235}"), (0x2F922, "\u{7250}"), (0x2F923, "\u{24608}"),
    (0x2F924, "\u{7280}"), (0x2F925, "\u{7295}"), (0x2F926, "\u{24735}"),
    (0x2F927, "\u{24814}"), (0x2F928, "\u{737a}"), (0x2F929, "\u{738b}"),
    (0x2F92A, "\u{3eac}"), (0x2F92B, "\u{73a5}"), (0x2F92C, "\u{3eb8}"), (0x2F92D, "\u{3eb8}"),
    (0x2F92E, "\u{7447}"), (0x2F92F, "\u{745c}"), (0x2F930, "\u{7471}"), (0x2F931, "\u{7485}"),
    (0x2F932, "\u{74ca}"), (0x2F933, "\u{3f1b}"), (0x2F934, "\u{7524}"),
    (0x2F935, "\u{24c36}"), (0x2F936, "\u{753e}"), (0x2F937, "\u{24c92}"),
    (0x2F938, "\u{7570}"), (0x2F939, "\u{2219f}"), (0x2F93A, "\u{7610}"),
    (0x2F93B, "\u{24fa1}"), (0x2F93C, "\u{24fb8}"), (0x2F93D, "\u{25044}"),
    (0x2F93E, "\u{3ffc}"), (0x2F93F, "\u{4008}"), (0x2F940, "\u{76f4}"),
    (0x2F941, "\u{250f3}"), (0x2F942, "\u{250f2}"), (0x2F943, "\u{25119}"),
    (0x2F944, "\u{25133}"), (0x2F945, "\u{771e}"), (0x2F946, "\u{771f}"),
    (0x2F947, "\u{771f}"), (0x2F948, "\u{774a}"), (0x2F949, "\u{4039}"), (0x2F94A, "\u{778b}"),
    (0x2F94B, "\u{4046}"), (0x2F94C, "\u{4096}"), (0x2F94D, "\u{2541d}"),
    (0x2F94E, "\u{784e}"), (0x2F94F, "\u{788c}"), (0x2F950, "\u{78cc}"), (0x2F951, "\u{40e3}"),
    (0x2F952, "\u{25626}"), (0x2F953, "\u{7956}"), (0x2F954, "\u{2569a}"),
    (0x2F955, "\u{256c5}"), (0x2F956, "\u{798f}"), (0x2F957, "\u{79eb}"),
    (0x2F958, "\u{412f}"), (0x2F959, "\u{7a40}"), (0x2F95A, "\u{7a4a}"), (0x2F95B, "\u{7a4f}"),
    (0x2F95C, "\u{2597c}"), (0x2F95D, "\u{25aa7}"), (0x2F95E, "\u{25aa7}"),
    (0x2F95F, "\u{7aee}"), (0x2F960, "\u{4202}"), (0x2F961, "\u{25bab}"),
    (0x2F962, "\u{7bc6}"), (0x2F963, "\u{7bc9}"), (0x2F964, "\u{4227}"),
    (0x2F965, "\u{25c80}"), (0x2F966, "\u{7cd2}"), (0x2F967, "\u{42a0}"),
    (0x2F968, "\u{7ce8}"), (0x2F969, "\u{7ce3}"), (0x2F96A, "\u{7d00}"),
    (0x2F96B, "\u{25f86}"), (0x2F96C, "\u{7d63}"), (0x2F96D, "\u{4301}"),
    (0x2F96E, "\u{7dc7}"), (0x2F96F, "\u{7e02}"), (0x2F970, "\u{7e45}"), (0x2F971, "\u{4334}"),
    (0x2F972, "\u{26228}"), (0x2F973, "\u{26247}"), (0x2F974, "\u{4359}"),
    (0x2F975, "\u{262d9}"), (0x2F976, "\u{7f7a}"), (0x2F977, "\u{2633e}"),
    (0x2F978, "\u{7f95}"), (0x2F979, "\u{7ffa}"), (0x2F97A, "\u{8005}"),
    (0x2F97B, "\u{264da}"), (0x2F97C, "\u{26523}"), (0x2F97D, "\u{8060}"),
    (0x2F97E, "\u{265a8}"), (0x2F97F, "\u{8070}"), (0x2F980, "\u{2335f}"),
    (0x2F981, "\u{43d5}"), (0x2F982, "\u{80b2}"), (0x2F983, "\u{8103}"), (0x2F984, "\u{440b}"),
    (0x2F985, "\u{813e}"), (0x2F986, "\u{5ab5}"), (0x2F987, "\u{267a7}"),
    (0x2F988, "\u{267b5}"), (0x2F989, "\u{23393}"), (0x2F98A, "\u{2339c}"),
    (0x2F98B, "\u{8201}"), (0x2F98C, "\u{8204}"), (0x2F98D, "\u{8f9e}"), (0x2F98E, "\u{446b}"),
    (0x2F98F, "\u{8291}"), (0x2F990, "\u{828b}"), (0x2F991, "\u{829d}"), (0x2F992, "\u{52b3}"),
    (0x2F993, "\u{82b1}"), (0x2F994, "\u{82b3}"), (0x2F995, "\u{82bd}"), (0x2F996, "\u{82e6}"),
    (0x2F997, "\u{26b3c}"), (0x2F998, "\u{82e5}"), (0x2F999, "\u{831d}"),
    (0x2F99A, "\u{8363}"), (0x2F99B, "\u{83ad}"), (0x2F99C, "\u{8323}"), (0x2F99D, "\u{83bd}"),
    (0x2F99E, "\u{83e7}"), (0x2F99F, "\u{8457}"), (0x2F9A0, "\u{8353}"), (0x2F9A1, "\u{83ca}"),
    (0x2F9A2, "\u{83cc}"), (0x2F9A3, "\u{83dc}"), (0x2F9A4, "\u{26c36}"),
    (0x2F9A5, "\u{26d6b}"), (0x2F9A6, "\u{26cd5}"), (0x2F9A7, "\u{452b}"),
    (0x2F9A8, "\u{84f1}"), (0x2F9A9, "\u{84f3}"), (0x2F9AA, "\u{8516}"),
    (0x2F9AB, "\u{273ca}"), (0x2F9AC, "\u{8564}"), (0x2F9AD, "\u{26f2c}"),
    (0x2F9AE, "\u{455d}"), (0x2F9AF, "\u{4561}"), (0x2F9B0, "\u{26fb1}"),
    (0x2F9B1, "\u{270d2}"), (0x2F9B2, "\u{456b}"), (0x2F9B3, "\u{8650}"),
    (0x2F9B4, "\u{865c}"), (0x2F9B5, "\u{8667}"), (0x2F9B6, "\u{8669}"), (0x2F9B7, "\u{86a9}"),
    (0x2F9B8, "\u{8688}"), (0x2F9B9, "\u{870e}"), (0x2F9BA, "\u{86e2}"), (0x2F9BB, "\u{8779}"),
    (0x2F9BC, "\u{8728}"), (0x2F9BD, "\u{876b}"), (0x2F9BE, "\u{8786}"), (0x2F9BF, "\u{45d7}"),
    (0x2F9C0, "\u{87e1}"), (0x2F9C1, "\u{8801}"), (0x2F9C2, "\u{45f9}"), (0x2F9C3, "\u{8860}"),
    (0x2F9C4, "\u{8863}"), (0x2F9C5, "\u{27667}"), (0x2F9C6, "\u{88d7}"),
    (0x2F9C7, "\u{88de}"), (0x2F9C8, "\u{4635}"), (0x2F9C9, "\u{88fa}"), (0x2F9CA, "\u{34bb}"),
    (0x2F9CB, "\u{278ae}"), (0x2F9CC, "\u{27966}"), (0x2F9CD, "\u{46be}"),
    (0x2F9CE, "\u{46c7}"), (0x2F9CF, "\u{8aa0}"), (0x2F9D0, "\u{8aed}"), (0x2F9D1, "\u{8b8a}"),
    (0x2F9D2, "\u{8c55}"), (0x2F9D3, "\u{27ca8}"), (0x2F9D4, "\u{8cab}"),
    (0x2F9D5, "\u{8cc1}"), (0x2F9D6, "\u{8d1b}"), (0x2F9D7, "\u{8d77}"),
    (0x2F9D8, "\u{27f2f}"), (0x2F9D9, "\u{20804}"), (0x2F9DA, "\u{8dcb}"),
    (0x2F9DB, "\u{8dbc}"), (0x2F9DC, "\u{8df0}"), (0x2F9DD, "\u{208de}"),
    (0x2F9DE, "\u{8ed4}"), (0x2F9DF, "\u{8f38}"), (0x2F9E0, "\u{285d2}"),
    (0x2F9E1, "\u{285ed}"), (0x2F9E2, "\u{9094}"), (0x2F9E3, "\u{90f1}"),
    (0x2F9E4, "\u{9111}"), (0x2F9E5, "\u{2872e}"), (0x2F9E6, "\u{911b}"),
    (0x2F9E7, "\u{9238}"), (0x2F9E8, "\u{92d7}"), (0x2F9E9, "\u{92d8}"), (0x2F9EA, "\u{927c}"),
    (0x2F9EB, "\u{93f9}"), (0x2F9EC, "\u{9415}"), (0x2F9ED, "\u{28bfa}"),
    (0x2F9EE, "\u{958b}"), (0x2F9EF, "\u{4995}"), (0x2F9F0, "\u{95b7}"),
    (0x2F9F1, "\u{28d77}"), (0x2F9F2, "\u{49e6}"), (0x2F9F3, "\u{96c3}"),
    (0x2F9F4, "\u{5db2}"), (0x2F9F5, "\u{9723}"), (0x2F9F6, "\u{29145}"),
    (0x2F9F7, "\u{2921a}"), (0x2F9F8, "\u{4a6e}"), (0x2F9F9, "\u{4a76}"),
    (0x2F9FA, "\u{97e0}"), (0x2F9FB, "\u{2940a}"), (0x2F9FC, "\u{4ab2}"),
    (0x2F9FD, "\u{29496}"), (0x2F9FE, "\u{980b}"), (0x2F9FF, "\u{980b}"),
    (0x2FA00, "\u{9829}"), (0x2FA01, "\u{295b6}"), (0x2FA02, "\u{98e2}"),
    (0x2FA03, "\u{4b33}"), (0x2FA04, "\u{9929}"), (0x2FA05, "\u{99a7}"), (0x2FA06, "\u{99c2}"),
    (0x2FA07, "\u{99fe}"), (0x2FA08, "\u{4bce}"), (0x2FA09, "\u{29b30}"),
    (0x2FA0A, "\u{9b12}"), (0x2FA0B, "\u{9c40}"), (0x2FA0C, "\u{9cfd}"), (0x2FA0D, "\u{4cce}"),
    (0x2FA0E, "\u{4ced}"), (0x2FA0F, "\u{9d67}"), (0x2FA10, "\u{2a0ce}"),
    (0x2FA11, "\u{4cf8}"), (0x2FA12, "\u{2a105}"), (0x2FA13, "\u{2a20e}"),
    (0x2FA14, "\u{2a291}"), (0x2FA15, "\u{9ebb}"), (0x2FA16, "\u{4d56}"),
    (0x2FA17, "\u{9ef9}"), (0x2FA18, "\u{9efe}"), (0x2FA19, "\u{9f05}"), (0x2FA1A, "\u{9f0f}"),
    (0x2FA1B, "\u{9f16}"), (0x2FA1C, "\u{9f3b}"), (0x2FA1D, "\u{2a600}"),
];

/// Nonzero canonical combining classes.
#[rustfmt::skip]
static CCC_TABLE: &[(u32, u8)] = &[
    (0x300, 230), (0x301, 230), (0x302, 230), (0x303, 230), (0x304, 230), (0x305, 230),
    (0x306, 230), (0x307, 230), (0x308, 230), (0x309, 230), (0x30A, 230), (0x30B, 230),
    (0x30C, 230), (0x30D, 230), (0x30E, 230), (0x30F, 230), (0x310, 230), (0x311, 230),
    (0x312, 230), (0x313, 230), (0x314, 230), (0x315, 232), (0x316, 220), (0x317, 220),
    (0x318, 220), (0x319, 220), (0x31A, 232), (0x31B, 216), (0x31C, 220), (0x31D, 220),
    (0x31E, 220), (0x31F, 220), (0x320, 220), (0x321, 202), (0x322, 202), (0x323, 220),
    (0x324, 220), (0x325, 220), (0x326, 220), (0x327, 202), (0x328, 202), (0x329, 220),
    (0x32A, 220), (0x32B, 220), (0x32C, 220), (0x32D, 220), (0x32E, 220), (0x32F, 220),
    (0x330, 220), (0x331, 220), (0x332, 220), (0x333, 220), (0x334, 1), (0x335, 1), (0x336, 1),
    (0x337, 1), (0x338, 1), (0x339, 220), (0x33A, 220), (0x33B, 220), (0x33C, 220),
    (0x33D, 230), (0x33E, 230), (0x33F, 230), (0x340, 230), (0x341, 230), (0x342, 230),
    (0x343, 230), (0x344, 230), (0x345, 240), (0x346, 230), (0x347, 220), (0x348, 220),
    (0x349, 220), (0x34A, 230), (0x34B, 230), (0x34C, 230), (0x34D, 220), (0x34E, 220),
    (0x350, 230), (0x351, 230), (0x352, 230), (0x353, 220), (0x354, 220), (0x355, 220),
    (0x356, 220), (0x357, 230), (0x358, 232), (0x359, 220), (0x35A, 220), (0x35B, 230),
    (0x35C, 233), (0x35D, 234), (0x35E, 234), (0x35F, 233), (0x360, 234), (0x361, 234),
    (0x362, 233), (0x363, 230), (0x364, 230), (0x365, 230), (0x366, 230), (0x367, 230),
    (0x368, 230), (0x369, 230), (0x36A, 230), (0x36B, 230), (0x36C, 230), (0x36D, 230),
    (0x36E, 230), (0x36F, 230), (0x483, 230), (0x484, 230), (0x485, 230), (0x486, 230),
    (0x487, 230), (0x591, 220), (0x592, 230), (0x593, 230), (0x594, 230), (0x595, 230),
    (0x596, 220), (0x597, 230), (0x598, 230), (0x599, 230), (0x59A, 222), (0x59B, 220),
    (0x59C, 230), (0x59D, 230), (0x59E, 230), (0x59F, 230), (0x5A0, 230), (0x5A1, 230),
    (0x5A2, 220), (0x5A3, 220), (0x5A4, 220), (0x5A5, 220), (0x5A6, 220), (0x5A7, 220),
    (0x5A8, 230), (0x5A9, 230), (0x5AA, 220), (0x5AB, 230), (0x5AC, 230), (0x5AD, 222),
    (0x5AE, 228), (0x5AF, 230), (0x5B0, 10), (0x5B1, 11), (0x5B2, 12), (0x5B3, 13),
    (0x5B4, 14), (0x5B5, 15), (0x5B6, 16), (0x5B7, 17), (0x5B8, 18), (0x5B9, 19), (0x5BA, 19),
    (0x5BB, 20), (0x5BC, 21), (0x5BD, 22), (0x5BF, 23), (0x5C1, 24), (0x5C2, 25), (0x5C4, 230),
    (0x5C5, 220), (0x5C7, 18), (0x610, 230), (0x611, 230), (0x612, 230), (0x613, 230),
    (0x614, 230), (0x615, 230), (0x616, 230), (0x617, 230), (0x618, 30), (0x619, 31),
    (0x61A, 32), (0x64B, 27), (0x64C, 28), (0x64D, 29), (0x64E, 30), (0x64F, 31), (0x650, 32),
    (0x651, 33), (0x652, 34), (0x653, 230), (0x654, 230), (0x655, 220), (0x656, 220),
    (0x657, 230), (0x658, 230), (0x659, 230), (0x65A, 230), (0x65B, 230), (0x65C, 220),
    (0x65D, 230), (0x65E, 230), (0x65F, 220), (0x670, 35), (0x6D6, 230), (0x6D7, 230),
    (0x6D8, 230), (0x6D9, 230), (0x6DA, 230), (0x6DB, 230), (0x6DC, 230), (0x6DF, 230),
    (0x6E0, 230), (0x6E1, 230), (0x6E2, 230), (0x6E3, 220), (0x6E4, 230), (0x6E7, 230),
    (0x6E8, 230), (0x6EA, 220), (0x6EB, 230), (0x6EC, 230), (0x6ED, 220), (0x711, 36),
    (0x730, 230), (0x731, 220), (0x732, 230), (0x733, 230), (0x734, 220), (0x735, 230),
    (0x736, 230), (0x737, 220), (0x738, 220), (0x739, 220), (0x73A, 230), (0x73B, 220),
    (0x73C, 220), (0x73D, 230), (0x73E, 220), (0x73F, 230), (0x740, 230), (0x741, 230),
    (0x742, 220), (0x743, 230), (0x744, 220), (0x745, 230), (0x746, 220), (0x747, 230),
    (0x748, 220), (0x749, 230), (0x74A, 230), (0x7EB, 230), (0x7EC, 230), (0x7ED, 230),
    (0x7EE, 230), (0x7EF, 230), (0x7F0, 230), (0x7F1, 230), (0x7F2, 220), (0x7F3, 230),
    (0x7FD, 220), (0x816, 230), (0x817, 230), (0x818, 230), (0x819, 230), (0x81B, 230),
    (0x81C, 230), (0x81D, 230), (0x81E, 230), (0x81F, 230), (0x820, 230), (0x821, 230),
    (0x822, 230), (0x823, 230), (0x825, 230), (0x826, 230), (0x827, 230), (0x829, 230),
    (0x82A, 230), (0x82B, 230), (0x82C, 230), (0x82D, 230), (0x859, 220), (0x85A, 220),
    (0x85B, 220), (0x898, 230), (0x899, 220), (0x89A, 220), (0x89B, 220), (0x89C, 230),
    (0x89D, 230), (0x89E, 230), (0x89F, 230), (0x8CA, 230), (0x8CB, 230), (0x8CC, 230),
    (0x8CD, 230), (0x8CE, 230), (0x8CF, 220), (0x8D0, 220), (0x8D1, 220), (0x8D2, 220),
    (0x8D3, 220), (0x8D4, 230), (0x8D5, 230), (0x8D6, 230), (0x8D7, 230), (0x8D8, 230),
    (0x8D9, 230), (0x8DA, 230), (0x8DB, 230), (0x8DC, 230), (0x8DD, 230), (0x8DE, 230),
    (0x8DF, 230), (0x8E0, 230), (0x8E1, 230), (0x8E3, 220), (0x8E4, 230), (0x8E5, 230),
    (0x8E6, 220), (0x8E7, 230), (0x8E8, 230), (0x8E9, 220), (0x8EA, 230), (0x8EB, 230),
    (0x8EC, 230), (0x8ED, 220), (0x8EE, 220), (0x8EF, 220), (0x8F0, 27), (0x8F1, 28),
    (0x8F2, 29), (0x8F3, 230), (0x8F4, 230), (0x8F5, 230), (0x8F6, 220), (0x8F7, 230),
    (0x8F8, 230), (0x8F9, 220), (0x8FA, 220), (0x8FB, 230), (0x8FC, 230), (0x8FD, 230),
    (0x8FE, 230), (0x8FF, 230), (0x93C, 7), (0x94D, 9), (0x951, 230), (0x952, 220),
    (0x953, 230), (0x954, 230), (0x9BC, 7), (0x9CD, 9), (0x9FE, 230), (0xA3C, 7), (0xA4D, 9),
    (0xABC, 7), (0xACD, 9), (0xB3C, 7), (0xB4D, 9), (0xBCD, 9), (0xC3C, 7), (0xC4D, 9),
    (0xC55, 84), (0xC56, 91), (0xCBC, 7), (0xCCD, 9), (0xD3B, 9), (0xD3C, 9), (0xD4D, 9),
    (0xDCA, 9), (0xE38, 103), (0xE39, 103), (0xE3A, 9), (0xE48, 107), (0xE49, 107),
    (0xE4A, 107), (0xE4B, 107), (0xEB8, 118), (0xEB9, 118), (0xEBA, 9), (0xEC8, 122),
    (0xEC9, 122), (0xECA, 122), (0xECB, 122), (0xF18, 220), (0xF19, 220), (0xF35, 220),
    (0xF37, 220), (0xF39, 216), (0xF71, 129), (0xF72, 130), (0xF74, 132), (0xF7A, 130),
    (0xF7B, 130), (0xF7C, 130), (0xF7D, 130), (0xF80, 130), (0xF82, 230), (0xF83, 230),
    (0xF84, 9), (0xF86, 230), (0xF87, 230), (0xFC6, 220), (0x1037, 7), (0x1039, 9),
    (0x103A, 9), (0x108D, 220), (0x135D, 230), (0x135E, 230), (0x135F, 230), (0x1714, 9),
    (0x1715, 9), (0x1734, 9), (0x17D2, 9), (0x17DD, 230), (0x18A9, 228), (0x1939, 222),
    (0x193A, 230), (0x193B, 220), (0x1A17, 230), (0x1A18, 220), (0x1A60, 9), (0x1A75, 230),
    (0x1A76, 230), (0x1A77, 230), (0x1A78, 230), (0x1A79, 230), (0x1A7A, 230), (0x1A7B, 230),
    (0x1A7C, 230), (0x1A7F, 220), (0x1AB0, 230), (0x1AB1, 230), (0x1AB2, 230), (0x1AB3, 230),
    (0x1AB4, 230), (0x1AB5, 220), (0x1AB6, 220), (0x1AB7, 220), (0x1AB8, 220), (0x1AB9, 220),
    (0x1ABA, 220), (0x1ABB, 230), (0x1ABC, 230), (0x1ABD, 220), (0x1ABF, 220), (0x1AC0, 220),
    (0x1AC1, 230), (0x1AC2, 230), (0x1AC3, 220), (0x1AC4, 220), (0x1AC5, 230), (0x1AC6, 230),
    (0x1AC7, 230), (0x1AC8, 230), (0x1AC9, 230), (0x1ACA, 220), (0x1ACB, 230), (0x1ACC, 230),
    (0x1ACD, 230), (0x1ACE, 230), (0x1B34, 7), (0x1B44, 9), (0x1B6B, 230), (0x1B6C, 220),
    (0x1B6D, 230), (0x1B6E, 230), (0x1B6F, 230), (0x1B70, 230), (0x1B71, 230), (0x1B72, 230),
    (0x1B73, 230), (0x1BAA, 9), (0x1BAB, 9), (0x1BE6, 7), (0x1BF2, 9), (0x1BF3, 9),
    (0x1C37, 7), (0x1CD0, 230), (0x1CD1, 230), (0x1CD2, 230), (0x1CD4, 1), (0x1CD5, 220),
    (0x1CD6, 220), (0x1CD7, 220), (0x1CD8, 220), (0x1CD9, 220), (0x1CDA, 230), (0x1CDB, 230),
    (0x1CDC, 220), (0x1CDD, 220), (0x1CDE, 220), (0x1CDF, 220), (0x1CE0, 230), (0x1CE2, 1),
    (0x1CE3, 1), (0x1CE4, 1), (0x1CE5, 1), (0x1CE6, 1), (0x1CE7, 1), (0x1CE8, 1),
    (0x1CED, 220), (0x1CF4, 230), (0x1CF8, 230), (0x1CF9, 230), (0x1DC0, 230), (0x1DC1, 230),
    (0x1DC2, 220), (0x1DC3, 230), (0x1DC4, 230), (0x1DC5, 230), (0x1DC6, 230), (0x1DC7, 230),
    (0x1DC8, 230), (0x1DC9, 230), (0x1DCA, 220), (0x1DCB, 230), (0x1DCC, 230), (0x1DCD, 234),
    (0x1DCE, 214), (0x1DCF, 220), (0x1DD0, 202), (0x1DD1, 230), (0x1DD2, 230), (0x1DD3, 230),
    (0x1DD4, 230), (0x1DD5, 230), (0x1DD6, 230), (0x1DD7, 230), (0x1DD8, 230), (0x1DD9, 230),
    (0x1DDA, 230), (0x1DDB, 230), (0x1DDC, 230), (0x1DDD, 230), (0x1DDE, 230), (0x1DDF, 230),
    (0x1DE0, 230), (0x1DE1, 230), (0x1DE2, 230), (0x1DE3, 230), (0x1DE4, 230), (0x1DE5, 230),
    (0x1DE6, 230), (0x1DE7, 230), (0x1DE8, 230), (0x1DE9, 230), (0x1DEA, 230), (0x1DEB, 230),
    (0x1DEC, 230), (0x1DED, 230), (0x1DEE, 230), (0x1DEF, 230), (0x1DF0, 230), (0x1DF1, 230),
    (0x1DF2, 230), (0x1DF3, 230), (0x1DF4, 230), (0x1DF5, 230), (0x1DF6, 232), (0x1DF7, 228),
    (0x1DF8, 228), (0x1DF9, 220), (0x1DFA, 218), (0x1DFB, 230), (0x1DFC, 233), (0x1DFD, 220),
    (0x1DFE, 230), (0x1DFF, 220), (0x20D0, 230), (0x20D1, 230), (0x20D2, 1), (0x20D3, 1),
    (0x20D4, 230), (0x20D5, 230), (0x20D6, 230), (0x20D7, 230), (0x20D8, 1), (0x20D9, 1),
    (0x20DA, 1), (0x20DB, 230), (0x20DC, 230), (0x20E1, 230), (0x20E5, 1), (0x20E6, 1),
    (0x20E7, 230), (0x20E8, 220), (0x20E9, 230), (0x20EA, 1), (0x20EB, 1), (0x20EC, 220),
    (0x20ED, 220), (0x20EE, 220), (0x20EF, 220), (0x20F0, 230), (0x2CEF, 230), (0x2CF0, 230),
    (0x2CF1, 230), (0x2D7F, 9), (0x2DE0, 230), (0x2DE1, 230), (0x2DE2, 230), (0x2DE3, 230),
    (0x2DE4, 230), (0x2DE5, 230), (0x2DE6, 230), (0x2DE7, 230), (0x2DE8, 230), (0x2DE9, 230),
    (0x2DEA, 230), (0x2DEB, 230), (0x2DEC, 230), (0x2DED, 230), (0x2DEE, 230), (0x2DEF, 230),
    (0x2DF0, 230), (0x2DF1, 230), (0x2DF2, 230), (0x2DF3, 230), (0x2DF4, 230), (0x2DF5, 230),
    (0x2DF6, 230), (0x2DF7, 230), (0x2DF8, 230), (0x2DF9, 230), (0x2DFA, 230), (0x2DFB, 230),
    (0x2DFC, 230), (0x2DFD, 230), (0x2DFE, 230), (0x2DFF, 230), (0x302A, 218), (0x302B, 228),
    (0x302C, 232), (0x302D, 222), (0x302E, 224), (0x302F, 224), (0x3099, 8), (0x309A, 8),
    (0xA66F, 230), (0xA674, 230), (0xA675, 230), (0xA676, 230), (0xA677, 230), (0xA678, 230),
    (0xA679, 230), (0xA67A, 230), (0xA67B, 230), (0xA67C, 230), (0xA67D, 230), (0xA69E, 230),
    (0xA69F, 230), (0xA6F0, 230), (0xA6F1, 230), (0xA806, 9), (0xA82C, 9), (0xA8C4, 9),
    (0xA8E0, 230), (0xA8E1, 230), (0xA8E2, 230), (0xA8E3, 230), (0xA8E4, 230), (0xA8E5, 230),
    (0xA8E6, 230), (0xA8E7, 230), (0xA8E8, 230), (0xA8E9, 230), (0xA8EA, 230), (0xA8EB, 230),
    (0xA8EC, 230), (0xA8ED, 230), (0xA8EE, 230), (0xA8EF, 230), (0xA8F0, 230), (0xA8F1, 230),
    (0xA92B, 220), (0xA92C, 220), (0xA92D, 220), (0xA953, 9), (0xA9B3, 7), (0xA9C0, 9),
    (0xAAB0, 230), (0xAAB2, 230), (0xAAB3, 230), (0xAAB4, 220), (0xAAB7, 230), (0xAAB8, 230),
    (0xAABE, 230), (0xAABF, 230), (0xAAC1, 230), (0xAAF6, 9), (0xABED, 9), (0xFB1E, 26),
    (0xFE20, 230), (0xFE21, 230), (0xFE22, 230), (0xFE23, 230), (0xFE24, 230), (0xFE25, 230),
    (0xFE26, 230), (0xFE27, 220), (0xFE28, 220), (0xFE29, 220), (0xFE2A, 220), (0xFE2B, 220),
    (0xFE2C, 220), (0xFE2D, 220), (0xFE2E, 230), (0xFE2F, 230), (0x101FD, 220), (0x102E0, 220),
    (0x10376, 230), (0x10377, 230), (0x10378, 230), (0x10379, 230), (0x1037A, 230),
    (0x10A0D, 220), (0x10A0F, 230), (0x10A38, 230), (0x10A39, 1), (0x10A3A, 220), (0x10A3F, 9),
    (0x10AE5, 230), (0x10AE6, 220), (0x10D24, 230), (0x10D25, 230), (0x10D26, 230),
    (0x10D27, 230), (0x10EAB, 230), (0x10EAC, 230), (0x10EFD, 220), (0x10EFE, 220),
    (0x10EFF, 220), (0x10F46, 220), (0x10F47, 220), (0x10F48, 230), (0x10F49, 230),
    (0x10F4A, 230), (0x10F4B, 220), (0x10F4C, 230), (0x10F4D, 220), (0x10F4E, 220),
    (0x10F4F, 220), (0x10F50, 220), (0x10F82, 230), (0x10F83, 220), (0x10F84, 230),
    (0x10F85, 220), (0x11046, 9), (0x11070, 9), (0x1107F, 9), (0x110B9, 9), (0x110BA, 7),
    (0x11100, 230), (0x11101, 230), (0x11102, 230), (0x11133, 9), (0x11134, 9), (0x11173, 7),
    (0x111C0, 9), (0x111CA, 7), (0x11235, 9), (0x11236, 7), (0x112E9, 7), (0x112EA, 9),
    (0x1133B, 7), (0x1133C, 7), (0x1134D, 9), (0x11366, 230), (0x11367, 230), (0x11368, 230),
    (0x11369, 230), (0x1136A, 230), (0x1136B, 230), (0x1136C, 230), (0x11370, 230),
    (0x11371, 230), (0x11372, 230), (0x11373, 230), (0x11374, 230), (0x11442, 9), (0x11446, 7),
    (0x1145E, 230), (0x114C2, 9), (0x114C3, 7), (0x115BF, 9), (0x115C0, 7), (0x1163F, 9),
    (0x116B6, 9), (0x116B7, 7), (0x1172B, 9), (0x11839, 9), (0x1183A, 7), (0x1193D, 9),
    (0x1193E, 9), (0x11943, 7), (0x119E0, 9), (0x11A34, 9), (0x11A47, 9), (0x11A99, 9),
    (0x11C3F, 9), (0x11D42, 7), (0x11D44, 9), (0x11D45, 9), (0x11D97, 9), (0x11F41, 9),
    (0x11F42, 9), (0x16AF0, 1), (0x16AF1, 1), (0x16AF2, 1), (0x16AF3, 1), (0x16AF4, 1),
    (0x16B30, 230), (0x16B31, 230), (0x16B32, 230), (0x16B33, 230), (0x16B34, 230),
    (0x16B35, 230), (0x16B36, 230), (0x16FF0, 6), (0x16FF1, 6), (0x1BC9E, 1), (0x1D165, 216),
    (0x1D166, 216), (0x1D167, 1), (0x1D168, 1), (0x1D169, 1), (0x1D16D, 226), (0x1D16E, 216),
    (0x1D16F, 216), (0x1D170, 216), (0x1D171, 216), (0x1D172, 216), (0x1D17B, 220),
    (0x1D17C, 220), (0x1D17D, 220), (0x1D17E, 220), (0x1D17F, 220), (0x1D180, 220),
    (0x1D181, 220), (0x1D182, 220), (0x1D185, 230), (0x1D186, 230), (0x1D187, 230),
    (0x1D188, 230), (0x1D189, 230), (0x1D18A, 220), (0x1D18B, 220), (0x1D1AA, 230),
    (0x1D1AB, 230), (0x1D1AC, 230), (0x1D1AD, 230), (0x1D242, 230), (0x1D243, 230),
    (0x1D244, 230), (0x1E000, 230), (0x1E001, 230), (0x1E002, 230), (0x1E003, 230),
    (0x1E004, 230), (0x1E005, 230), (0x1E006, 230), (0x1E008, 230), (0x1E009, 230),
    (0x1E00A, 230), (0x1E00B, 230), (0x1E00C, 230), (0x1E00D, 230), (0x1E00E, 230),
    (0x1E00F, 230), (0x1E010, 230), (0x1E011, 230), (0x1E012, 230), (0x1E013, 230),
    (0x1E014, 230), (0x1E015, 230), (0x1E016, 230), (0x1E017, 230), (0x1E018, 230),
    (0x1E01B, 230), (0x1E01C, 230), (0x1E01D, 230), (0x1E01E, 230), (0x1E01F, 230),
    (0x1E020, 230), (0x1E021, 230), (0x1E023, 230), (0x1E024, 230), (0x1E026, 230),
    (0x1E027, 230), (0x1E028, 230), (0x1E029, 230), (0x1E02A, 230), (0x1E08F, 230),
    (0x1E130, 230), (0x1E131, 230), (0x1E132, 230), (0x1E133, 230), (0x1E134, 230),
    (0x1E135, 230), (0x1E136, 230), (0x1E2AE, 230), (0x1E2EC, 230), (0x1E2ED, 230),
    (0x1E2EE, 230), (0x1E2EF, 230), (0x1E4EC, 232), (0x1E4ED, 232), (0x1E4EE, 220),
    (0x1E4EF, 230), (0x1E8D0, 220), (0x1E8D1, 220), (0x1E8D2, 220), (0x1E8D3, 220),
    (0x1E8D4, 220), (0x1E8D5, 220), (0x1E8D6, 220), (0x1E944, 230), (0x1E945, 230),
    (0x1E946, 230), (0x1E947, 230), (0x1E948, 230), (0x1E949, 230), (0x1E94A, 7),
];

/// Canonical composition pairs `(starter, combining, composite)`.
#[rustfmt::skip]
static COMPOSE_TABLE: &[(u32, u32, u32)] = &[
    (0x3C, 0x338, 0x226E), (0x3D, 0x338, 0x2260), (0x3E, 0x338, 0x226F), (0x41, 0x300, 0xC0),
    (0x41, 0x301, 0xC1), (0x41, 0x302, 0xC2), (0x41, 0x303, 0xC3), (0x41, 0x304, 0x100),
    (0x41, 0x306, 0x102), (0x41, 0x307, 0x226), (0x41, 0x308, 0xC4), (0x41, 0x309, 0x1EA2),
    (0x41, 0x30A, 0xC5), (0x41, 0x30C, 0x1CD), (0x41, 0x30F, 0x200), (0x41, 0x311, 0x202),
    (0x41, 0x323, 0x1EA0), (0x41, 0x325, 0x1E00), (0x41, 0x328, 0x104), (0x42, 0x307, 0x1E02),
    (0x42, 0x323, 0x1E04), (0x42, 0x331, 0x1E06), (0x43, 0x301, 0x106), (0x43, 0x302, 0x108),
    (0x43, 0x307, 0x10A), (0x43, 0x30C, 0x10C), (0x43, 0x327, 0xC7), (0x44, 0x307, 0x1E0A),
    (0x44, 0x30C, 0x10E), (0x44, 0x323, 0x1E0C), (0x44, 0x327, 0x1E10), (0x44, 0x32D, 0x1E12),
    (0x44, 0x331, 0x1E0E), (0x45, 0x300, 0xC8), (0x45, 0x301, 0xC9), (0x45, 0x302, 0xCA),
    (0x45, 0x303, 0x1EBC), (0x45, 0x304, 0x112), (0x45, 0x306, 0x114), (0x45, 0x307, 0x116),
    (0x45, 0x308, 0xCB), (0x45, 0x309, 0x1EBA), (0x45, 0x30C, 0x11A), (0x45, 0x30F, 0x204),
    (0x45, 0x311, 0x206), (0x45, 0x323, 0x1EB8), (0x45, 0x327, 0x228), (0x45, 0x328, 0x118),
    (0x45, 0x32D, 0x1E18), (0x45, 0x330, 0x1E1A), (0x46, 0x307, 0x1E1E), (0x47, 0x301, 0x1F4),
    (0x47, 0x302, 0x11C), (0x47, 0x304, 0x1E20), (0x47, 0x306, 0x11E), (0x47, 0x307, 0x120),
    (0x47, 0x30C, 0x1E6), (0x47, 0x327, 0x122), (0x48, 0x302, 0x124), (0x48, 0x307, 0x1E22),
    (0x48, 0x308, 0x1E26), (0x48, 0x30C, 0x21E), (0x48, 0x323, 0x1E24), (0x48, 0x327, 0x1E28),
    (0x48, 0x32E, 0x1E2A), (0x49, 0x300, 0xCC), (0x49, 0x301, 0xCD), (0x49, 0x302, 0xCE),
    (0x49, 0x303, 0x128), (0x49, 0x304, 0x12A), (0x49, 0x306, 0x12C), (0x49, 0x307, 0x130),
    (0x49, 0x308, 0xCF), (0x49, 0x309, 0x1EC8), (0x49, 0x30C, 0x1CF), (0x49, 0x30F, 0x208),
    (0x49, 0x311, 0x20A), (0x49, 0x323, 0x1ECA), (0x49, 0x328, 0x12E), (0x49, 0x330, 0x1E2C),
    (0x4A, 0x302, 0x134), (0x4B, 0x301, 0x1E30), (0x4B, 0x30C, 0x1E8), (0x4B, 0x323, 0x1E32),
    (0x4B, 0x327, 0x136), (0x4B, 0x331, 0x1E34), (0x4C, 0x301, 0x139), (0x4C, 0x30C, 0x13D),
    (0x4C, 0x323, 0x1E36), (0x4C, 0x327, 0x13B), (0x4C, 0x32D, 0x1E3C), (0x4C, 0x331, 0x1E3A),
    (0x4D, 0x301, 0x1E3E), (0x4D, 0x307, 0x1E40), (0x4D, 0x323, 0x1E42), (0x4E, 0x300, 0x1F8),
    (0x4E, 0x301, 0x143), (0x4E, 0x303, 0xD1), (0x4E, 0x307, 0x1E44), (0x4E, 0x30C, 0x147),
    (0x4E, 0x323, 0x1E46), (0x4E, 0x327, 0x145), (0x4E, 0x32D, 0x1E4A), (0x4E, 0x331, 0x1E48),
    (0x4F, 0x300, 0xD2), (0x4F, 0x301, 0xD3), (0x4F, 0x302, 0xD4), (0x4F, 0x303, 0xD5),
    (0x4F, 0x304, 0x14C), (0x4F, 0x306, 0x14E), (0x4F, 0x307, 0x22E), (0x4F, 0x308, 0xD6),
    (0x4F, 0x309, 0x1ECE), (0x4F, 0x30B, 0x150), (0x4F, 0x30C, 0x1D1), (0x4F, 0x30F, 0x20C),
    (0x4F, 0x311, 0x20E), (0x4F, 0x31B, 0x1A0), (0x4F, 0x323, 0x1ECC), (0x4F, 0x328, 0x1EA),
    (0x50, 0x301, 0x1E54), (0x50, 0x307, 0x1E56), (0x52, 0x301, 0x154), (0x52, 0x307, 0x1E58),
    (0x52, 0x30C, 0x158), (0x52, 0x30F, 0x210), (0x52, 0x311, 0x212), (0x52, 0x323, 0x1E5A),
    (0x52, 0x327, 0x156), (0x52, 0x331, 0x1E5E), (0x53, 0x301, 0x15A), (0x53, 0x302, 0x15C),
    (0x53, 0x307, 0x1E60), (0x53, 0x30C, 0x160), (0x53, 0x323, 0x1E62), (0x53, 0x326, 0x218),
    (0x53, 0x327, 0x15E), (0x54, 0x307, 0x1E6A), (0x54, 0x30C, 0x164), (0x54, 0x323, 0x1E6C),
    (0x54, 0x326, 0x21A), (0x54, 0x327, 0x162), (0x54, 0x32D, 0x1E70), (0x54, 0x331, 0x1E6E),
    (0x55, 0x300, 0xD9), (0x55, 0x301, 0xDA), (0x55, 0x302, 0xDB), (0x55, 0x303, 0x168),
    (0x55, 0x304, 0x16A), (0x55, 0x306, 0x16C), (0x55, 0x308, 0xDC), (0x55, 0x309, 0x1EE6),
    (0x55, 0x30A, 0x16E), (0x55, 0x30B, 0x170), (0x55, 0x30C, 0x1D3), (0x55, 0x30F, 0x214),
    (0x55, 0x311, 0x216), (0x55, 0x31B, 0x1AF), (0x55, 0x323, 0x1EE4), (0x55, 0x324, 0x1E72),
    (0x55, 0x328, 0x172), (0x55, 0x32D, 0x1E76), (0x55, 0x330, 0x1E74), (0x56, 0x303, 0x1E7C),
    (0x56, 0x323, 0x1E7E), (0x57, 0x300, 0x1E80), (0x57, 0x301, 0x1E82), (0x57, 0x302, 0x174),
    (0x57, 0x307, 0x1E86), (0x57, 0x308, 0x1E84), (0x57, 0x323, 0x1E88), (0x58, 0x307, 0x1E8A),
    (0x58, 0x308, 0x1E8C), (0x59, 0x300, 0x1EF2), (0x59, 0x301, 0xDD), (0x59, 0x302, 0x176),
    (0x59, 0x303, 0x1EF8), (0x59, 0x304, 0x232), (0x59, 0x307, 0x1E8E), (0x59, 0x308, 0x178),
    (0x59, 0x309, 0x1EF6), (0x59, 0x323, 0x1EF4), (0x5A, 0x301, 0x179), (0x5A, 0x302, 0x1E90),
    (0x5A, 0x307, 0x17B), (0x5A, 0x30C, 0x17D), (0x5A, 0x323, 0x1E92), (0x5A, 0x331, 0x1E94),
    (0x61, 0x300, 0xE0), (0x61, 0x301, 0xE1), (0x61, 0x302, 0xE2), (0x61, 0x303, 0xE3),
    (0x61, 0x304, 0x101), (0x61, 0x306, 0x103), (0x61, 0x307, 0x227), (0x61, 0x308, 0xE4),
    (0x61, 0x309, 0x1EA3), (0x61, 0x30A, 0xE5), (0x61, 0x30C, 0x1CE), (0x61, 0x30F, 0x201),
    (0x61, 0x311, 0x203), (0x61, 0x323, 0x1EA1), (0x61, 0x325, 0x1E01), (0x61, 0x328, 0x105),
    (0x62, 0x307, 0x1E03), (0x62, 0x323, 0x1E05), (0x62, 0x331, 0x1E07), (0x63, 0x301, 0x107),
    (0x63, 0x302, 0x109), (0x63, 0x307, 0x10B), (0x63, 0x30C, 0x10D), (0x63, 0x327, 0xE7),
    (0x64, 0x307, 0x1E0B), (0x64, 0x30C, 0x10F), (0x64, 0x323, 0x1E0D), (0x64, 0x327, 0x1E11),
    (0x64, 0x32D, 0x1E13), (0x64, 0x331, 0x1E0F), (0x65, 0x300, 0xE8), (0x65, 0x301, 0xE9),
    (0x65, 0x302, 0xEA), (0x65, 0x303, 0x1EBD), (0x65, 0x304, 0x113), (0x65, 0x306, 0x115),
    (0x65, 0x307, 0x117), (0x65, 0x308, 0xEB), (0x65, 0x309, 0x1EBB), (0x65, 0x30C, 0x11B),
    (0x65, 0x30F, 0x205), (0x65, 0x311, 0x207), (0x65, 0x323, 0x1EB9), (0x65, 0x327, 0x229),
    (0x65, 0x328, 0x119), (0x65, 0x32D, 0x1E19), (0x65, 0x330, 0x1E1B), (0x66, 0x307, 0x1E1F),
    (0x67, 0x301, 0x1F5), (0x67, 0x302, 0x11D), (0x67, 0x304, 0x1E21), (0x67, 0x306, 0x11F),
    (0x67, 0x307, 0x121), (0x67, 0x30C, 0x1E7), (0x67, 0x327, 0x123), (0x68, 0x302, 0x125),
    (0x68, 0x307, 0x1E23), (0x68, 0x308, 0x1E27), (0x68, 0x30C, 0x21F), (0x68, 0x323, 0x1E25),
    (0x68, 0x327, 0x1E29), (0x68, 0x32E, 0x1E2B), (0x68, 0x331, 0x1E96), (0x69, 0x300, 0xEC),
    (0x69, 0x301, 0xED), (0x69, 0x302, 0xEE), (0x69, 0x303, 0x129), (0x69, 0x304, 0x12B),
    (0x69, 0x306, 0x12D), (0x69, 0x308, 0xEF), (0x69, 0x309, 0x1EC9), (0x69, 0x30C, 0x1D0),
    (0x69, 0x30F, 0x209), (0x69, 0x311, 0x20B), (0x69, 0x323, 0x1ECB), (0x69, 0x328, 0x12F),
    (0x69, 0x330, 0x1E2D), (0x6A, 0x302, 0x135), (0x6A, 0x30C, 0x1F0), (0x6B, 0x301, 0x1E31),
    (0x6B, 0x30C, 0x1E9), (0x6B, 0x323, 0x1E33), (0x6B, 0x327, 0x137), (0x6B, 0x331, 0x1E35),
    (0x6C, 0x301, 0x13A), (0x6C, 0x30C, 0x13E), (0x6C, 0x323, 0x1E37), (0x6C, 0x327, 0x13C),
    (0x6C, 0x32D, 0x1E3D), (0x6C, 0x331, 0x1E3B), (0x6D, 0x301, 0x1E3F), (0x6D, 0x307, 0x1E41),
    (0x6D, 0x323, 0x1E43), (0x6E, 0x300, 0x1F9), (0x6E, 0x301, 0x144), (0x6E, 0x303, 0xF1),
    (0x6E, 0x307, 0x1E45), (0x6E, 0x30C, 0x148), (0x6E, 0x323, 0x1E47), (0x6E, 0x327, 0x146),
    (0x6E, 0x32D, 0x1E4B), (0x6E, 0x331, 0x1E49), (0x6F, 0x300, 0xF2), (0x6F, 0x301, 0xF3),
    (0x6F, 0x302, 0xF4), (0x6F, 0x303, 0xF5), (0x6F, 0x304, 0x14D), (0x6F, 0x306, 0x14F),
    (0x6F, 0x307, 0x22F), (0x6F, 0x308, 0xF6), (0x6F, 0x309, 0x1ECF), (0x6F, 0x30B, 0x151),
    (0x6F, 0x30C, 0x1D2), (0x6F, 0x30F, 0x20D), (0x6F, 0x311, 0x20F), (0x6F, 0x31B, 0x1A1),
    (0x6F, 0x323, 0x1ECD), (0x6F, 0x328, 0x1EB), (0x70, 0x301, 0x1E55), (0x70, 0x307, 0x1E57),
    (0x72, 0x301, 0x155), (0x72, 0x307, 0x1E59), (0x72, 0x30C, 0x159), (0x72, 0x30F, 0x211),
    (0x72, 0x311, 0x213), (0x72, 0x323, 0x1E5B), (0x72, 0x327, 0x157), (0x72, 0x331, 0x1E5F),
    (0x73, 0x301, 0x15B), (0x73, 0x302, 0x15D), (0x73, 0x307, 0x1E61), (0x73, 0x30C, 0x161),
    (0x73, 0x323, 0x1E63), (0x73, 0x326, 0x219), (0x73, 0x327, 0x15F), (0x74, 0x307, 0x1E6B),
    (0x74, 0x308, 0x1E97), (0x74, 0x30C, 0x165), (0x74, 0x323, 0x1E6D), (0x74, 0x326, 0x21B),
    (0x74, 0x327, 0x163), (0x74, 0x32D, 0x1E71), (0x74, 0x331, 0x1E6F), (0x75, 0x300, 0xF9),
    (0x75, 0x301, 0xFA), (0x75, 0x302, 0xFB), (0x75, 0x303, 0x169), (0x75, 0x304, 0x16B),
    (0x75, 0x306, 0x16D), (0x75, 0x308, 0xFC), (0x75, 0x309, 0x1EE7), (0x75, 0x30A, 0x16F),
    (0x75, 0x30B, 0x171), (0x75, 0x30C, 0x1D4), (0x75, 0x30F, 0x215), (0x75, 0x311, 0x217),
    (0x75, 0x31B, 0x1B0), (0x75, 0x323, 0x1EE5), (0x75, 0x324, 0x1E73), (0x75, 0x328, 0x173),
    (0x75, 0x32D, 0x1E77), (0x75, 0x330, 0x1E75), (0x76, 0x303, 0x1E7D), (0x76, 0x323, 0x1E7F),
    (0x77, 0x300, 0x1E81), (0x77, 0x301, 0x1E83), (0x77, 0x302, 0x175), (0x77, 0x307, 0x1E87),
    (0x77, 0x308, 0x1E85), (0x77, 0x30A, 0x1E98), (0x77, 0x323, 0x1E89), (0x78, 0x307, 0x1E8B),
    (0x78, 0x308, 0x1E8D), (0x79, 0x300, 0x1EF3), (0x79, 0x301, 0xFD), (0x79, 0x302, 0x177),
    (0x79, 0x303, 0x1EF9), (0x79, 0x304, 0x233), (0x79, 0x307, 0x1E8F), (0x79, 0x308, 0xFF),
    (0x79, 0x309, 0x1EF7), (0x79, 0x30A, 0x1E99), (0x79, 0x323, 0x1EF5), (0x7A, 0x301, 0x17A),
    (0x7A, 0x302, 0x1E91), (0x7A, 0x307, 0x17C), (0x7A, 0x30C, 0x17E), (0x7A, 0x323, 0x1E93),
    (0x7A, 0x331, 0x1E95), (0xA8, 0x300, 0x1FED), (0xA8, 0x301, 0x385), (0xA8, 0x342, 0x1FC1),
    (0xC2, 0x300, 0x1EA6), (0xC2, 0x301, 0x1EA4), (0xC2, 0x303, 0x1EAA), (0xC2, 0x309, 0x1EA8),
    (0xC4, 0x304, 0x1DE), (0xC5, 0x301, 0x1FA), (0xC6, 0x301, 0x1FC), (0xC6, 0x304, 0x1E2),
    (0xC7, 0x301, 0x1E08), (0xCA, 0x300, 0x1EC0), (0xCA, 0x301, 0x1EBE), (0xCA, 0x303, 0x1EC4),
    (0xCA, 0x309, 0x1EC2), (0xCF, 0x301, 0x1E2E), (0xD4, 0x300, 0x1ED2), (0xD4, 0x301, 0x1ED0),
    (0xD4, 0x303, 0x1ED6), (0xD4, 0x309, 0x1ED4), (0xD5, 0x301, 0x1E4C), (0xD5, 0x304, 0x22C),
    (0xD5, 0x308, 0x1E4E), (0xD6, 0x304, 0x22A), (0xD8, 0x301, 0x1FE), (0xDC, 0x300, 0x1DB),
    (0xDC, 0x301, 0x1D7), (0xDC, 0x304, 0x1D5), (0xDC, 0x30C, 0x1D9), (0xE2, 0x300, 0x1EA7),
    (0xE2, 0x301, 0x1EA5), (0xE2, 0x303, 0x1EAB), (0xE2, 0x309, 0x1EA9), (0xE4, 0x304, 0x1DF),
    (0xE5, 0x301, 0x1FB), (0xE6, 0x301, 0x1FD), (0xE6, 0x304, 0x1E3), (0xE7, 0x301, 0x1E09),
    (0xEA, 0x300, 0x1EC1), (0xEA, 0x301, 0x1EBF), (0xEA, 0x303, 0x1EC5), (0xEA, 0x309, 0x1EC3),
    (0xEF, 0x301, 0x1E2F), (0xF4, 0x300, 0x1ED3), (0xF4, 0x301, 0x1ED1), (0xF4, 0x303, 0x1ED7),
    (0xF4, 0x309, 0x1ED5), (0xF5, 0x301, 0x1E4D), (0xF5, 0x304, 0x22D), (0xF5, 0x308, 0x1E4F),
    (0xF6, 0x304, 0x22B), (0xF8, 0x301, 0x1FF), (0xFC, 0x300, 0x1DC), (0xFC, 0x301, 0x1D8),
    (0xFC, 0x304, 0x1D6), (0xFC, 0x30C, 0x1DA), (0x102, 0x300, 0x1EB0), (0x102, 0x301, 0x1EAE),
    (0x102, 0x303, 0x1EB4), (0x102, 0x309, 0x1EB2), (0x103, 0x300, 0x1EB1),
    (0x103, 0x301, 0x1EAF), (0x103, 0x303, 0x1EB5), (0x103, 0x309, 0x1EB3),
    (0x112, 0x300, 0x1E14), (0x112, 0x301, 0x1E16), (0x113, 0x300, 0x1E15),
    (0x113, 0x301, 0x1E17), (0x14C, 0x300, 0x1E50), (0x14C, 0x301, 0x1E52),
    (0x14D, 0x300, 0x1E51), (0x14D, 0x301, 0x1E53), (0x15A, 0x307, 0x1E64),
    (0x15B, 0x307, 0x1E65), (0x160, 0x307, 0x1E66), (0x161, 0x307, 0x1E67),
    (0x168, 0x301, 0x1E78), (0x169, 0x301, 0x1E79), (0x16A, 0x308, 0x1E7A),
    (0x16B, 0x308, 0x1E7B), (0x17F, 0x307, 0x1E9B), (0x1A0, 0x300, 0x1EDC),
    (0x1A0, 0x301, 0x1EDA), (0x1A0, 0x303, 0x1EE0), (0x1A0, 0x309, 0x1EDE),
    (0x1A0, 0x323, 0x1EE2), (0x1A1, 0x300, 0x1EDD), (0x1A1, 0x301, 0x1EDB),
    (0x1A1, 0x303, 0x1EE1), (0x1A1, 0x309, 0x1EDF), (0x1A1, 0x323, 0x1EE3),
    (0x1AF, 0x300, 0x1EEA), (0x1AF, 0x301, 0x1EE8), (0x1AF, 0x303, 0x1EEE),
    (0x1AF, 0x309, 0x1EEC), (0x1AF, 0x323, 0x1EF0), (0x1B0, 0x300, 0x1EEB),
    (0x1B0, 0x301, 0x1EE9), (0x1B0, 0x303, 0x1EEF), (0x1B0, 0x309, 0x1EED),
    (0x1B0, 0x323, 0x1EF1), (0x1B7, 0x30C, 0x1EE), (0x1EA, 0x304, 0x1EC),
    (0x1EB, 0x304, 0x1ED), (0x226, 0x304, 0x1E0), (0x227, 0x304, 0x1E1),
    (0x228, 0x306, 0x1E1C), (0x229, 0x306, 0x1E1D), (0x22E, 0x304, 0x230),
    (0x22F, 0x304, 0x231), (0x292, 0x30C, 0x1EF), (0x391, 0x300, 0x1FBA),
    (0x391, 0x301, 0x386), (0x391, 0x304, 0x1FB9), (0x391, 0x306, 0x1FB8),
    (0x391, 0x313, 0x1F08), (0x391, 0x314, 0x1F09), (0x391, 0x345, 0x1FBC),
    (0x395, 0x300, 0x1FC8), (0x395, 0x301, 0x388), (0x395, 0x313, 0x1F18),
    (0x395, 0x314, 0x1F19), (0x397, 0x300, 0x1FCA), (0x397, 0x301, 0x389),
    (0x397, 0x313, 0x1F28), (0x397, 0x314, 0x1F29), (0x397, 0x345, 0x1FCC),
    (0x399, 0x300, 0x1FDA), (0x399, 0x301, 0x38A), (0x399, 0x304, 0x1FD9),
    (0x399, 0x306, 0x1FD8), (0x399, 0x308, 0x3AA), (0x399, 0x313, 0x1F38),
    (0x399, 0x314, 0x1F39), (0x39F, 0x300, 0x1FF8), (0x39F, 0x301, 0x38C),
    (0x39F, 0x313, 0x1F48), (0x39F, 0x314, 0x1F49), (0x3A1, 0x314, 0x1FEC),
    (0x3A5, 0x300, 0x1FEA), (0x3A5, 0x301, 0x38E), (0x3A5, 0x304, 0x1FE9),
    (0x3A5, 0x306, 0x1FE8), (0x3A5, 0x308, 0x3AB), (0x3A5, 0x314, 0x1F59),
    (0x3A9, 0x300, 0x1FFA), (0x3A9, 0x301, 0x38F), (0x3A9, 0x313, 0x1F68),
    (0x3A9, 0x314, 0x1F69), (0x3A9, 0x345, 0x1FFC), (0x3AC, 0x345, 0x1FB4),
    (0x3AE, 0x345, 0x1FC4), (0x3B1, 0x300, 0x1F70), (0x3B1, 0x301, 0x3AC),
    (0x3B1, 0x304, 0x1FB1), (0x3B1, 0x306, 0x1FB0), (0x3B1, 0x313, 0x1F00),
    (0x3B1, 0x314, 0x1F01), (0x3B1, 0x342, 0x1FB6), (0x3B1, 0x345, 0x1FB3),
    (0x3B5, 0x300, 0x1F72), (0x3B5, 0x301, 0x3AD), (0x3B5, 0x313, 0x1F10),
    (0x3B5, 0x314, 0x1F11), (0x3B7, 0x300, 0x1F74), (0x3B7, 0x301, 0x3AE),
    (0x3B7, 0x313, 0x1F20), (0x3B7, 0x314, 0x1F21), (0x3B7, 0x342, 0x1FC6),
    (0x3B7, 0x345, 0x1FC3), (0x3B9, 0x300, 0x1F76), (0x3B9, 0x301, 0x3AF),
    (0x3B9, 0x304, 0x1FD1), (0x3B9, 0x306, 0x1FD0), (0x3B9, 0x308, 0x3CA),
    (0x3B9, 0x313, 0x1F30), (0x3B9, 0x314, 0x1F31), (0x3B9, 0x342, 0x1FD6),
    (0x3BF, 0x300, 0x1F78), (0x3BF, 0x301, 0x3CC), (0x3BF, 0x313, 0x1F40),
    (0x3BF, 0x314, 0x1F41), (0x3C1, 0x313, 0x1FE4), (0x3C1, 0x314, 0x1FE5),
    (0x3C5, 0x300, 0x1F7A), (0x3C5, 0x301, 0x3CD), (0x3C5, 0x304, 0x1FE1),
    (0x3C5, 0x306, 0x1FE0), (0x3C5, 0x308, 0x3CB), (0x3C5, 0x313, 0x1F50),
    (0x3C5, 0x314, 0x1F51), (0x3C5, 0x342, 0x1FE6), (0x3C9, 0x300, 0x1F7C),
    (0x3C9, 0x301, 0x3CE), (0x3C9, 0x313, 0x1F60), (0x3C9, 0x314, 0x1F61),
    (0x3C9, 0x342, 0x1FF6), (0x3C9, 0x345, 0x1FF3), (0x3CA, 0x300, 0x1FD2),
    (0x3CA, 0x301, 0x390), (0x3CA, 0x342, 0x1FD7), (0x3CB, 0x300, 0x1FE2),
    (0x3CB, 0x301, 0x3B0), (0x3CB, 0x342, 0x1FE7), (0x3CE, 0x345, 0x1FF4),
    (0x3D2, 0x301, 0x3D3), (0x3D2, 0x308, 0x3D4), (0x406, 0x308, 0x407), (0x410, 0x306, 0x4D0),
    (0x410, 0x308, 0x4D2), (0x413, 0x301, 0x403), (0x415, 0x300, 0x400), (0x415, 0x306, 0x4D6),
    (0x415, 0x308, 0x401), (0x416, 0x306, 0x4C1), (0x416, 0x308, 0x4DC), (0x417, 0x308, 0x4DE),
    (0x418, 0x300, 0x40D), (0x418, 0x304, 0x4E2), (0x418, 0x306, 0x419), (0x418, 0x308, 0x4E4),
    (0x41A, 0x301, 0x40C), (0x41E, 0x308, 0x4E6), (0x423, 0x304, 0x4EE), (0x423, 0x306, 0x40E),
    (0x423, 0x308, 0x4F0), (0x423, 0x30B, 0x4F2), (0x427, 0x308, 0x4F4), (0x42B, 0x308, 0x4F8),
    (0x42D, 0x308, 0x4EC), (0x430, 0x306, 0x4D1), (0x430, 0x308, 0x4D3), (0x433, 0x301, 0x453),
    (0x435, 0x300, 0x450), (0x435, 0x306, 0x4D7), (0x435, 0x308, 0x451), (0x436, 0x306, 0x4C2),
    (0x436, 0x308, 0x4DD), (0x437, 0x308, 0x4DF), (0x438, 0x300, 0x45D), (0x438, 0x304, 0x4E3),
    (0x438, 0x306, 0x439), (0x438, 0x308, 0x4E5), (0x43A, 0x301, 0x45C), (0x43E, 0x308, 0x4E7),
    (0x443, 0x304, 0x4EF), (0x443, 0x306, 0x45E), (0x443, 0x308, 0x4F1), (0x443, 0x30B, 0x4F3),
    (0x447, 0x308, 0x4F5), (0x44B, 0x308, 0x4F9), (0x44D, 0x308, 0x4ED), (0x456, 0x308, 0x457),
    (0x474, 0x30F, 0x476), (0x475, 0x30F, 0x477), (0x4D8, 0x308, 0x4DA), (0x4D9, 0x308, 0x4DB),
    (0x4E8, 0x308, 0x4EA), (0x4E9, 0x308, 0x4EB), (0x627, 0x653, 0x622), (0x627, 0x654, 0x623),
    (0x627, 0x655, 0x625), (0x648, 0x654, 0x624), (0x64A, 0x654, 0x626), (0x6C1, 0x654, 0x6C2),
    (0x6D2, 0x654, 0x6D3), (0x6D5, 0x654, 0x6C0), (0x928, 0x93C, 0x929), (0x930, 0x93C, 0x931),
    (0x933, 0x93C, 0x934), (0x9C7, 0x9BE, 0x9CB), (0x9C7, 0x9D7, 0x9CC), (0xB47, 0xB3E, 0xB4B),
    (0xB47, 0xB56, 0xB48), (0xB47, 0xB57, 0xB4C), (0xB92, 0xBD7, 0xB94), (0xBC6, 0xBBE, 0xBCA),
    (0xBC6, 0xBD7, 0xBCC), (0xBC7, 0xBBE, 0xBCB), (0xC46, 0xC56, 0xC48), (0xCBF, 0xCD5, 0xCC0),
    (0xCC6, 0xCC2, 0xCCA), (0xCC6, 0xCD5, 0xCC7), (0xCC6, 0xCD6, 0xCC8), (0xCCA, 0xCD5, 0xCCB),
    (0xD46, 0xD3E, 0xD4A), (0xD46, 0xD57, 0xD4C), (0xD47, 0xD3E, 0xD4B), (0xDD9, 0xDCA, 0xDDA),
    (0xDD9, 0xDCF, 0xDDC), (0xDD9, 0xDDF, 0xDDE), (0xDDC, 0xDCA, 0xDDD),
    (0x1025, 0x102E, 0x1026), (0x1B05, 0x1B35, 0x1B06), (0x1B07, 0x1B35, 0x1B08),
    (0x1B09, 0x1B35, 0x1B0A), (0x1B0B, 0x1B35, 0x1B0C), (0x1B0D, 0x1B35, 0x1B0E),
    (0x1B11, 0x1B35, 0x1B12), (0x1B3A, 0x1B35, 0x1B3B), (0x1B3C, 0x1B35, 0x1B3D),
    (0x1B3E, 0x1B35, 0x1B40), (0x1B3F, 0x1B35, 0x1B41), (0x1B42, 0x1B35, 0x1B43),
    (0x1E36, 0x304, 0x1E38), (0x1E37, 0x304, 0x1E39), (0x1E5A, 0x304, 0x1E5C),
    (0x1E5B, 0x304, 0x1E5D), (0x1E62, 0x307, 0x1E68), (0x1E63, 0x307, 0x1E69),
    (0x1EA0, 0x302, 0x1EAC), (0x1EA0, 0x306, 0x1EB6), (0x1EA1, 0x302, 0x1EAD),
    (0x1EA1, 0x306, 0x1EB7), (0x1EB8, 0x302, 0x1EC6), (0x1EB9, 0x302, 0x1EC7),
    (0x1ECC, 0x302, 0x1ED8), (0x1ECD, 0x302, 0x1ED9), (0x1F00, 0x300, 0x1F02),
    (0x1F00, 0x301, 0x1F04), (0x1F00, 0x342, 0x1F06), (0x1F00, 0x345, 0x1F80),
    (0x1F01, 0x300, 0x1F03), (0x1F01, 0x301, 0x1F05), (0x1F01, 0x342, 0x1F07),
    (0x1F01, 0x345, 0x1F81), (0x1F02, 0x345, 0x1F82), (0x1F03, 0x345, 0x1F83),
    (0x1F04, 0x345, 0x1F84), (0x1F05, 0x345, 0x1F85), (0x1F06, 0x345, 0x1F86),
    (0x1F07, 0x345, 0x1F87), (0x1F08, 0x300, 0x1F0A), (0x1F08, 0x301, 0x1F0C),
    (0x1F08, 0x342, 0x1F0E), (0x1F08, 0x345, 0x1F88), (0x1F09, 0x300, 0x1F0B),
    (0x1F09, 0x301, 0x1F0D), (0x1F09, 0x342, 0x1F0F), (0x1F09, 0x345, 0x1F89),
    (0x1F0A, 0x345, 0x1F8A), (0x1F0B, 0x345, 0x1F8B), (0x1F0C, 0x345, 0x1F8C),
    (0x1F0D, 0x345, 0x1F8D), (0x1F0E, 0x345, 0x1F8E), (0x1F0F, 0x345, 0x1F8F),
    (0x1F10, 0x300, 0x1F12), (0x1F10, 0x301, 0x1F14), (0x1F11, 0x300, 0x1F13),
    (0x1F11, 0x301, 0x1F15), (0x1F18, 0x300, 0x1F1A), (0x1F18, 0x301, 0x1F1C),
    (0x1F19, 0x300, 0x1F1B), (0x1F19, 0x301, 0x1F1D), (0x1F20, 0x300, 0x1F22),
    (0x1F20, 0x301, 0x1F24), (0x1F20, 0x342, 0x1F26), (0x1F20, 0x345, 0x1F90),
    (0x1F21, 0x300, 0x1F23), (0x1F21, 0x301, 0x1F25), (0x1F21, 0x342, 0x1F27),
    (0x1F21, 0x345, 0x1F91), (0x1F22, 0x345, 0x1F92), (0x1F23, 0x345, 0x1F93),
    (0x1F24, 0x345, 0x1F94), (0x1F25, 0x345, 0x1F95), (0x1F26, 0x345, 0x1F96),
    (0x1F27, 0x345, 0x1F97), (0x1F28, 0x300, 0x1F2A), (0x1F28, 0x301, 0x1F2C),
    (0x1F28, 0x342, 0x1F2E), (0x1F28, 0x345, 0x1F98), (0x1F29, 0x300, 0x1F2B),
    (0x1F29, 0x301, 0x1F2D), (0x1F29, 0x342, 0x1F2F), (0x1F29, 0x345, 0x1F99),
    (0x1F2A, 0x345, 0x1F9A), (0x1F2B, 0x345, 0x1F9B), (0x1F2C, 0x345, 0x1F9C),
    (0x1F2D, 0x345, 0x1F9D), (0x1F2E, 0x345, 0x1F9E), (0x1F2F, 0x345, 0x1F9F),
    (0x1F30, 0x300, 0x1F32), (0x1F30, 0x301, 0x1F34), (0x1F30, 0x342, 0x1F36),
    (0x1F31, 0x300, 0x1F33), (0x1F31, 0x301, 0x1F35), (0x1F31, 0x342, 0x1F37),
    (0x1F38, 0x300, 0x1F3A), (0x1F38, 0x301, 0x1F3C), (0x1F38, 0x342, 0x1F3E),
    (0x1F39, 0x300, 0x1F3B), (0x1F39, 0x301, 0x1F3D), (0x1F39, 0x342, 0x1F3F),
    (0x1F40, 0x300, 0x1F42), (0x1F40, 0x301, 0x1F44), (0x1F41, 0x300, 0x1F43),
    (0x1F41, 0x301, 0x1F45), (0x1F48, 0x300, 0x1F4A), (0x1F48, 0x301, 0x1F4C),
    (0x1F49, 0x300, 0x1F4B), (0x1F49, 0x301, 0x1F4D), (0x1F50, 0x300, 0x1F52),
    (0x1F50, 0x301, 0x1F54), (0x1F50, 0x342, 0x1F56), (0x1F51, 0x300, 0x1F53),
    (0x1F51, 0x301, 0x1F55), (0x1F51, 0x342, 0x1F57), (0x1F59, 0x300, 0x1F5B),
    (0x1F59, 0x301, 0x1F5D), (0x1F59, 0x342, 0x1F5F), (0x1F60, 0x300, 0x1F62),
    (0x1F60, 0x301, 0x1F64), (0x1F60, 0x342, 0x1F66), (0x1F60, 0x345, 0x1FA0),
    (0x1F61, 0x300, 0x1F63), (0x1F61, 0x301, 0x1F65), (0x1F61, 0x342, 0x1F67),
    (0x1F61, 0x345, 0x1FA1), (0x1F62, 0x345, 0x1FA2), (0x1F63, 0x345, 0x1FA3),
    (0x1F64, 0x345, 0x1FA4), (0x1F65, 0x345, 0x1FA5), (0x1F66, 0x345, 0x1FA6),
    (0x1F67, 0x345, 0x1FA7), (0x1F68, 0x300, 0x1F6A), (0x1F68, 0x301, 0x1F6C),
    (0x1F68, 0x342, 0x1F6E), (0x1F68, 0x345, 0x1FA8), (0x1F69, 0x300, 0x1F6B),
    (0x1F69, 0x301, 0x1F6D), (0x1F69, 0x342, 0x1F6F), (0x1F69, 0x345, 0x1FA9),
    (0x1F6A, 0x345, 0x1FAA), (0x1F6B, 0x345, 0x1FAB), (0x1F6C, 0x345, 0x1FAC),
    (0x1F6D, 0x345, 0x1FAD), (0x1F6E, 0x345, 0x1FAE), (0x1F6F, 0x345, 0x1FAF),
    (0x1F70, 0x345, 0x1FB2), (0x1F74, 0x345, 0x1FC2), (0x1F7C, 0x345, 0x1FF2),
    (0x1FB6, 0x345, 0x1FB7), (0x1FBF, 0x300, 0x1FCD), (0x1FBF, 0x301, 0x1FCE),
    (0x1FBF, 0x342, 0x1FCF), (0x1FC6, 0x345, 0x1FC7), (0x1FF6, 0x345, 0x1FF7),
    (0x1FFE, 0x300, 0x1FDD), (0x1FFE, 0x301, 0x1FDE), (0x1FFE, 0x342, 0x1FDF),
    (0x2190, 0x338, 0x219A), (0x2192, 0x338, 0x219B), (0x2194, 0x338, 0x21AE),
    (0x21D0, 0x338, 0x21CD), (0x21D2, 0x338, 0x21CF), (0x21D4, 0x338, 0x21CE),
    (0x2203, 0x338, 0x2204), (0x2208, 0x338, 0x2209), (0x220B, 0x338, 0x220C),
    (0x2223, 0x338, 0x2224), (0x2225, 0x338, 0x2226), (0x223C, 0x338, 0x2241),
    (0x2243, 0x338, 0x2244), (0x2245, 0x338, 0x2247), (0x2248, 0x338, 0x2249),
    (0x224D, 0x338, 0x226D), (0x2261, 0x338, 0x2262), (0x2264, 0x338, 0x2270),
    (0x2265, 0x338, 0x2271), (0x2272, 0x338, 0x2274), (0x2273, 0x338, 0x2275),
    (0x2276, 0x338, 0x2278), (0x2277, 0x338, 0x2279), (0x227A, 0x338, 0x2280),
    (0x227B, 0x338, 0x2281), (0x227C, 0x338, 0x22E0), (0x227D, 0x338, 0x22E1),
    (0x2282, 0x338, 0x2284), (0x2283, 0x338, 0x2285), (0x2286, 0x338, 0x2288),
    (0x2287, 0x338, 0x2289), (0x2291, 0x338, 0x22E2), (0x2292, 0x338, 0x22E3),
    (0x22A2, 0x338, 0x22AC), (0x22A8, 0x338, 0x22AD), (0x22A9, 0x338, 0x22AE),
    (0x22AB, 0x338, 0x22AF), (0x22B2, 0x338, 0x22EA), (0x22B3, 0x338, 0x22EB),
    (0x22B4, 0x338, 0x22EC), (0x22B5, 0x338, 0x22ED), (0x3046, 0x3099, 0x3094),
    (0x304B, 0x3099, 0x304C), (0x304D, 0x3099, 0x304E), (0x304F, 0x3099, 0x3050),
    (0x3051, 0x3099, 0x3052), (0x3053, 0x3099, 0x3054), (0x3055, 0x3099, 0x3056),
    (0x3057, 0x3099, 0x3058), (0x3059, 0x3099, 0x305A), (0x305B, 0x3099, 0x305C),
    (0x305D, 0x3099, 0x305E), (0x305F, 0x3099, 0x3060), (0x3061, 0x3099, 0x3062),
    (0x3064, 0x3099, 0x3065), (0x3066, 0x3099, 0x3067), (0x3068, 0x3099, 0x3069),
    (0x306F, 0x3099, 0x3070), (0x306F, 0x309A, 0x3071), (0x3072, 0x3099, 0x3073),
    (0x3072, 0x309A, 0x3074), (0x3075, 0x3099, 0x3076), (0x3075, 0x309A, 0x3077),
    (0x3078, 0x3099, 0x3079), (0x3078, 0x309A, 0x307A), (0x307B, 0x3099, 0x307C),
    (0x307B, 0x309A, 0x307D), (0x309D, 0x3099, 0x309E), (0x30A6, 0x3099, 0x30F4),
    (0x30AB, 0x3099, 0x30AC), (0x30AD, 0x3099, 0x30AE), (0x30AF, 0x3099, 0x30B0),
    (0x30B1, 0x3099, 0x30B2), (0x30B3, 0x3099, 0x30B4), (0x30B5, 0x3099, 0x30B6),
    (0x30B7, 0x3099, 0x30B8), (0x30B9, 0x3099, 0x30BA), (0x30BB, 0x3099, 0x30BC),
    (0x30BD, 0x3099, 0x30BE), (0x30BF, 0x3099, 0x30C0), (0x30C1, 0x3099, 0x30C2),
    (0x30C4, 0x3099, 0x30C5), (0x30C6, 0x3099, 0x30C7), (0x30C8, 0x3099, 0x30C9),
    (0x30CF, 0x3099, 0x30D0), (0x30CF, 0x309A, 0x30D1), (0x30D2, 0x3099, 0x30D3),
    (0x30D2, 0x309A, 0x30D4), (0x30D5, 0x3099, 0x30D6), (0x30D5, 0x309A, 0x30D7),
    (0x30D8, 0x3099, 0x30D9), (0x30D8, 0x309A, 0x30DA), (0x30DB, 0x3099, 0x30DC),
    (0x30DB, 0x309A, 0x30DD), (0x30EF, 0x3099, 0x30F7), (0x30F0, 0x3099, 0x30F8),
    (0x30F1, 0x3099, 0x30F9), (0x30F2, 0x3099, 0x30FA), (0x30FD, 0x3099, 0x30FE),
    (0x11099, 0x110BA, 0x1109A), (0x1109B, 0x110BA, 0x1109C), (0x110A5, 0x110BA, 0x110AB),
    (0x11131, 0x11127, 0x1112E), (0x11132, 0x11127, 0x1112F), (0x11347, 0x1133E, 0x1134B),
    (0x11347, 0x11357, 0x1134C), (0x114B9, 0x114B0, 0x114BC), (0x114B9, 0x114BA, 0x114BB),
    (0x114B9, 0x114BD, 0x114BE), (0x115B8, 0x115AF, 0x115BA), (0x115B9, 0x115AF, 0x115BB),
    (0x11935, 0x11930, 0x11938),
];

/// Full case folding per code point (identity mappings omitted).
#[rustfmt::skip]
static CASEFOLD_TABLE: &[(u32, &str)] = &[
    (0x41, "a"), (0x42, "b"), (0x43, "c"), (0x44, "d"), (0x45, "e"), (0x46, "f"), (0x47, "g"),
    (0x48, "h"), (0x49, "i"), (0x4A, "j"), (0x4B, "k"), (0x4C, "l"), (0x4D, "m"), (0x4E, "n"),
    (0x4F, "o"), (0x50, "p"), (0x51, "q"), (0x52, "r"), (0x53, "s"), (0x54, "t"), (0x55, "u"),
    (0x56, "v"), (0x57, "w"), (0x58, "x"), (0x59, "y"), (0x5A, "z"), (0xB5, "\u{3bc}"),
    (0xC0, "\u{e0}"), (0xC1, "\u{e1}"), (0xC2, "\u{e2}"), (0xC3, "\u{e3}"), (0xC4, "\u{e4}"),
    (0xC5, "\u{e5}"), (0xC6, "\u{e6}"), (0xC7, "\u{e7}"), (0xC8, "\u{e8}"), (0xC9, "\u{e9}"),
    (0xCA, "\u{ea}"), (0xCB, "\u{eb}"), (0xCC, "\u{ec}"), (0xCD, "\u{ed}"), (0xCE, "\u{ee}"),
    (0xCF, "\u{ef}"), (0xD0, "\u{f0}"), (0xD1, "\u{f1}"), (0xD2, "\u{f2}"), (0xD3, "\u{f3}"),
    (0xD4, "\u{f4}"), (0xD5, "\u{f5}"), (0xD6, "\u{f6}"), (0xD8, "\u{f8}"), (0xD9, "\u{f9}"),
    (0xDA, "\u{fa}"), (0xDB, "\u{fb}"), (0xDC, "\u{fc}"), (0xDD, "\u{fd}"), (0xDE, "\u{fe}"),
    (0xDF, "ss"), (0x100, "\u{101}"), (0x102, "\u{103}"), (0x104, "\u{105}"),
    (0x106, "\u{107}"), (0x108, "\u{109}"), (0x10A, "\u{10b}"), (0x10C, "\u{10d}"),
    (0x10E, "\u{10f}"), (0x110, "\u{111}"), (0x112, "\u{113}"), (0x114, "\u{115}"),
    (0x116, "\u{117}"), (0x118, "\u{119}"), (0x11A, "\u{11b}"), (0x11C, "\u{11d}"),
    (0x11E, "\u{11f}"), (0x120, "\u{121}"), (0x122, "\u{123}"), (0x124, "\u{125}"),
    (0x126, "\u{127}"), (0x128, "\u{129}"), (0x12A, "\u{12b}"), (0x12C, "\u{12d}"),
    (0x12E, "\u{12f}"), (0x130, "i\u{307}"), (0x132, "\u{133}"), (0x134, "\u{135}"),
    (0x136, "\u{137}"), (0x139, "\u{13a}"), (0x13B, "\u{13c}"), (0x13D, "\u{13e}"),
    (0x13F, "\u{140}"), (0x141, "\u{142}"), (0x143, "\u{144}"), (0x145, "\u{146}"),
    (0x147, "\u{148}"), (0x149, "\u{2bc}n"), (0x14A, "\u{14b}"), (0x14C, "\u{14d}"),
    (0x14E, "\u{14f}"), (0x150, "\u{151}"), (0x152, "\u{153}"), (0x154, "\u{155}"),
    (0x156, "\u{157}"), (0x158, "\u{159}"), (0x15A, "\u{15b}"), (0x15C, "\u{15d}"),
    (0x15E, "\u{15f}"), (0x160, "\u{161}"), (0x162, "\u{163}"), (0x164, "\u{165}"),
    (0x166, "\u{167}"), (0x168, "\u{169}"), (0x16A, "\u{16b}"), (0x16C, "\u{16d}"),
    (0x16E, "\u{16f}"), (0x170, "\u{171}"), (0x172, "\u{173}"), (0x174, "\u{175}"),
    (0x176, "\u{177}"), (0x178, "\u{ff}"), (0x179, "\u{17a}"), (0x17B, "\u{17c}"),
    (0x17D, "\u{17e}"), (0x17F, "s"), (0x181, "\u{253}"), (0x182, "\u{183}"),
    (0x184, "\u{185}"), (0x186, "\u{254}"), (0x187, "\u{188}"), (0x189, "\u{256}"),
    (0x18A, "\u{257}"), (0x18B, "\u{18c}"), (0x18E, "\u{1dd}"), (0x18F, "\u{259}"),
    (0x190, "\u{25b}"), (0x191, "\u{192}"), (0x193, "\u{260}"), (0x194, "\u{263}"),
    (0x196, "\u{269}"), (0x197, "\u{268}"), (0x198, "\u{199}"), (0x19C, "\u{26f}"),
    (0x19D, "\u{272}"), (0x19F, "\u{275}"), (0x1A0, "\u{1a1}"), (0x1A2, "\u{1a3}"),
    (0x1A4, "\u{1a5}"), (0x1A6, "\u{280}"), (0x1A7, "\u{1a8}"), (0x1A9, "\u{283}"),
    (0x1AC, "\u{1ad}"), (0x1AE, "\u{288}"), (0x1AF, "\u{1b0}"), (0x1B1, "\u{28a}"),
    (0x1B2, "\u{28b}"), (0x1B3, "\u{1b4}"), (0x1B5, "\u{1b6}"), (0x1B7, "\u{292}"),
    (0x1B8, "\u{1b9}"), (0x1BC, "\u{1bd}"), (0x1C4, "\u{1c6}"), (0x1C5, "\u{1c6}"),
    (0x1C7, "\u{1c9}"), (0x1C8, "\u{1c9}"), (0x1CA, "\u{1cc}"), (0x1CB, "\u{1cc}"),
    (0x1CD, "\u{1ce}"), (0x1CF, "\u{1d0}"), (0x1D1, "\u{1d2}"), (0x1D3, "\u{1d4}"),
    (0x1D5, "\u{1d6}"), (0x1D7, "\u{1d8}"), (0x1D9, "\u{1da}"), (0x1DB, "\u{1dc}"),
    (0x1DE, "\u{1df}"), (0x1E0, "\u{1e1}"), (0x1E2, "\u{1e3}"), (0x1E4, "\u{1e5}"),
    (0x1E6, "\u{1e7}"), (0x1E8, "\u{1e9}"), (0x1EA, "\u{1eb}"), (0x1EC, "\u{1ed}"),
    (0x1EE, "\u{1ef}"), (0x1F0, "j\u{30c}"), (0x1F1, "\u{1f3}"), (0x1F2, "\u{1f3}"),
    (0x1F4, "\u{1f5}"), (0x1F6, "\u{195}"), (0x1F7, "\u{1bf}"), (0x1F8, "\u{1f9}"),
    (0x1FA, "\u{1fb}"), (0x1FC, "\u{1fd}"), (0x1FE, "\u{1ff}"), (0x200, "\u{201}"),
    (0x202, "\u{203}"), (0x204, "\u{205}"), (0x206, "\u{207}"), (0x208, "\u{209}"),
    (0x20A, "\u{20b}"), (0x20C, "\u{20d}"), (0x20E, "\u{20f}"), (0x210, "\u{211}"),
    (0x212, "\u{213}"), (0x214, "\u{215}"), (0x216, "\u{217}"), (0x218, "\u{219}"),
    (0x21A, "\u{21b}"), (0x21C, "\u{21d}"), (0x21E, "\u{21f}"), (0x220, "\u{19e}"),
    (0x222, "\u{223}"), (0x224, "\u{225}"), (0x226, "\u{227}"), (0x228, "\u{229}"),
    (0x22A, "\u{22b}"), (0x22C, "\u{22d}"), (0x22E, "\u{22f}"), (0x230, "\u{231}"),
    (0x232, "\u{233}"), (0x23A, "\u{2c65}"), (0x23B, "\u{23c}"), (0x23D, "\u{19a}"),
    (0x23E, "\u{2c66}"), (0x241, "\u{242}"), (0x243, "\u{180}"), (0x244, "\u{289}"),
    (0x245, "\u{28c}"), (0x246, "\u{247}"), (0x248, "\u{249}"), (0x24A, "\u{24b}"),
    (0x24C, "\u{24d}"), (0x24E, "\u{24f}"), (0x345, "\u{3b9}"), (0x370, "\u{371}"),
    (0x372, "\u{373}"), (0x376, "\u{377}"), (0x37F, "\u{3f3}"), (0x386, "\u{3ac}"),
    (0x388, "\u{3ad}"), (0x389, "\u{3ae}"), (0x38A, "\u{3af}"), (0x38C, "\u{3cc}"),
    (0x38E, "\u{3cd}"), (0x38F, "\u{3ce}"), (0x390, "\u{3b9}\u{308}\u{301}"),
    (0x391, "\u{3b1}"), (0x392, "\u{3b2}"), (0x393, "\u{3b3}"), (0x394, "\u{3b4}"),
    (0x395, "\u{3b5}"), (0x396, "\u{3b6}"), (0x397, "\u{3b7}"), (0x398, "\u{3b8}"),
    (0x399, "\u{3b9}"), (0x39A, "\u{3ba}"), (0x39B, "\u{3bb}"), (0x39C, "\u{3bc}"),
    (0x39D, "\u{3bd}"), (0x39E, "\u{3be}"), (0x39F, "\u{3bf}"), (0x3A0, "\u{3c0}"),
    (0x3A1, "\u{3c1}"), (0x3A3, "\u{3c3}"), (0x3A4, "\u{3c4}"), (0x3A5, "\u{3c5}"),
    (0x3A6, "\u{3c6}"), (0x3A7, "\u{3c7}"), (0x3A8, "\u{3c8}"), (0x3A9, "\u{3c9}"),
    (0x3AA, "\u{3ca}"), (0x3AB, "\u{3cb}"), (0x3B0, "\u{3c5}\u{308}\u{301}"),
    (0x3C2, "\u{3c3}"), (0x3CF, "\u{3d7}"), (0x3D0, "\u{3b2}"), (0x3D1, "\u{3b8}"),
    (0x3D5, "\u{3c6}"), (0x3D6, "\u{3c0}"), (0x3D8, "\u{3d9}"), (0x3DA, "\u{3db}"),
    (0x3DC, "\u{3dd}"), (0x3DE, "\u{3df}"), (0x3E0, "\u{3e1}"), (0x3E2, "\u{3e3}"),
    (0x3E4, "\u{3e5}"), (0x3E6, "\u{3e7}"), (0x3E8, "\u{3e9}"), (0x3EA, "\u{3eb}"),
    (0x3EC, "\u{3ed}"), (0x3EE, "\u{3ef}"), (0x3F0, "\u{3ba}"), (0x3F1, "\u{3c1}"),
    (0x3F4, "\u{3b8}"), (0x3F5, "\u{3b5}"), (0x3F7, "\u{3f8}"), (0x3F9, "\u{3f2}"),
    (0x3FA, "\u{3fb}"), (0x3FD, "\u{37b}"), (0x3FE, "\u{37c}"), (0x3FF, "\u{37d}"),
    (0x400, "\u{450}"), (0x401, "\u{451}"), (0x402, "\u{452}"), (0x403, "\u{453}"),
    (0x404, "\u{454}"), (0x405, "\u{455}"), (0x406, "\u{456}"), (0x407, "\u{457}"),
    (0x408, "\u{458}"), (0x409, "\u{459}"), (0x40A, "\u{45a}"), (0x40B, "\u{45b}"),
    (0x40C, "\u{45c}"), (0x40D, "\u{45d}"), (0x40E, "\u{45e}"), (0x40F, "\u{45f}"),
    (0x410, "\u{430}"), (0x411, "\u{431}"), (0x412, "\u{432}"), (0x413, "\u{433}"),
    (0x414, "\u{434}"), (0x415, "\u{435}"), (0x416, "\u{436}"), (0x417, "\u{437}"),
    (0x418, "\u{438}"), (0x419, "\u{439}"), (0x41A, "\u{43a}"), (0x41B, "\u{43b}"),
    (0x41C, "\u{43c}"), (0x41D, "\u{43d}"), (0x41E, "\u{43e}"), (0x41F, "\u{43f}"),
    (0x420, "\u{440}"), (0x421, "\u{441}"), (0x422, "\u{442}"), (0x423, "\u{443}"),
    (0x424, "\u{444}"), (0x425, "\u{445}"), (0x426, "\u{446}"), (0x427, "\u{447}"),
    (0x428, "\u{448}"), (0x429, "\u{449}"), (0x42A, "\u{44a}"), (0x42B, "\u{44b}"),
    (0x42C, "\u{44c}"), (0x42D, "\u{44d}"), (0x42E, "\u{44e}"), (0x42F, "\u{44f}"),
    (0x460, "\u{461}"), (0x462, "\u{463}"), (0x464, "\u{465}"), (0x466, "\u{467}"),
    (0x468, "\u{469}"), (0x46A, "\u{46b}"), (0x46C, "\u{46d}"), (0x46E, "\u{46f}"),
    (0x470, "\u{471}"), (0x472, "\u{473}"), (0x474, "\u{475}"), (0x476, "\u{477}"),
    (0x478, "\u{479}"), (0x47A, "\u{47b}"), (0x47C, "\u{47d}"), (0x47E, "\u{47f}"),
    (0x480, "\u{481}"), (0x48A, "\u{48b}"), (0x48C, "\u{48d}"), (0x48E, "\u{48f}"),
    (0x490, "\u{491}"), (0x492, "\u{493}"), (0x494, "\u{495}"), (0x496, "\u{497}"),
    (0x498, "\u{499}"), (0x49A, "\u{49b}"), (0x49C, "\u{49d}"), (0x49E, "\u{49f}"),
    (0x4A0, "\u{4a1}"), (0x4A2, "\u{4a3}"), (0x4A4, "\u{4a5}"), (0x4A6, "\u{4a7}"),
    (0x4A8, "\u{4a9}"), (0x4AA, "\u{4ab}"), (0x4AC, "\u{4ad}"), (0x4AE, "\u{4af}"),
    (0x4B0, "\u{4b1}"), (0x4B2, "\u{4b3}"), (0x4B4, "\u{4b5}"), (0x4B6, "\u{4b7}"),
    (0x4B8, "\u{4b9}"), (0x4BA, "\u{4bb}"), (0x4BC, "\u{4bd}"), (0x4BE, "\u{4bf}"),
    (0x4C0, "\u{4cf}"), (0x4C1, "\u{4c2}"), (0x4C3, "\u{4c4}"), (0x4C5, "\u{4c6}"),
    (0x4C7, "\u{4c8}"), (0x4C9, "\u{4ca}"), (0x4CB, "\u{4cc}"), (0x4CD, "\u{4ce}"),
    (0x4D0, "\u{4d1}"), (0x4D2, "\u{4d3}"), (0x4D4, "\u{4d5}"), (0x4D6, "\u{4d7}"),
    (0x4D8, "\u{4d9}"), (0x4DA, "\u{4db}"), (0x4DC, "\u{4dd}"), (0x4DE, "\u{4df}"),
    (0x4E0, "\u{4e1}"), (0x4E2, "\u{4e3}"), (0x4E4, "\u{4e5}"), (0x4E6, "\u{4e7}"),
    (0x4E8, "\u{4e9}"), (0x4EA, "\u{4eb}"), (0x4EC, "\u{4ed}"), (0x4EE, "\u{4ef}"),
    (0x4F0, "\u{4f1}"), (0x4F2, "\u{4f3}"), (0x4F4, "\u{4f5}"), (0x4F6, "\u{4f7}"),
    (0x4F8, "\u{4f9}"), (0x4FA, "\u{4fb}"), (0x4FC, "\u{4fd}"), (0x4FE, "\u{4ff}"),
    (0x500, "\u{501}"), (0x502, "\u{503}"), (0x504, "\u{505}"), (0x506, "\u{507}"),
    (0x508, "\u{509}"), (0x50A, "\u{50b}"), (0x50C, "\u{50d}"), (0x50E, "\u{50f}"),
    (0x510, "\u{511}"), (0x512, "\u{513}"), (0x514, "\u{515}"), (0x516, "\u{517}"),
    (0x518, "\u{519}"), (0x51A, "\u{51b}"), (0x51C, "\u{51d}"), (0x51E, "\u{51f}"),
    (0x520, "\u{521}"), (0x522, "\u{523}"), (0x524, "\u{525}"), (0x526, "\u{527}"),
    (0x528, "\u{529}"), (0x52A, "\u{52b}"), (0x52C, "\u{52d}"), (0x52E, "\u{52f}"),
    (0x531, "\u{561}"), (0x532, "\u{562}"), (0x533, "\u{563}"), (0x534, "\u{564}"),
    (0x535, "\u{565}"), (0x536, "\u{566}"), (0x537, "\u{567}"), (0x538, "\u{568}"),
    (0x539, "\u{569}"), (0x53A, "\u{56a}"), (0x53B, "\u{56b}"), (0x53C, "\u{56c}"),
    (0x53D, "\u{56d}"), (0x53E, "\u{56e}"), (0x53F, "\u{56f}"), (0x540, "\u{570}"),
    (0x541, "\u{571}"), (0x542, "\u{572}"), (0x543, "\u{573}"), (0x544, "\u{574}"),
    (0x545, "\u{575}"), (0x546, "\u{576}"), (0x547, "\u{577}"), (0x548, "\u{578}"),
    (0x549, "\u{579}"), (0x54A, "\u{57a}"), (0x54B, "\u{57b}"), (0x54C, "\u{57c}"),
    (0x54D, "\u{57d}"), (0x54E, "\u{57e}"), (0x54F, "\u{57f}"), (0x550, "\u{580}"),
    (0x551, "\u{581}"), (0x552, "\u{582}"), (0x553, "\u{583}"), (0x554, "\u{584}"),
    (0x555, "\u{585}"), (0x556, "\u{586}"), (0x587, "\u{565}\u{582}"), (0x10A0, "\u{2d00}"),
    (0x10A1, "\u{2d01}"), (0x10A2, "\u{2d02}"), (0x10A3, "\u{2d03}"), (0x10A4, "\u{2d04}"),
    (0x10A5, "\u{2d05}"), (0x10A6, "\u{2d06}"), (0x10A7, "\u{2d07}"), (0x10A8, "\u{2d08}"),
    (0x10A9, "\u{2d09}"), (0x10AA, "\u{2d0a}"), (0x10AB, "\u{2d0b}"), (0x10AC, "\u{2d0c}"),
    (0x10AD, "\u{2d0d}"), (0x10AE, "\u{2d0e}"), (0x10AF, "\u{2d0f}"), (0x10B0, "\u{2d10}"),
    (0x10B1, "\u{2d11}"), (0x10B2, "\u{2d12}"), (0x10B3, "\u{2d13}"), (0x10B4, "\u{2d14}"),
    (0x10B5, "\u{2d15}"), (0x10B6, "\u{2d16}"), (0x10B7, "\u{2d17}"), (0x10B8, "\u{2d18}"),
    (0x10B9, "\u{2d19}"), (0x10BA, "\u{2d1a}"), (0x10BB, "\u{2d1b}"), (0x10BC, "\u{2d1c}"),
    (0x10BD, "\u{2d1d}"), (0x10BE, "\u{2d1e}"), (0x10BF, "\u{2d1f}"), (0x10C0, "\u{2d20}"),
    (0x10C1, "\u{2d21}"), (0x10C2, "\u{2d22}"), (0x10C3, "\u{2d23}"), (0x10C4, "\u{2d24}"),
    (0x10C5, "\u{2d25}"), (0x10C7, "\u{2d27}"), (0x10CD, "\u{2d2d}"), (0x13F8, "\u{13f0}"),
    (0x13F9, "\u{13f1}"), (0x13FA, "\u{13f2}"), (0x13FB, "\u{13f3}"), (0x13FC, "\u{13f4}"),
    (0x13FD, "\u{13f5}"), (0x1C80, "\u{432}"), (0x1C81, "\u{434}"), (0x1C82, "\u{43e}"),
    (0x1C83, "\u{441}"), (0x1C84, "\u{442}"), (0x1C85, "\u{442}"), (0x1C86, "\u{44a}"),
    (0x1C87, "\u{463}"), (0x1C88, "\u{a64b}"), (0x1C90, "\u{10d0}"), (0x1C91, "\u{10d1}"),
    (0x1C92, "\u{10d2}"), (0x1C93, "\u{10d3}"), (0x1C94, "\u{10d4}"), (0x1C95, "\u{10d5}"),
    (0x1C96, "\u{10d6}"), (0x1C97, "\u{10d7}"), (0x1C98, "\u{10d8}"), (0x1C99, "\u{10d9}"),
    (0x1C9A, "\u{10da}"), (0x1C9B, "\u{10db}"), (0x1C9C, "\u{10dc}"), (0x1C9D, "\u{10dd}"),
    (0x1C9E, "\u{10de}"), (0x1C9F, "\u{10df}"), (0x1CA0, "\u{10e0}"), (0x1CA1, "\u{10e1}"),
    (0x1CA2, "\u{10e2}"), (0x1CA3, "\u{10e3}"), (0x1CA4, "\u{10e4}"), (0x1CA5, "\u{10e5}"),
    (0x1CA6, "\u{10e6}"), (0x1CA7, "\u{10e7}"), (0x1CA8, "\u{10e8}"), (0x1CA9, "\u{10e9}"),
    (0x1CAA, "\u{10ea}"), (0x1CAB, "\u{10eb}"), (0x1CAC, "\u{10ec}"), (0x1CAD, "\u{10ed}"),
    (0x1CAE, "\u{10ee}"), (0x1CAF, "\u{10ef}"), (0x1CB0, "\u{10f0}"), (0x1CB1, "\u{10f1}"),
    (0x1CB2, "\u{10f2}"), (0x1CB3, "\u{10f3}"), (0x1CB4, "\u{10f4}"), (0x1CB5, "\u{10f5}"),
    (0x1CB6, "\u{10f6}"), (0x1CB7, "\u{10f7}"), (0x1CB8, "\u{10f8}"), (0x1CB9, "\u{10f9}"),
    (0x1CBA, "\u{10fa}"), (0x1CBD, "\u{10fd}"), (0x1CBE, "\u{10fe}"), (0x1CBF, "\u{10ff}"),
    (0x1E00, "\u{1e01}"), (0x1E02, "\u{1e03}"), (0x1E04, "\u{1e05}"), (0x1E06, "\u{1e07}"),
    (0x1E08, "\u{1e09}"), (0x1E0A, "\u{1e0b}"), (0x1E0C, "\u{1e0d}"), (0x1E0E, "\u{1e0f}"),
    (0x1E10, "\u{1e11}"), (0x1E12, "\u{1e13}"), (0x1E14, "\u{1e15}"), (0x1E16, "\u{1e17}"),
    (0x1E18, "\u{1e19}"), (0x1E1A, "\u{1e1b}"), (0x1E1C, "\u{1e1d}"), (0x1E1E, "\u{1e1f}"),
    (0x1E20, "\u{1e21}"), (0x1E22, "\u{1e23}"), (0x1E24, "\u{1e25}"), (0x1E26, "\u{1e27}"),
    (0x1E28, "\u{1e29}"), (0x1E2A, "\u{1e2b}"), (0x1E2C, "\u{1e2d}"), (0x1E2E, "\u{1e2f}"),
    (0x1E30, "\u{1e31}"), (0x1E32, "\u{1e33}"), (0x1E34, "\u{1e35}"), (0x1E36, "\u{1e37}"),
    (0x1E38, "\u{1e39}"), (0x1E3A, "\u{1e3b}"), (0x1E3C, "\u{1e3d}"), (0x1E3E, "\u{1e3f}"),
    (0x1E40, "\u{1e41}"), (0x1E42, "\u{1e43}"), (0x1E44, "\u{1e45}"), (0x1E46, "\u{1e47}"),
    (0x1E48, "\u{1e49}"), (0x1E4A, "\u{1e4b}"), (0x1E4C, "\u{1e4d}"), (0x1E4E, "\u{1e4f}"),
    (0x1E50, "\u{1e51}"), (0x1E52, "\u{1e53}"), (0x1E54, "\u{1e55}"), (0x1E56, "\u{1e57}"),
    (0x1E58, "\u{1e59}"), (0x1E5A, "\u{1e5b}"), (0x1E5C, "\u{1e5d}"), (0x1E5E, "\u{1e5f}"),
    (0x1E60, "\u{1e61}"), (0x1E62, "\u{1e63}"), (0x1E64, "\u{1e65}"), (0x1E66, "\u{1e67}"),
    (0x1E68, "\u{1e69}"), (0x1E6A, "\u{1e6b}"), (0x1E6C, "\u{1e6d}"), (0x1E6E, "\u{1e6f}"),
    (0x1E70, "\u{1e71}"), (0x1E72, "\u{1e73}"), (0x1E74, "\u{1e75}"), (0x1E76, "\u{1e77}"),
    (0x1E78, "\u{1e79}"), (0x1E7A, "\u{1e7b}"), (0x1E7C, "\u{1e7d}"), (0x1E7E, "\u{1e7f}"),
    (0x1E80, "\u{1e81}"), (0x1E82, "\u{1e83}"), (0x1E84, "\u{1e85}"), (0x1E86, "\u{1e87}"),
    (0x1E88, "\u{1e89}"), (0x1E8A, "\u{1e8b}"), (0x1E8C, "\u{1e8d}"), (0x1E8E, "\u{1e8f}"),
    (0x1E90, "\u{1e91}"), (0x1E92, "\u{1e93}"), (0x1E94, "\u{1e95}"), (0x1E96, "h\u{331}"),
    (0x1E97, "t\u{308}"), (0x1E98, "w\u{30a}"), (0x1E99, "y\u{30a}"), (0x1E9A, "a\u{2be}"),
    (0x1E9B, "\u{1e61}"), (0x1E9E, "ss"), (0x1EA0, "\u{1ea1}"), (0x1EA2, "\u{1ea3}"),
    (0x1EA4, "\u{1ea5}"), (0x1EA6, "\u{1ea7}"), (0x1EA8, "\u{1ea9}"), (0x1EAA, "\u{1eab}"),
    (0x1EAC, "\u{1ead}"), (0x1EAE, "\u{1eaf}"), (0x1EB0, "\u{1eb1}"), (0x1EB2, "\u{1eb3}"),
    (0x1EB4, "\u{1eb5}"), (0x1EB6, "\u{1eb7}"), (0x1EB8, "\u{1eb9}"), (0x1EBA, "\u{1ebb}"),
    (0x1EBC, "\u{1ebd}"), (0x1EBE, "\u{1ebf}"), (0x1EC0, "\u{1ec1}"), (0x1EC2, "\u{1ec3}"),
    (0x1EC4, "\u{1ec5}"), (0x1EC6, "\u{1ec7}"), (0x1EC8, "\u{1ec9}"), (0x1ECA, "\u{1ecb}"),
    (0x1ECC, "\u{1ecd}"), (0x1ECE, "\u{1ecf}"), (0x1ED0, "\u{1ed1}"), (0x1ED2, "\u{1ed3}"),
    (0x1ED4, "\u{1ed5}"), (0x1ED6, "\u{1ed7}"), (0x1ED8, "\u{1ed9}"), (0x1EDA, "\u{1edb}"),
    (0x1EDC, "\u{1edd}"), (0x1EDE, "\u{1edf}"), (0x1EE0, "\u{1ee1}"), (0x1EE2, "\u{1ee3}"),
    (0x1EE4, "\u{1ee5}"), (0x1EE6, "\u{1ee7}"), (0x1EE8, "\u{1ee9}"), (0x1EEA, "\u{1eeb}"),
    (0x1EEC, "\u{1eed}"), (0x1EEE, "\u{1eef}"), (0x1EF0, "\u{1ef1}"), (0x1EF2, "\u{1ef3}"),
    (0x1EF4, "\u{1ef5}"), (0x1EF6, "\u{1ef7}"), (0x1EF8, "\u{1ef9}"), (0x1EFA, "\u{1efb}"),
    (0x1EFC, "\u{1efd}"), (0x1EFE, "\u{1eff}"), (0x1F08, "\u{1f00}"), (0x1F09, "\u{1f01}"),
    (0x1F0A, "\u{1f02}"), (0x1F0B, "\u{1f03}"), (0x1F0C, "\u{1f04}"), (0x1F0D, "\u{1f05}"),
    (0x1F0E, "\u{1f06}"), (0x1F0F, "\u{1f07}"), (0x1F18, "\u{1f10}"), (0x1F19, "\u{1f11}"),
    (0x1F1A, "\u{1f12}"), (0x1F1B, "\u{1f13}"), (0x1F1C, "\u{1f14}"), (0x1F1D, "\u{1f15}"),
    (0x1F28, "\u{1f20}"), (0x1F29, "\u{1f21}"), (0x1F2A, "\u{1f22}"), (0x1F2B, "\u{1f23}"),
    (0x1F2C, "\u{1f24}"), (0x1F2D, "\u{1f25}"), (0x1F2E, "\u{1f26}"), (0x1F2F, "\u{1f27}"),
    (0x1F38, "\u{1f30}"), (0x1F39, "\u{1f31}"), (0x1F3A, "\u{1f32}"), (0x1F3B, "\u{1f33}"),
    (0x1F3C, "\u{1f34}"), (0x1F3D, "\u{1f35}"), (0x1F3E, "\u{1f36}"), (0x1F3F, "\u{1f37}"),
    (0x1F48, "\u{1f40}"), (0x1F49, "\u{1f41}"), (0x1F4A, "\u{1f42}"), (0x1F4B, "\u{1f43}"),
    (0x1F4C, "\u{1f44}"), (0x1F4D, "\u{1f45}"), (0x1F50, "\u{3c5}\u{313}"),
    (0x1F52, "\u{3c5}\u{313}\u{300}"), (0x1F54, "\u{3c5}\u{313}\u{301}"),
    (0x1F56, "\u{3c5}\u{313}\u{342}"), (0x1F59, "\u{1f51}"), (0x1F5B, "\u{1f53}"),
    (0x1F5D, "\u{1f55}"), (0x1F5F, "\u{1f57}"), (0x1F68, "\u{1f60}"), (0x1F69, "\u{1f61}"),
    (0x1F6A, "\u{1f62}"), (0x1F6B, "\u{1f63}"), (0x1F6C, "\u{1f64}"), (0x1F6D, "\u{1f65}"),
    (0x1F6E, "\u{1f66}"), (0x1F6F, "\u{1f67}"), (0x1F80, "\u{1f00}\u{3b9}"),
    (0x1F81, "\u{1f01}\u{3b9}"), (0x1F82, "\u{1f02}\u{3b9}"), (0x1F83, "\u{1f03}\u{3b9}"),
    (0x1F84, "\u{1f04}\u{3b9}"), (0x1F85, "\u{1f05}\u{3b9}"), (0x1F86, "\u{1f06}\u{3b9}"),
    (0x1F87, "\u{1f07}\u{3b9}"), (0x1F88, "\u{1f00}\u{3b9}"), (0x1F89, "\u{1f01}\u{3b9}"),
    (0x1F8A, "\u{1f02}\u{3b9}"), (0x1F8B, "\u{1f03}\u{3b9}"), (0x1F8C, "\u{1f04}\u{3b9}"),
    (0x1F8D, "\u{1f05}\u{3b9}"), (0x1F8E, "\u{1f06}\u{3b9}"), (0x1F8F, "\u{1f07}\u{3b9}"),
    (0x1F90, "\u{1f20}\u{3b9}"), (0x1F91, "\u{1f21}\u{3b9}"), (0x1F92, "\u{1f22}\u{3b9}"),
    (0x1F93, "\u{1f23}\u{3b9}"), (0x1F94, "\u{1f24}\u{3b9}"), (0x1F95, "\u{1f25}\u{3b9}"),
    (0x1F96, "\u{1f26}\u{3b9}"), (0x1F97, "\u{1f27}\u{3b9}"), (0x1F98, "\u{1f20}\u{3b9}"),
    (0x1F99, "\u{1f21}\u{3b9}"), (0x1F9A, "\u{1f22}\u{3b9}"), (0x1F9B, "\u{1f23}\u{3b9}"),
    (0x1F9C, "\u{1f24}\u{3b9}"), (0x1F9D, "\u{1f25}\u{3b9}"), (0x1F9E, "\u{1f26}\u{3b9}"),
    (0x1F9F, "\u{1f27}\u{3b9}"), (0x1FA0, "\u{1f60}\u{3b9}"), (0x1FA1, "\u{1f61}\u{3b9}"),
    (0x1FA2, "\u{1f62}\u{3b9}"), (0x1FA3, "\u{1f63}\u{3b9}"), (0x1FA4, "\u{1f64}\u{3b9}"),
    (0x1FA5, "\u{1f65}\u{3b9}"), (0x1FA6, "\u{1f66}\u{3b9}"), (0x1FA7, "\u{1f67}\u{3b9}"),
    (0x1FA8, "\u{1f60}\u{3b9}"), (0x1FA9, "\u{1f61}\u{3b9}"), (0x1FAA, "\u{1f62}\u{3b9}"),
    (0x1FAB, "\u{1f63}\u{3b9}"), (0x1FAC, "\u{1f64}\u{3b9}"), (0x1FAD, "\u{1f65}\u{3b9}"),
    (0x1FAE, "\u{1f66}\u{3b9}"), (0x1FAF, "\u{1f67}\u{3b9}"), (0x1FB2, "\u{1f70}\u{3b9}"),
    (0x1FB3, "\u{3b1}\u{3b9}"), (0x1FB4, "\u{3ac}\u{3b9}"), (0x1FB6, "\u{3b1}\u{342}"),
    (0x1FB7, "\u{3b1}\u{342}\u{3b9}"), (0x1FB8, "\u{1fb0}"), (0x1FB9, "\u{1fb1}"),
    (0x1FBA, "\u{1f70}"), (0x1FBB, "\u{1f71}"), (0x1FBC, "\u{3b1}\u{3b9}"),
    (0x1FBE, "\u{3b9}"), (0x1FC2, "\u{1f74}\u{3b9}"), (0x1FC3, "\u{3b7}\u{3b9}"),
    (0x1FC4, "\u{3ae}\u{3b9}"), (0x1FC6, "\u{3b7}\u{342}"), (0x1FC7, "\u{3b7}\u{342}\u{3b9}"),
    (0x1FC8, "\u{1f72}"), (0x1FC9, "\u{1f73}"), (0x1FCA, "\u{1f74}"), (0x1FCB, "\u{1f75}"),
    (0x1FCC, "\u{3b7}\u{3b9}"), (0x1FD2, "\u{3b9}\u{308}\u{300}"),
    (0x1FD3, "\u{3b9}\u{308}\u{301}"), (0x1FD6, "\u{3b9}\u{342}"),
    (0x1FD7, "\u{3b9}\u{308}\u{342}"), (0x1FD8, "\u{1fd0}"), (0x1FD9, "\u{1fd1}"),
    (0x1FDA, "\u{1f76}"), (0x1FDB, "\u{1f77}"), (0x1FE2, "\u{3c5}\u{308}\u{300}"),
    (0x1FE3, "\u{3c5}\u{308}\u{301}"), (0x1FE4, "\u{3c1}\u{313}"), (0x1FE6, "\u{3c5}\u{342}"),
    (0x1FE7, "\u{3c5}\u{308}\u{342}"), (0x1FE8, "\u{1fe0}"), (0x1FE9, "\u{1fe1}"),
    (0x1FEA, "\u{1f7a}"), (0x1FEB, "\u{1f7b}"), (0x1FEC, "\u{1fe5}"),
    (0x1FF2, "\u{1f7c}\u{3b9}"), (0x1FF3, "\u{3c9}\u{3b9}"), (0x1FF4, "\u{3ce}\u{3b9}"),
    (0x1FF6, "\u{3c9}\u{342}"), (0x1FF7, "\u{3c9}\u{342}\u{3b9}"), (0x1FF8, "\u{1f78}"),
    (0x1FF9, "\u{1f79}"), (0x1FFA, "\u{1f7c}"), (0x1FFB, "\u{1f7d}"),
    (0x1FFC, "\u{3c9}\u{3b9}"), (0x2126, "\u{3c9}"), (0x212A, "k"), (0x212B, "\u{e5}"),
    (0x2132, "\u{214e}"), (0x2160, "\u{2170}"), (0x2161, "\u{2171}"), (0x2162, "\u{2172}"),
    (0x2163, "\u{2173}"), (0x2164, "\u{2174}"), (0x2165, "\u{2175}"), (0x2166, "\u{2176}"),
    (0x2167, "\u{2177}"), (0x2168, "\u{2178}"), (0x2169, "\u{2179}"), (0x216A, "\u{217a}"),
    (0x216B, "\u{217b}"), (0x216C, "\u{217c}"), (0x216D, "\u{217d}"), (0x216E, "\u{217e}"),
    (0x216F, "\u{217f}"), (0x2183, "\u{2184}"), (0x24B6, "\u{24d0}"), (0x24B7, "\u{24d1}"),
    (0x24B8, "\u{24d2}"), (0x24B9, "\u{24d3}"), (0x24BA, "\u{24d4}"), (0x24BB, "\u{24d5}"),
    (0x24BC, "\u{24d6}"), (0x24BD, "\u{24d7}"), (0x24BE, "\u{24d8}"), (0x24BF, "\u{24d9}"),
    (0x24C0, "\u{24da}"), (0x24C1, "\u{24db}"), (0x24C2, "\u{24dc}"), (0x24C3, "\u{24dd}"),
    (0x24C4, "\u{24de}"), (0x24C5, "\u{24df}"), (0x24C6, "\u{24e0}"), (0x24C7, "\u{24e1}"),
    (0x24C8, "\u{24e2}"), (0x24C9, "\u{24e3}"), (0x24CA, "\u{24e4}"), (0x24CB, "\u{24e5}"),
    (0x24CC, "\u{24e6}"), (0x24CD, "\u{24e7}"), (0x24CE, "\u{24e8}"), (0x24CF, "\u{24e9}"),
    (0x2C00, "\u{2c30}"), (0x2C01, "\u{2c31}"), (0x2C02, "\u{2c32}"), (0x2C03, "\u{2c33}"),
    (0x2C04, "\u{2c34}"), (0x2C05, "\u{2c35}"), (0x2C06, "\u{2c36}"), (0x2C07, "\u{2c37}"),
    (0x2C08, "\u{2c38}"), (0x2C09, "\u{2c39}"), (0x2C0A, "\u{2c3a}"), (0x2C0B, "\u{2c3b}"),
    (0x2C0C, "\u{2c3c}"), (0x2C0D, "\u{2c3d}"), (0x2C0E, "\u{2c3e}"), (0x2C0F, "\u{2c3f}"),
    (0x2C10, "\u{2c40}"), (0x2C11, "\u{2c41}"), (0x2C12, "\u{2c42}"), (0x2C13, "\u{2c43}"),
    (0x2C14, "\u{2c44}"), (0x2C15, "\u{2c45}"), (0x2C16, "\u{2c46}"), (0x2C17, "\u{2c47}"),
    (0x2C18, "\u{2c48}"), (0x2C19, "\u{2c49}"), (0x2C1A, "\u{2c4a}"), (0x2C1B, "\u{2c4b}"),
    (0x2C1C, "\u{2c4c}"), (0x2C1D, "\u{2c4d}"), (0x2C1E, "\u{2c4e}"), (0x2C1F, "\u{2c4f}"),
    (0x2C20, "\u{2c50}"), (0x2C21, "\u{2c51}"), (0x2C22, "\u{2c52}"), (0x2C23, "\u{2c53}"),
    (0x2C24, "\u{2c54}"), (0x2C25, "\u{2c55}"), (0x2C26, "\u{2c56}"), (0x2C27, "\u{2c57}"),
    (0x2C28, "\u{2c58}"), (0x2C29, "\u{2c59}"), (0x2C2A, "\u{2c5a}"), (0x2C2B, "\u{2c5b}"),
    (0x2C2C, "\u{2c5c}"), (0x2C2D, "\u{2c5d}"), (0x2C2E, "\u{2c5e}"), (0x2C2F, "\u{2c5f}"),
    (0x2C60, "\u{2c61}"), (0x2C62, "\u{26b}"), (0x2C63, "\u{1d7d}"), (0x2C64, "\u{27d}"),
    (0x2C67, "\u{2c68}"), (0x2C69, "\u{2c6a}"), (0x2C6B, "\u{2c6c}"), (0x2C6D, "\u{251}"),
    (0x2C6E, "\u{271}"), (0x2C6F, "\u{250}"), (0x2C70, "\u{252}"), (0x2C72, "\u{2c73}"),
    (0x2C75, "\u{2c76}"), (0x2C7E, "\u{23f}"), (0x2C7F, "\u{240}"), (0x2C80, "\u{2c81}"),
    (0x2C82, "\u{2c83}"), (0x2C84, "\u{2c85}"), (0x2C86, "\u{2c87}"), (0x2C88, "\u{2c89}"),
    (0x2C8A, "\u{2c8b}"), (0x2C8C, "\u{2c8d}"), (0x2C8E, "\u{2c8f}"), (0x2C90, "\u{2c91}"),
    (0x2C92, "\u{2c93}"), (0x2C94, "\u{2c95}"), (0x2C96, "\u{2c97}"), (0x2C98, "\u{2c99}"),
    (0x2C9A, "\u{2c9b}"), (0x2C9C, "\u{2c9d}"), (0x2C9E, "\u{2c9f}"), (0x2CA0, "\u{2ca1}"),
    (0x2CA2, "\u{2ca3}"), (0x2CA4, "\u{2ca5}"), (0x2CA6, "\u{2ca7}"), (0x2CA8, "\u{2ca9}"),
    (0x2CAA, "\u{2cab}"), (0x2CAC, "\u{2cad}"), (0x2CAE, "\u{2caf}"), (0x2CB0, "\u{2cb1}"),
    (0x2CB2, "\u{2cb3}"), (0x2CB4, "\u{2cb5}"), (0x2CB6, "\u{2cb7}"), (0x2CB8, "\u{2cb9}"),
    (0x2CBA, "\u{2cbb}"), (0x2CBC, "\u{2cbd}"), (0x2CBE, "\u{2cbf}"), (0x2CC0, "\u{2cc1}"),
    (0x2CC2, "\u{2cc3}"), (0x2CC4, "\u{2cc5}"), (0x2CC6, "\u{2cc7}"), (0x2CC8, "\u{2cc9}"),
    (0x2CCA, "\u{2ccb}"), (0x2CCC, "\u{2ccd}"), (0x2CCE, "\u{2ccf}"), (0x2CD0, "\u{2cd1}"),
    (0x2CD2, "\u{2cd3}"), (0x2CD4, "\u{2cd5}"), (0x2CD6, "\u{2cd7}"), (0x2CD8, "\u{2cd9}"),
    (0x2CDA, "\u{2cdb}"), (0x2CDC, "\u{2cdd}"), (0x2CDE, "\u{2cdf}"), (0x2CE0, "\u{2ce1}"),
    (0x2CE2, "\u{2ce3}"), (0x2CEB, "\u{2cec}"), (0x2CED, "\u{2cee}"), (0x2CF2, "\u{2cf3}"),
    (0xA640, "\u{a641}"), (0xA642, "\u{a643}"), (0xA644, "\u{a645}"), (0xA646, "\u{a647}"),
    (0xA648, "\u{a649}"), (0xA64A, "\u{a64b}"), (0xA64C, "\u{a64d}"), (0xA64E, "\u{a64f}"),
    (0xA650, "\u{a651}"), (0xA652, "\u{a653}"), (0xA654, "\u{a655}"), (0xA656, "\u{a657}"),
    (0xA658, "\u{a659}"), (0xA65A, "\u{a65b}"), (0xA65C, "\u{a65d}"), (0xA65E, "\u{a65f}"),
    (0xA660, "\u{a661}"), (0xA662, "\u{a663}"), (0xA664, "\u{a665}"), (0xA666, "\u{a667}"),
    (0xA668, "\u{a669}"), (0xA66A, "\u{a66b}"), (0xA66C, "\u{a66d}"), (0xA680, "\u{a681}"),
    (0xA682, "\u{a683}"), (0xA684, "\u{a685}"), (0xA686, "\u{a687}"), (0xA688, "\u{a689}"),
    (0xA68A, "\u{a68b}"), (0xA68C, "\u{a68d}"), (0xA68E, "\u{a68f}"), (0xA690, "\u{a691}"),
    (0xA692, "\u{a693}"), (0xA694, "\u{a695}"), (0xA696, "\u{a697}"), (0xA698, "\u{a699}"),
    (0xA69A, "\u{a69b}"), (0xA722, "\u{a723}"), (0xA724, "\u{a725}"), (0xA726, "\u{a727}"),
    (0xA728, "\u{a729}"), (0xA72A, "\u{a72b}"), (0xA72C, "\u{a72d}"), (0xA72E, "\u{a72f}"),
    (0xA732, "\u{a733}"), (0xA734, "\u{a735}"), (0xA736, "\u{a737}"), (0xA738, "\u{a739}"),
    (0xA73A, "\u{a73b}"), (0xA73C, "\u{a73d}"), (0xA73E, "\u{a73f}"), (0xA740, "\u{a741}"),
    (0xA742, "\u{a743}"), (0xA744, "\u{a745}"), (0xA746, "\u{a747}"), (0xA748, "\u{a749}"),
    (0xA74A, "\u{a74b}"), (0xA74C, "\u{a74d}"), (0xA74E, "\u{a74f}"), (0xA750, "\u{a751}"),
    (0xA752, "\u{a753}"), (0xA754, "\u{a755}"), (0xA756, "\u{a757}"), (0xA758, "\u{a759}"),
    (0xA75A, "\u{a75b}"), (0xA75C, "\u{a75d}"), (0xA75E, "\u{a75f}"), (0xA760, "\u{a761}"),
    (0xA762, "\u{a763}"), (0xA764, "\u{a765}"), (0xA766, "\u{a767}"), (0xA768, "\u{a769}"),
    (0xA76A, "\u{a76b}"), (0xA76C, "\u{a76d}"), (0xA76E, "\u{a76f}"), (0xA779, "\u{a77a}"),
    (0xA77B, "\u{a77c}"), (0xA77D, "\u{1d79}"), (0xA77E, "\u{a77f}"), (0xA780, "\u{a781}"),
    (0xA782, "\u{a783}"), (0xA784, "\u{a785}"), (0xA786, "\u{a787}"), (0xA78B, "\u{a78c}"),
    (0xA78D, "\u{265}"), (0xA790, "\u{a791}"), (0xA792, "\u{a793}"), (0xA796, "\u{a797}"),
    (0xA798, "\u{a799}"), (0xA79A, "\u{a79b}"), (0xA79C, "\u{a79d}"), (0xA79E, "\u{a79f}"),
    (0xA7A0, "\u{a7a1}"), (0xA7A2, "\u{a7a3}"), (0xA7A4, "\u{a7a5}"), (0xA7A6, "\u{a7a7}"),
    (0xA7A8, "\u{a7a9}"), (0xA7AA, "\u{266}"), (0xA7AB, "\u{25c}"), (0xA7AC, "\u{261}"),
    (0xA7AD, "\u{26c}"), (0xA7AE, "\u{26a}"), (0xA7B0, "\u{29e}"), (0xA7B1, "\u{287}"),
    (0xA7B2, "\u{29d}"), (0xA7B3, "\u{ab53}"), (0xA7B4, "\u{a7b5}"), (0xA7B6, "\u{a7b7}"),
    (0xA7B8, "\u{a7b9}"), (0xA7BA, "\u{a7bb}"), (0xA7BC, "\u{a7bd}"), (0xA7BE, "\u{a7bf}"),
    (0xA7C0, "\u{a7c1}"), (0xA7C2, "\u{a7c3}"), (0xA7C4, "\u{a794}"), (0xA7C5, "\u{282}"),
    (0xA7C6, "\u{1d8e}"), (0xA7C7, "\u{a7c8}"), (0xA7C9, "\u{a7ca}"), (0xA7D0, "\u{a7d1}"),
    (0xA7D6, "\u{a7d7}"), (0xA7D8, "\u{a7d9}"), (0xA7F5, "\u{a7f6}"), (0xAB70, "\u{13a0}"),
    (0xAB71, "\u{13a1}"), (0xAB72, "\u{13a2}"), (0xAB73, "\u{13a3}"), (0xAB74, "\u{13a4}"),
    (0xAB75, "\u{13a5}"), (0xAB76, "\u{13a6}"), (0xAB77, "\u{13a7}"), (0xAB78, "\u{13a8}"),
    (0xAB79, "\u{13a9}"), (0xAB7A, "\u{13aa}"), (0xAB7B, "\u{13ab}"), (0xAB7C, "\u{13ac}"),
    (0xAB7D, "\u{13ad}"), (0xAB7E, "\u{13ae}"), (0xAB7F, "\u{13af}"), (0xAB80, "\u{13b0}"),
    (0xAB81, "\u{13b1}"), (0xAB82, "\u{13b2}"), (0xAB83, "\u{13b3}"), (0xAB84, "\u{13b4}"),
    (0xAB85, "\u{13b5}"), (0xAB86, "\u{13b6}"), (0xAB87, "\u{13b7}"), (0xAB88, "\u{13b8}"),
    (0xAB89, "\u{13b9}"), (0xAB8A, "\u{13ba}"), (0xAB8B, "\u{13bb}"), (0xAB8C, "\u{13bc}"),
    (0xAB8D, "\u{13bd}"), (0xAB8E, "\u{13be}"), (0xAB8F, "\u{13bf}"), (0xAB90, "\u{13c0}"),
    (0xAB91, "\u{13c1}"), (0xAB92, "\u{13c2}"), (0xAB93, "\u{13c3}"), (0xAB94, "\u{13c4}"),
    (0xAB95, "\u{13c5}"), (0xAB96, "\u{13c6}"), (0xAB97, "\u{13c7}"), (0xAB98, "\u{13c8}"),
    (0xAB99, "\u{13c9}"), (0xAB9A, "\u{13ca}"), (0xAB9B, "\u{13cb}"), (0xAB9C, "\u{13cc}"),
    (0xAB9D, "\u{13cd}"), (0xAB9E, "\u{13ce}"), (0xAB9F, "\u{13cf}"), (0xABA0, "\u{13d0}"),
    (0xABA1, "\u{13d1}"), (0xABA2, "\u{13d2}"), (0xABA3, "\u{13d3}"), (0xABA4, "\u{13d4}"),
    (0xABA5, "\u{13d5}"), (0xABA6, "\u{13d6}"), (0xABA7, "\u{13d7}"), (0xABA8, "\u{13d8}"),
    (0xABA9, "\u{13d9}"), (0xABAA, "\u{13da}"), (0xABAB, "\u{13db}"), (0xABAC, "\u{13dc}"),
    (0xABAD, "\u{13dd}"), (0xABAE, "\u{13de}"), (0xABAF, "\u{13df}"), (0xABB0, "\u{13e0}"),
    (0xABB1, "\u{13e1}"), (0xABB2, "\u{13e2}"), (0xABB3, "\u{13e3}"), (0xABB4, "\u{13e4}"),
    (0xABB5, "\u{13e5}"), (0xABB6, "\u{13e6}"), (0xABB7, "\u{13e7}"), (0xABB8, "\u{13e8}"),
    (0xABB9, "\u{13e9}"), (0xABBA, "\u{13ea}"), (0xABBB, "\u{13eb}"), (0xABBC, "\u{13ec}"),
    (0xABBD, "\u{13ed}"), (0xABBE, "\u{13ee}"), (0xABBF, "\u{13ef}"), (0xFB00, "ff"),
    (0xFB01, "fi"), (0xFB02, "fl"), (0xFB03, "ffi"), (0xFB04, "ffl"), (0xFB05, "st"),
    (0xFB06, "st"), (0xFB13, "\u{574}\u{576}"), (0xFB14, "\u{574}\u{565}"),
    (0xFB15, "\u{574}\u{56b}"), (0xFB16, "\u{57e}\u{576}"), (0xFB17, "\u{574}\u{56d}"),
    (0xFF21, "\u{ff41}"), (0xFF22, "\u{ff42}"), (0xFF23, "\u{ff43}"), (0xFF24, "\u{ff44}"),
    (0xFF25, "\u{ff45}"), (0xFF26, "\u{ff46}"), (0xFF27, "\u{ff47}"), (0xFF28, "\u{ff48}"),
    (0xFF29, "\u{ff49}"), (0xFF2A, "\u{ff4a}"), (0xFF2B, "\u{ff4b}"), (0xFF2C, "\u{ff4c}"),
    (0xFF2D, "\u{ff4d}"), (0xFF2E, "\u{ff4e}"), (0xFF2F, "\u{ff4f}"), (0xFF30, "\u{ff50}"),
    (0xFF31, "\u{ff51}"), (0xFF32, "\u{ff52}"), (0xFF33, "\u{ff53}"), (0xFF34, "\u{ff54}"),
    (0xFF35, "\u{ff55}"), (0xFF36, "\u{ff56}"), (0xFF37, "\u{ff57}"), (0xFF38, "\u{ff58}"),
    (0xFF39, "\u{ff59}"), (0xFF3A, "\u{ff5a}"), (0x10400, "\u{10428}"), (0x10401, "\u{10429}"),
    (0x10402, "\u{1042a}"), (0x10403, "\u{1042b}"), (0x10404, "\u{1042c}"),
    (0x10405, "\u{1042d}"), (0x10406, "\u{1042e}"), (0x10407, "\u{1042f}"),
    (0x10408, "\u{10430}"), (0x10409, "\u{10431}"), (0x1040A, "\u{10432}"),
    (0x1040B, "\u{10433}"), (0x1040C, "\u{10434}"), (0x1040D, "\u{10435}"),
    (0x1040E, "\u{10436}"), (0x1040F, "\u{10437}"), (0x10410, "\u{10438}"),
    (0x10411, "\u{10439}"), (0x10412, "\u{1043a}"), (0x10413, "\u{1043b}"),
    (0x10414, "\u{1043c}"), (0x10415, "\u{1043d}"), (0x10416, "\u{1043e}"),
    (0x10417, "\u{1043f}"), (0x10418, "\u{10440}"), (0x10419, "\u{10441}"),
    (0x1041A, "\u{10442}"), (0x1041B, "\u{10443}"), (0x1041C, "\u{10444}"),
    (0x1041D, "\u{10445}"), (0x1041E, "\u{10446}"), (0x1041F, "\u{10447}"),
    (0x10420, "\u{10448}"), (0x10421, "\u{10449}"), (0x10422, "\u{1044a}"),
    (0x10423, "\u{1044b}"), (0x10424, "\u{1044c}"), (0x10425, "\u{1044d}"),
    (0x10426, "\u{1044e}"), (0x10427, "\u{1044f}"), (0x104B0, "\u{104d8}"),
    (0x104B1, "\u{104d9}"), (0x104B2, "\u{104da}"), (0x104B3, "\u{104db}"),
    (0x104B4, "\u{104dc}"), (0x104B5, "\u{104dd}"), (0x104B6, "\u{104de}"),
    (0x104B7, "\u{104df}"), (0x104B8, "\u{104e0}"), (0x104B9, "\u{104e1}"),
    (0x104BA, "\u{104e2}"), (0x104BB, "\u{104e3}"), (0x104BC, "\u{104e4}"),
    (0x104BD, "\u{104e5}"), (0x104BE, "\u{104e6}"), (0x104BF, "\u{104e7}"),
    (0x104C0, "\u{104e8}"), (0x104C1, "\u{104e9}"), (0x104C2, "\u{104ea}"),
    (0x104C3, "\u{104eb}"), (0x104C4, "\u{104ec}"), (0x104C5, "\u{104ed}"),
    (0x104C6, "\u{104ee}"), (0x104C7, "\u{104ef}"), (0x104C8, "\u{104f0}"),
    (0x104C9, "\u{104f1}"), (0x104CA, "\u{104f2}"), (0x104CB, "\u{104f3}"),
    (0x104CC, "\u{104f4}"), (0x104CD, "\u{104f5}"), (0x104CE, "\u{104f6}"),
    (0x104CF, "\u{104f7}"), (0x104D0, "\u{104f8}"), (0x104D1, "\u{104f9}"),
    (0x104D2, "\u{104fa}"), (0x104D3, "\u{104fb}"), (0x10570, "\u{10597}"),
    (0x10571, "\u{10598}"), (0x10572, "\u{10599}"), (0x10573, "\u{1059a}"),
    (0x10574, "\u{1059b}"), (0x10575, "\u{1059c}"), (0x10576, "\u{1059d}"),
    (0x10577, "\u{1059e}"), (0x10578, "\u{1059f}"), (0x10579, "\u{105a0}"),
    (0x1057A, "\u{105a1}"), (0x1057C, "\u{105a3}"), (0x1057D, "\u{105a4}"),
    (0x1057E, "\u{105a5}"), (0x1057F, "\u{105a6}"), (0x10580, "\u{105a7}"),
    (0x10581, "\u{105a8}"), (0x10582, "\u{105a9}"), (0x10583, "\u{105aa}"),
    (0x10584, "\u{105ab}"), (0x10585, "\u{105ac}"), (0x10586, "\u{105ad}"),
    (0x10587, "\u{105ae}"), (0x10588, "\u{105af}"), (0x10589, "\u{105b0}"),
    (0x1058A, "\u{105b1}"), (0x1058C, "\u{105b3}"), (0x1058D, "\u{105b4}"),
    (0x1058E, "\u{105b5}"), (0x1058F, "\u{105b6}"), (0x10590, "\u{105b7}"),
    (0x10591, "\u{105b8}"), (0x10592, "\u{105b9}"), (0x10594, "\u{105bb}"),
    (0x10595, "\u{105bc}"), (0x10C80, "\u{10cc0}"), (0x10C81, "\u{10cc1}"),
    (0x10C82, "\u{10cc2}"), (0x10C83, "\u{10cc3}"), (0x10C84, "\u{10cc4}"),
    (0x10C85, "\u{10cc5}"), (0x10C86, "\u{10cc6}"), (0x10C87, "\u{10cc7}"),
    (0x10C88, "\u{10cc8}"), (0x10C89, "\u{10cc9}"), (0x10C8A, "\u{10cca}"),
    (0x10C8B, "\u{10ccb}"), (0x10C8C, "\u{10ccc}"), (0x10C8D, "\u{10ccd}"),
    (0x10C8E, "\u{10cce}"), (0x10C8F, "\u{10ccf}"), (0x10C90, "\u{10cd0}"),
    (0x10C91, "\u{10cd1}"), (0x10C92, "\u{10cd2}"), (0x10C93, "\u{10cd3}"),
    (0x10C94, "\u{10cd4}"), (0x10C95, "\u{10cd5}"), (0x10C96, "\u{10cd6}"),
    (0x10C97, "\u{10cd7}"), (0x10C98, "\u{10cd8}"), (0x10C99, "\u{10cd9}"),
    (0x10C9A, "\u{10cda}"), (0x10C9B, "\u{10cdb}"), (0x10C9C, "\u{10cdc}"),
    (0x10C9D, "\u{10cdd}"), (0x10C9E, "\u{10cde}"), (0x10C9F, "\u{10cdf}"),
    (0x10CA0, "\u{10ce0}"), (0x10CA1, "\u{10ce1}"), (0x10CA2, "\u{10ce2}"),
    (0x10CA3, "\u{10ce3}"), (0x10CA4, "\u{10ce4}"), (0x10CA5, "\u{10ce5}"),
    (0x10CA6, "\u{10ce6}"), (0x10CA7, "\u{10ce7}"), (0x10CA8, "\u{10ce8}"),
    (0x10CA9, "\u{10ce9}"), (0x10CAA, "\u{10cea}"), (0x10CAB, "\u{10ceb}"),
    (0x10CAC, "\u{10cec}"), (0x10CAD, "\u{10ced}"), (0x10CAE, "\u{10cee}"),
    (0x10CAF, "\u{10cef}"), (0x10CB0, "\u{10cf0}"), (0x10CB1, "\u{10cf1}"),
    (0x10CB2, "\u{10cf2}"), (0x118A0, "\u{118c0}"), (0x118A1, "\u{118c1}"),
    (0x118A2, "\u{118c2}"), (0x118A3, "\u{118c3}"), (0x118A4, "\u{118c4}"),
    (0x118A5, "\u{118c5}"), (0x118A6, "\u{118c6}"), (0x118A7, "\u{118c7}"),
    (0x118A8, "\u{118c8}"), (0x118A9, "\u{118c9}"), (0x118AA, "\u{118ca}"),
    (0x118AB, "\u{118cb}"), (0x118AC, "\u{118cc}"), (0x118AD, "\u{118cd}"),
    (0x118AE, "\u{118ce}"), (0x118AF, "\u{118cf}"), (0x118B0, "\u{118d0}"),
    (0x118B1, "\u{118d1}"), (0x118B2, "\u{118d2}"), (0x118B3, "\u{118d3}"),
    (0x118B4, "\u{118d4}"), (0x118B5, "\u{118d5}"), (0x118B6, "\u{118d6}"),
    (0x118B7, "\u{118d7}"), (0x118B8, "\u{118d8}"), (0x118B9, "\u{118d9}"),
    (0x118BA, "\u{118da}"), (0x118BB, "\u{118db}"), (0x118BC, "\u{118dc}"),
    (0x118BD, "\u{118dd}"), (0x118BE, "\u{118de}"), (0x118BF, "\u{118df}"),
    (0x16E40, "\u{16e60}"), (0x16E41, "\u{16e61}"), (0x16E42, "\u{16e62}"),
    (0x16E43, "\u{16e63}"), (0x16E44, "\u{16e64}"), (0x16E45, "\u{16e65}"),
    (0x16E46, "\u{16e66}"), (0x16E47, "\u{16e67}"), (0x16E48, "\u{16e68}"),
    (0x16E49, "\u{16e69}"), (0x16E4A, "\u{16e6a}"), (0x16E4B, "\u{16e6b}"),
    (0x16E4C, "\u{16e6c}"), (0x16E4D, "\u{16e6d}"), (0x16E4E, "\u{16e6e}"),
    (0x16E4F, "\u{16e6f}"), (0x16E50, "\u{16e70}"), (0x16E51, "\u{16e71}"),
    (0x16E52, "\u{16e72}"), (0x16E53, "\u{16e73}"), (0x16E54, "\u{16e74}"),
    (0x16E55, "\u{16e75}"), (0x16E56, "\u{16e76}"), (0x16E57, "\u{16e77}"),
    (0x16E58, "\u{16e78}"), (0x16E59, "\u{16e79}"), (0x16E5A, "\u{16e7a}"),
    (0x16E5B, "\u{16e7b}"), (0x16E5C, "\u{16e7c}"), (0x16E5D, "\u{16e7d}"),
    (0x16E5E, "\u{16e7e}"), (0x16E5F, "\u{16e7f}"), (0x1E900, "\u{1e922}"),
    (0x1E901, "\u{1e923}"), (0x1E902, "\u{1e924}"), (0x1E903, "\u{1e925}"),
    (0x1E904, "\u{1e926}"), (0x1E905, "\u{1e927}"), (0x1E906, "\u{1e928}"),
    (0x1E907, "\u{1e929}"), (0x1E908, "\u{1e92a}"), (0x1E909, "\u{1e92b}"),
    (0x1E90A, "\u{1e92c}"), (0x1E90B, "\u{1e92d}"), (0x1E90C, "\u{1e92e}"),
    (0x1E90D, "\u{1e92f}"), (0x1E90E, "\u{1e930}"), (0x1E90F, "\u{1e931}"),
    (0x1E910, "\u{1e932}"), (0x1E911, "\u{1e933}"), (0x1E912, "\u{1e934}"),
    (0x1E913, "\u{1e935}"), (0x1E914, "\u{1e936}"), (0x1E915, "\u{1e937}"),
    (0x1E916, "\u{1e938}"), (0x1E917, "\u{1e939}"), (0x1E918, "\u{1e93a}"),
    (0x1E919, "\u{1e93b}"), (0x1E91A, "\u{1e93c}"), (0x1E91B, "\u{1e93d}"),
    (0x1E91C, "\u{1e93e}"), (0x1E91D, "\u{1e93f}"), (0x1E91E, "\u{1e940}"),
    (0x1E91F, "\u{1e941}"), (0x1E920, "\u{1e942}"), (0x1E921, "\u{1e943}"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    fn memory(id: &str, summary: &str) -> Memory {
        Memory {
            id: id.to_string(),
            title: "Title".to_string(),
            summary: summary.to_string(),
            tags: Vec::new(),
            created_at: "2026-08-22T01:02:03.000000Z".to_string(),
            updated_at: "2026-08-22T01:02:03.000000Z".to_string(),
            body: "Body".to_string(),
            kind: "persona".to_string(),
            project: None,
            relative_path: format!("persona/{id}.md"),
        }
    }

    fn ulid_timestamp_ms(value: &str) -> u128 {
        let mut number: u128 = 0;
        for c in value.chars() {
            let digit = ULID_ALPHABET
                .iter()
                .position(|b| *b as char == c)
                .expect("ulid char");
            number = (number << 5) | digit as u128;
        }
        number >> 80
    }

    #[test]
    fn nfkc_matches_the_python_reference() {
        let cases: &[(&str, &str)] = &[
            ("\u{fb01}", "fi"),
            ("\u{2460}", "1"),
            ("\u{bd}", "1\u{2044}2"),
            ("\u{b2}", "2"),
            ("\u{2126}", "\u{3a9}"),
            ("\u{212b}", "\u{c5}"),
            ("\u{ff21}", "A"),
            ("\u{ff76}", "\u{30ab}"),
            ("\u{ff76}\u{ff9e}", "\u{30ac}"),
            ("\u{3131}", "\u{1100}"),
            ("\u{3131}\u{314f}", "\u{ac00}"),
            ("\u{ac00}", "\u{ac00}"),
            ("\u{1100}\u{1161}", "\u{ac00}"),
            ("e\u{301}", "\u{e9}"),
            ("A\u{30a}\u{301}", "\u{1fa}"),
            ("\u{344}", "\u{308}\u{301}"),
            ("\u{212a}", "K"),
            ("\u{337f}", "\u{682a}\u{5f0f}\u{4f1a}\u{793e}"),
            ("\u{958}", "\u{915}\u{93c}"),
            ("\u{33a1}", "m2"),
            ("\u{1e9b}", "\u{1e61}"),
            ("\u{1fbc}", "\u{1fbc}"),
            ("\u{1fb3}", "\u{1fb3}"),
            ("\u{390}", "\u{390}"),
            ("q\u{307}\u{323}", "q\u{323}\u{307}"),
            (
                "\u{fdfa}",
                "\u{635}\u{644}\u{649} \u{627}\u{644}\u{644}\u{647} \u{639}\u{644}\u{64a}\u{647} \u{648}\u{633}\u{644}\u{645}",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(&nfkc(input), expected, "nfkc({input:?})");
        }
    }

    #[test]
    fn casefold_matches_the_python_reference() {
        assert_eq!(casefold("Straße"), "strasse");
        assert_eq!(casefold("STRASSE"), "strasse");
        assert_eq!(casefold("\u{3c2}"), "\u{3c3}");
        assert_eq!(casefold("\u{130}"), "i\u{307}");
        assert_eq!(casefold("\u{1e9e}"), "ss");
        assert_eq!(casefold("\u{fb01}"), "fi");
        assert_eq!(casefold("\u{17f}"), "s");
        assert_eq!(casefold("\u{345}"), "\u{3b9}");
    }

    #[test]
    fn title_uses_nfkc_whitespace_collapse_and_casefold_key() {
        assert_eq!(
            normalize_title(
                "  \u{ff32}\u{ff45}\u{ff4c}\u{ff45}\u{ff41}\u{ff53}\u{ff45}\t\n notes  "
            )
            .unwrap(),
            "Release notes"
        );
        // U+001C is Python whitespace: folded, not treated as a control.
        assert_eq!(normalize_title("a\u{1c}b \u{1c}c").unwrap(), "a b c");
        assert_eq!(normalize_title("a\u{2028}b").unwrap(), "a b");
        assert_eq!(title_key("Straße").unwrap(), title_key("STRASSE").unwrap());
        assert_eq!(title_key("Straße").unwrap(), "strasse");
    }

    #[test]
    fn title_rejects_empty_overlong_and_control_values() {
        assert_eq!(normalize_title("").unwrap_err(), "title is empty");
        assert_eq!(normalize_title(" \t\n ").unwrap_err(), "title is empty");
        assert_eq!(normalize_title("\u{a0}").unwrap_err(), "title is empty");
        assert_eq!(
            normalize_title("a\u{0}b").unwrap_err(),
            "title contains a control character"
        );
        assert_eq!(
            normalize_title("a\u{99}b").unwrap_err(),
            "title contains a control character"
        );
        assert_eq!(
            normalize_title(&"x".repeat(121)).unwrap_err(),
            "title is too long"
        );
        assert_eq!(normalize_title(&"x".repeat(120)).unwrap(), "x".repeat(120));
    }

    #[test]
    fn summary_is_required_single_line_plain_text() {
        assert_eq!(
            normalize_summary("  What\nthis\tmemory covers.  ").unwrap(),
            "What this memory covers."
        );
        assert_eq!(normalize_summary(" \n ").unwrap_err(), "summary is empty");
        assert_eq!(
            normalize_summary(&"x".repeat(MAX_SUMMARY_LENGTH + 1)).unwrap_err(),
            "summary is too long"
        );
        assert_eq!(
            normalize_summary("summary\u{0}").unwrap_err(),
            "summary contains a control character"
        );
    }

    #[test]
    fn body_normalizes_line_endings_and_enforces_limits() {
        assert_eq!(
            normalize_body("\r\nfirst\rsecond\r\n").unwrap(),
            "first\nsecond"
        );
        assert_eq!(normalize_body("\n\nbody\n\n").unwrap(), "body");
        assert_eq!(normalize_body("first\nsecond\n").unwrap(), "first\nsecond");
        assert_eq!(normalize_body(" \t\nhere\n\t ").unwrap(), " \t\nhere\n\t ");
        assert_eq!(normalize_body("\r\n \t\r\n").unwrap_err(), "body is empty");
        assert_eq!(normalize_body("\u{a0}").unwrap_err(), "body is empty");
        assert_eq!(
            normalize_body("a\nb\tc\u{1c}").unwrap_err(),
            "body contains a control character"
        );
        assert_eq!(
            normalize_body(&"x".repeat(MAX_BODY_LENGTH + 1)).unwrap_err(),
            "body is too long"
        );
        assert_eq!(
            normalize_body("bad\n\u{0}tag").unwrap_err(),
            "body contains a control character"
        );
        assert_eq!(
            normalize_body("bad\u{7}tag").unwrap_err(),
            "body contains a control character"
        );
    }

    #[test]
    fn project_is_nfkc_normalized_lowercase() {
        assert_eq!(
            normalize_project("  \u{ff2d}\u{ff59}_Project-1.0  ").unwrap(),
            "my_project-1.0"
        );
        assert_eq!(normalize_project("My_App").unwrap(), "my_app");
    }

    #[test]
    fn project_rejects_empty_traversal_and_unsupported_values() {
        for project in [
            "",
            ".",
            "..",
            "a..b",
            "../escape",
            "a/b",
            "a\\b",
            "space name",
            "app.",
        ] {
            assert!(normalize_project(project).is_err(), "{project:?}");
        }
        assert_eq!(
            normalize_project(&"x".repeat(65)).unwrap_err(),
            "project is too long"
        );
        assert_eq!(
            normalize_project(" ../escape").unwrap_err(),
            "project contains path traversal"
        );
        assert_eq!(
            normalize_project("space name").unwrap_err(),
            "project contains unsupported characters"
        );
    }

    #[test]
    fn project_rejects_windows_reserved_names() {
        for project in [
            "con", "nul", "aux", "prn", "com1", "com9", "lpt1", "lpt9", "con.x",
        ] {
            assert_eq!(
                normalize_project(project).unwrap_err(),
                "project is not a portable directory name",
                "{project:?}"
            );
        }
        assert!(normalize_project("com10").is_ok());
        assert!(normalize_project("con-vention").is_ok());
        assert_eq!(
            normalize_project("Com3.backup").unwrap_err(),
            "project is not a portable directory name"
        );
    }

    #[test]
    fn kind_is_normalized_and_restricted() {
        assert_eq!(normalize_kind("  Persona ").unwrap(), "persona");
        assert_eq!(normalize_kind("PROJECT").unwrap(), "project");
        assert_eq!(normalize_kind(" playbook\n").unwrap(), "playbook");
        assert_eq!(
            normalize_kind("global").unwrap_err(),
            "kind must be persona, project, or playbook"
        );
        assert_eq!(normalize_kind("\u{1c}persona\u{1c}").unwrap(), "persona");
    }

    #[test]
    fn location_requires_a_slug_exactly_for_project_kind() {
        assert_eq!(
            validate_location("persona", None).unwrap(),
            ("persona".into(), None)
        );
        assert_eq!(
            validate_location("playbook", None).unwrap(),
            ("playbook".into(), None)
        );
        assert_eq!(
            validate_location("project", Some("My_App")).unwrap(),
            ("project".into(), Some("my_app".into()))
        );
        assert_eq!(
            validate_location("project", None).unwrap_err(),
            "project kind requires a project slug"
        );
        assert_eq!(
            validate_location("persona", Some("my_app")).unwrap_err(),
            "persona kind does not take a project slug"
        );
        assert_eq!(
            validate_location("playbook", Some("my_app")).unwrap_err(),
            "playbook kind does not take a project slug"
        );
    }

    #[test]
    fn location_label_names_each_kind() {
        assert_eq!(location_label("persona", None).unwrap(), "persona");
        assert_eq!(location_label("playbook", None).unwrap(), "playbook");
        assert_eq!(
            location_label("project", Some("my_app")).unwrap(),
            "project/my_app"
        );
    }

    #[test]
    fn tags_are_normalized_deduplicated_and_sorted_by_casefold() {
        assert_eq!(normalize_tags(vec![]).unwrap(), Vec::<String>::new());
        assert_eq!(
            normalize_tags(vec![
                "  Zeta ".into(),
                "alpha".into(),
                "ALPHA".into(),
                "beta".into()
            ])
            .unwrap(),
            vec!["alpha".to_string(), "beta".to_string(), "Zeta".to_string()]
        );
        assert_eq!(
            normalize_tags(vec!["\u{ff22}\u{ff45}\u{ff54}\u{ff41}".into()]).unwrap(),
            vec!["Beta".to_string()]
        );
        assert_eq!(
            normalize_tags(vec!["stra\u{df}e".into(), "STRASSE".into()]).unwrap(),
            vec!["stra\u{df}e".to_string()]
        );
        assert_eq!(
            normalize_tags(vec!["\u{1c}x\u{1c}".into()]).unwrap(),
            vec!["x".to_string()]
        );
    }

    #[test]
    fn tags_reject_invalid_values() {
        assert_eq!(
            normalize_tags(vec!["  ".into()]).unwrap_err(),
            "tags contain an empty value"
        );
        assert_eq!(
            normalize_tags(vec!["x".repeat(MAX_TAG_LENGTH + 1)]).unwrap_err(),
            "tags contain a value that is too long"
        );
        assert_eq!(
            normalize_tags(vec!["bad\ntag".into()]).unwrap_err(),
            "tags contain a control character"
        );
        assert_eq!(
            normalize_tags(vec!["bad\u{0}tag".into()]).unwrap_err(),
            "tags contain a control character"
        );
        let too_many: Vec<String> = (0..=MAX_TAGS).map(|index| format!("tag-{index}")).collect();
        assert_eq!(
            normalize_tags(too_many).unwrap_err(),
            "tags exceed the maximum of 8"
        );
    }

    #[test]
    fn tags_deduplicate_before_counting_the_limit() {
        let mut duplicated: Vec<String> =
            (0..MAX_TAGS).map(|index| format!("Tag-{index}")).collect();
        duplicated.push("tag-0".into());
        let normalized = normalize_tags(duplicated).unwrap();
        assert_eq!(normalized.len(), MAX_TAGS);
        assert_eq!(normalized.last().unwrap(), "Tag-7");
        assert_eq!(
            normalized,
            vec![
                "Tag-0".to_string(),
                "Tag-1".to_string(),
                "Tag-2".to_string(),
                "Tag-3".to_string(),
                "Tag-4".to_string(),
                "Tag-5".to_string(),
                "Tag-6".to_string(),
                "Tag-7".to_string(),
            ]
        );
    }

    #[test]
    fn ulid_generation_validation_and_memory_paths() {
        let generated = new_ulid(Some(Utc.with_ymd_and_hms(2026, 8, 22, 0, 0, 0).unwrap()));
        assert_eq!(validate_ulid(&generated).unwrap(), generated);
        assert_eq!(generated.chars().count(), 26);
        assert_eq!(ulid_timestamp_ms(&generated), 1_787_356_800_000);
        assert_eq!(
            memory_path(&generated, "persona", None).unwrap(),
            format!("persona/{generated}.md")
        );
        assert_eq!(
            memory_path(&generated, "playbook", None).unwrap(),
            format!("playbooks/{generated}.md")
        );
        assert_eq!(
            memory_path(&generated, "project", Some("My_App")).unwrap(),
            format!("projects/my_app/{generated}.md")
        );
        assert_eq!(
            memory_path(&generated, "project", None).unwrap_err(),
            "project kind requires a project slug"
        );
        assert_eq!(
            memory_path("not-a-ulid", "persona", None).unwrap_err(),
            "id is not a valid ULID"
        );
    }

    #[test]
    fn validate_ulid_rejects_malformed_values() {
        assert_eq!(
            validate_ulid("01ARZ3NDEKTSV4RRFFQ69G5FAI").unwrap_err(),
            "id is not a valid ULID"
        );
        for bad in [
            "",
            "01ARZ3NDEKTSV4RRFFQ69G5FAVX",
            "01ARZ3NDEKTSV4RRFFQ69G5FA",
            "81ARZ3NDEKTSV4RRFFQ69G5FAV",
            "01arl3ndektsv4rrffq69g5fav",
            "01LRZ3NDEKTSV4RRFFQ69G5FAV",
            "01ORZ3NDEKTSV4RRFFQ69G5FAV",
            "01URZ3NDEKTSV4RRFFQ69G5FAV",
        ] {
            assert_eq!(
                validate_ulid(bad).unwrap_err(),
                "id is not a valid ULID",
                "{bad:?}"
            );
        }
        assert_eq!(
            validate_ulid("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
    }

    #[test]
    fn ulids_with_the_same_timestamp_are_unique_and_ordered_by_time() {
        let instant = Utc.with_ymd_and_hms(2026, 8, 22, 0, 0, 0).unwrap();
        let mut seen = HashSet::new();
        for _ in 0..256 {
            let id = new_ulid(Some(instant));
            assert_eq!(ulid_timestamp_ms(&id), 1_787_356_800_000);
            assert!(seen.insert(id));
        }
        let earlier = new_ulid(Some(instant - Duration::milliseconds(1)));
        let later = new_ulid(Some(instant + Duration::milliseconds(1)));
        assert!(earlier < later);
        let default_now_a = new_ulid(None);
        let default_now_b = new_ulid(None);
        assert_ne!(default_now_a, default_now_b);
        assert!(validate_ulid(&default_now_a).is_ok());
    }

    #[test]
    #[should_panic(expected = "timestamp cannot be encoded as a ULID")]
    fn ulid_rejects_a_timestamp_before_its_epoch() {
        new_ulid(Some(
            Utc.with_ymd_and_hms(1969, 12, 31, 23, 59, 59).unwrap(),
        ));
    }

    #[test]
    fn revision_requires_a_lowercase_sha256_digest() {
        assert_eq!(validate_revision(&"a".repeat(64)).unwrap(), "a".repeat(64));
        assert_eq!(
            validate_revision(&"0123456789abcdef".repeat(4)).unwrap(),
            "0123456789abcdef".repeat(4)
        );
        for bad in [
            "g".repeat(64),
            "A".repeat(64),
            "a".repeat(63),
            "a".repeat(65),
        ] {
            assert_eq!(
                validate_revision(&bad).unwrap_err(),
                "revision is not a lowercase SHA-256 digest",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn timestamp_round_trip_preserves_microseconds() {
        let now = Utc
            .with_ymd_and_hms(2026, 8, 22, 1, 2, 3)
            .unwrap()
            .with_nanosecond(123_456_000)
            .unwrap();
        let formatted = format_timestamp(now);
        assert_eq!(formatted, "2026-08-22T01:02:03.123456Z");
        assert_eq!(parse_timestamp(&formatted).unwrap(), now);
        let on_the_second = format_timestamp(Utc.with_ymd_and_hms(2026, 8, 22, 1, 2, 3).unwrap());
        assert_eq!(on_the_second, "2026-08-22T01:02:03.000000Z");
        assert_eq!(
            parse_timestamp(&on_the_second).unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 22, 1, 2, 3).unwrap()
        );
    }

    #[test]
    fn timestamps_accept_offsets_and_various_fractions() {
        assert_eq!(
            parse_timestamp("2026-08-22T06:32:03+05:30").unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 22, 1, 2, 3).unwrap()
        );
        assert_eq!(
            parse_timestamp("2026-08-22T01:02:03.5Z").unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 22, 1, 2, 3)
                .unwrap()
                .with_nanosecond(500_000_000)
                .unwrap()
        );
        assert_eq!(
            parse_timestamp("2026-08-22T01:02:03-00:00").unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 22, 1, 2, 3).unwrap()
        );
    }

    #[test]
    fn timestamps_reject_non_rfc3339_values() {
        for bad in [
            "2026-08-22T01:02:03",
            "2026-08-22 01:02:03Z",
            "2026-08-22t01:02:03Z",
            "2026-08-22T01:02:03z",
            "2026-08-22T01:02:03.1234567Z",
            "2026-08-22T01:02:03.Z",
            "2026-08-22",
            "2026-8-22T01:02:03Z",
            "2026-13-01T01:02:03Z",
            "2026-02-30T01:02:03Z",
            "2026-08-22T24:00:00Z",
            "2026-08-22T01:02:03+99:00",
            "2026-08-22T01:02:60Z",
            "0000-01-01T00:00:00Z",
            " 2026-08-22T01:02:03Z",
            "2026-08-22T01:02:03Z ",
        ] {
            assert!(parse_timestamp(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            parse_timestamp("2026-08-22 01:02:03Z").unwrap_err(),
            "timestamp must use RFC 3339 date-time syntax"
        );
    }

    #[test]
    fn next_update_time_stays_strictly_monotonic() {
        let previous = "2026-08-22T01:02:03.000000Z";
        let same = Utc.with_ymd_and_hms(2026, 8, 22, 1, 2, 3).unwrap();
        assert_eq!(
            next_update_time(same, previous),
            same + Duration::microseconds(1)
        );
        let earlier = same - Duration::hours(1);
        assert_eq!(
            next_update_time(earlier, previous),
            parse_timestamp(previous).unwrap() + Duration::microseconds(1)
        );
        let later = same + Duration::microseconds(2);
        assert_eq!(next_update_time(later, previous), later);
    }

    #[test]
    fn snapshot_by_id_indexes_and_lets_duplicates_win_later() {
        let empty = MemorySnapshot {
            commit: None,
            memories: Vec::new(),
        };
        assert!(empty.by_id().is_empty());

        let first = memory("01ARZ3NDEKTSV4RRFFQ69G5FAV", "first");
        let second = memory("01ARZ3NDEKTSV4RRFFQ69G5GAV", "second");
        let snapshot = MemorySnapshot {
            commit: None,
            memories: vec![first.clone(), second],
        };
        let by_id = snapshot.by_id();
        assert_eq!(by_id.len(), 2);
        assert_eq!(by_id["01ARZ3NDEKTSV4RRFFQ69G5FAV"].summary, "first");

        let duplicate = memory("01ARZ3NDEKTSV4RRFFQ69G5FAV", "shadowed");
        let duplicated = MemorySnapshot {
            commit: None,
            memories: vec![first.clone(), duplicate],
        };
        assert_eq!(
            duplicated.by_id()["01ARZ3NDEKTSV4RRFFQ69G5FAV"].summary,
            "shadowed"
        );
    }

    #[test]
    fn utc_now_is_usable_as_a_ulid_timestamp() {
        let id = new_ulid(Some(utc_now()));
        assert!(validate_ulid(&id).is_ok());
    }
}
