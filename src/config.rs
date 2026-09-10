//! Runtime configuration: home resolution and the space registry.
//!
//! Mirrors `python/src/memoro/config.py`: validation rules, constants, and
//! English error messages are ported verbatim (tests assert the wording).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::errors::MemoroError;
use crate::filesystem::atomic_replace;

pub const LOCAL_CONFIG_NAME: &str = "config.json";
pub const DEFAULT_SPACE: &str = "personal";
pub const RESERVED_SPACE_NAMES: &[&str] = &["all"];
pub const MAX_SPACE_NAME_LENGTH: usize = 32;
pub const SPACE_NAME_GUIDANCE: &str = "Use 1-32 lowercase letters, digits, or hyphens, starting and ending with a letter or digit, and avoid reserved names.";

/// `Path.home() / ".memoro"` — the default Memoro home directory.
pub fn default_home() -> PathBuf {
    match env::var_os("HOME") {
        Some(home) => PathBuf::from(home),
        None => home_from_passwd(),
    }
    .join(".memoro")
}

fn home_from_passwd() -> PathBuf {
    passwd_field(PasswdQuery::Uid(current_uid())).unwrap_or_default()
}

enum PasswdQuery {
    Uid(String),
    Name(String),
}

fn passwd_field(query: PasswdQuery) -> Option<PathBuf> {
    let passwd = fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 6 {
            continue;
        }
        let matches = match &query {
            PasswdQuery::Uid(uid) => fields[2] == *uid,
            PasswdQuery::Name(name) => fields[0] == name,
        };
        if matches {
            return Some(PathBuf::from(fields[5]));
        }
    }
    None
}

/// Resolve the Memoro home directory.
///
/// Precedence: the CLI value, then `MEMORO_HOME`, then [`default_home`]. An
/// explicitly empty value (after whitespace trimming) is rejected. `~` and
/// `$VAR`/`${VAR}` expansions use the real process environment, exactly like
/// `os.path.expanduser`/`os.path.expandvars` in Python.
pub fn resolve_home(
    cli_home: Option<&str>,
    environ: Option<&HashMap<String, String>>,
) -> Result<PathBuf, MemoroError> {
    let (raw, origin): (String, &str) = match cli_home {
        Some(value) => (value.to_string(), "--home"),
        None => {
            let from_environment = match environ {
                Some(mapping) => mapping.get("MEMORO_HOME").cloned(),
                None => {
                    env::var_os("MEMORO_HOME").map(|value| value.to_string_lossy().into_owned())
                }
            };
            match from_environment {
                Some(value) => (value, "MEMORO_HOME"),
                None => {
                    let default = default_home().to_string_lossy().into_owned();
                    return Ok(resolve_non_strict(Path::new(&expanduser(&default))));
                }
            }
        }
    };
    let trimmed = python_trim(&raw);
    if trimmed.is_empty() {
        return Err(MemoroError::Configuration(format!(
            "{origin} is empty. Provide a directory for Memoro data."
        )));
    }
    let expanded = expandvars(&expanduser(trimmed));
    Ok(resolve_non_strict(Path::new(&expanded)))
}

/// Settings attached to one entry of the space registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceSettings {
    pub readonly: bool,
}

/// Locations derived from the resolved home directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub home: PathBuf,
}

impl RuntimePaths {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }

    pub fn spaces(&self) -> PathBuf {
        self.home.join("spaces")
    }

    pub fn space(&self, name: &str) -> Result<PathBuf, MemoroError> {
        Ok(self.spaces().join(normalize_space_name(name)?))
    }
}

/// NFKC-normalize, trim, and lowercase a space name, then validate it.
pub fn normalize_space_name(value: &str) -> Result<String, MemoroError> {
    // The NFKC form is computed per character. Whenever the result would be
    // pure ASCII this is exactly `unicodedata.normalize("NFKC", value)`
    // (canonical composition never produces ASCII, so the NFKD expansion and
    // the NFKC form coincide there); otherwise at least one character is
    // non-ASCII either way, which only ever leads to the invalid-name error,
    // whose message quotes the original value.
    let expanded = nfkc_ascii(value);
    let normalized = python_trim(&expanded).to_lowercase();
    if normalized.is_empty() {
        return Err(MemoroError::Configuration(format!(
            "Space name is empty. {SPACE_NAME_GUIDANCE}"
        )));
    }
    if !is_valid_space_name(&normalized) {
        return Err(MemoroError::Configuration(format!(
            "Space name {} is invalid. {SPACE_NAME_GUIDANCE}",
            py_repr(value)
        )));
    }
    if RESERVED_SPACE_NAMES.contains(&normalized.as_str()) {
        return Err(MemoroError::Configuration(format!(
            "Space name {} is reserved. Choose a different name.",
            py_repr(&normalized)
        )));
    }
    Ok(normalized)
}

fn is_valid_space_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_SPACE_NAME_LENGTH {
        return false;
    }
    let is_alnum = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if !is_alnum(bytes[0]) || !is_alnum(bytes[bytes.len() - 1]) {
        return false;
    }
    bytes
        .get(1..bytes.len() - 1)
        .is_none_or(|middle| middle.iter().all(|&byte| is_alnum(byte) || byte == b'-'))
}

/// Read the space registry, with the default space always present.
pub fn load_spaces(home: &Path) -> Result<BTreeMap<String, SpaceSettings>, MemoroError> {
    let path = home.join(LOCAL_CONFIG_NAME);
    if !path.exists() {
        return Ok(single_default_space());
    }
    let unreadable = || {
        MemoroError::Configuration(format!(
            "Memoro local configuration {} is invalid or unreadable. Repair or remove that file, \
             then retry.",
            path.display()
        ))
    };
    let text = fs::read_to_string(&path).map_err(|_| unreadable())?;
    let payload: serde_json::Value = serde_json::from_str(&text).map_err(|_| unreadable())?;
    let Some(object) = payload.as_object() else {
        return Err(invalid_shape(&path));
    };
    if object.len() != 1 || !object.contains_key("spaces") {
        return Err(invalid_shape(&path));
    }
    let Some(raw_spaces) = object.get("spaces").and_then(|value| value.as_object()) else {
        return Err(MemoroError::Configuration(format!(
            "Memoro local configuration {} has an invalid spaces value. It must map space names \
             to their settings. Repair or remove that file, then retry.",
            path.display()
        )));
    };
    let mut spaces: BTreeMap<String, SpaceSettings> = BTreeMap::new();
    for (name, entry) in raw_spaces {
        let canonical = normalize_space_name(name)?;
        if canonical != *name {
            return Err(MemoroError::Configuration(format!(
                "Memoro local configuration {} has a space name {} that is not normalized. \
                 {SPACE_NAME_GUIDANCE}",
                path.display(),
                py_repr(name)
            )));
        }
        let readonly = match entry {
            serde_json::Value::Object(fields) if fields.len() == 1 => {
                match fields.get("readonly") {
                    Some(serde_json::Value::Bool(flag)) => *flag,
                    _ => return Err(invalid_settings(&path, name)),
                }
            }
            _ => return Err(invalid_settings(&path, name)),
        };
        spaces.insert(name.clone(), SpaceSettings { readonly });
    }
    if spaces
        .get(DEFAULT_SPACE)
        .map(|settings| settings.readonly)
        .unwrap_or(false)
    {
        return Err(MemoroError::Configuration(format!(
            "Memoro local configuration {} marks the default space {} as readonly. The default \
             space is always writable; remove that entry or set readonly to false, then retry.",
            path.display(),
            py_repr(DEFAULT_SPACE)
        )));
    }
    spaces
        .entry(DEFAULT_SPACE.to_string())
        .or_insert(SpaceSettings { readonly: false });
    Ok(spaces)
}

fn invalid_shape(path: &Path) -> MemoroError {
    MemoroError::Configuration(format!(
        "Memoro local configuration {} must contain exactly the spaces field. Repair or remove \
         that file, then retry.",
        path.display()
    ))
}

fn invalid_settings(path: &Path, name: &str) -> MemoroError {
    MemoroError::Configuration(format!(
        "Memoro local configuration {} has invalid settings for space {}. Each space must \
         contain exactly a boolean readonly field.",
        path.display(),
        py_repr(name)
    ))
}

/// Validate the registry and persist it as canonical JSON, atomically.
pub fn save_spaces(
    home: &Path,
    spaces: &BTreeMap<String, SpaceSettings>,
) -> Result<PathBuf, MemoroError> {
    let mut normalized: BTreeMap<String, SpaceSettings> = BTreeMap::new();
    for (name, settings) in spaces {
        let canonical = normalize_space_name(name)?;
        if canonical != *name {
            return Err(MemoroError::Configuration(format!(
                "Space name {} is not normalized. Use {} instead.",
                py_repr(name),
                py_repr(&canonical)
            )));
        }
        normalized.insert(name.clone(), settings.clone());
    }
    if normalized
        .get(DEFAULT_SPACE)
        .map(|settings| settings.readonly)
        .unwrap_or(false)
    {
        return Err(MemoroError::Configuration(format!(
            "The default space {} is always writable. Set readonly to false before saving the \
             space registry.",
            py_repr(DEFAULT_SPACE)
        )));
    }
    let path = home.join(LOCAL_CONFIG_NAME);
    let entries: serde_json::Map<String, serde_json::Value> = normalized
        .iter()
        .map(|(name, settings)| {
            let mut fields = serde_json::Map::new();
            fields.insert(
                "readonly".to_string(),
                serde_json::Value::Bool(settings.readonly),
            );
            (name.clone(), serde_json::Value::Object(fields))
        })
        .collect();
    let mut payload = serde_json::Map::new();
    payload.insert("spaces".to_string(), serde_json::Value::Object(entries));
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(payload))
        .map_err(|_| unsavable(&path))?;
    let mut text = text;
    text.push('\n');
    atomic_replace(&path, text.as_bytes()).map_err(|_| unsavable(&path))?;
    Ok(path)
}

fn unsavable(path: &Path) -> MemoroError {
    MemoroError::Configuration(format!(
        "Memoro could not save the space registry to {}. Check the directory permissions, then \
         retry.",
        path.display()
    ))
}

fn single_default_space() -> BTreeMap<String, SpaceSettings> {
    BTreeMap::from([(DEFAULT_SPACE.to_string(), SpaceSettings { readonly: false })])
}

// ---------------------------------------------------------------------------
// Python compatibility helpers
// ---------------------------------------------------------------------------

/// Python `str.strip()` whitespace set (`str.isspace`).
fn is_python_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{09}'..='\u{0d}'
            | '\u{1c}'..='\u{1f}'
            | ' '
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

fn python_trim(value: &str) -> &str {
    value.trim_matches(is_python_whitespace)
}

/// Python `repr()` for `str` values (quote selection, named escapes, and
/// `\x`/`\u`/`\U` escapes for non-printable characters).
fn py_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character == quote => {
                out.push('\\');
                out.push(character);
            }
            character if is_python_printable(character) => out.push(character),
            character if (character as u32) < 0x100 => {
                out.push_str(&format!("\\x{:02x}", character as u32));
            }
            character if (character as u32) < 0x10000 => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push_str(&format!("\\U{:08x}", character as u32)),
        }
    }
    out.push(quote);
    out
}

/// Python `str.isprintable()`, exactly (see [`NON_PRINTABLE_RUNS`]).
fn is_python_printable(character: char) -> bool {
    let codepoint = character as u32;
    let index = NON_PRINTABLE_RUNS.partition_point(|&(start, _)| start <= codepoint);
    index == 0 || NON_PRINTABLE_RUNS[index - 1].1 < codepoint
}

/// Maximal runs of codepoints for which `str.isprintable()` is false (Cc, Cf,
/// Cs, Co, Cn, and the Zs/Zl/Zp separators). Generated from
/// `unicodedata` (Unicode 15.1.0).
static NON_PRINTABLE_RUNS: &[(u32, u32)] = &[
    (0x000000, 0x00001f),
    (0x00007f, 0x0000a0),
    (0x0000ad, 0x0000ad),
    (0x000378, 0x000379),
    (0x000380, 0x000383),
    (0x00038b, 0x00038b),
    (0x00038d, 0x00038d),
    (0x0003a2, 0x0003a2),
    (0x000530, 0x000530),
    (0x000557, 0x000558),
    (0x00058b, 0x00058c),
    (0x000590, 0x000590),
    (0x0005c8, 0x0005cf),
    (0x0005eb, 0x0005ee),
    (0x0005f5, 0x000605),
    (0x00061c, 0x00061c),
    (0x0006dd, 0x0006dd),
    (0x00070e, 0x00070f),
    (0x00074b, 0x00074c),
    (0x0007b2, 0x0007bf),
    (0x0007fb, 0x0007fc),
    (0x00082e, 0x00082f),
    (0x00083f, 0x00083f),
    (0x00085c, 0x00085d),
    (0x00085f, 0x00085f),
    (0x00086b, 0x00086f),
    (0x00088f, 0x000897),
    (0x0008e2, 0x0008e2),
    (0x000984, 0x000984),
    (0x00098d, 0x00098e),
    (0x000991, 0x000992),
    (0x0009a9, 0x0009a9),
    (0x0009b1, 0x0009b1),
    (0x0009b3, 0x0009b5),
    (0x0009ba, 0x0009bb),
    (0x0009c5, 0x0009c6),
    (0x0009c9, 0x0009ca),
    (0x0009cf, 0x0009d6),
    (0x0009d8, 0x0009db),
    (0x0009de, 0x0009de),
    (0x0009e4, 0x0009e5),
    (0x0009ff, 0x000a00),
    (0x000a04, 0x000a04),
    (0x000a0b, 0x000a0e),
    (0x000a11, 0x000a12),
    (0x000a29, 0x000a29),
    (0x000a31, 0x000a31),
    (0x000a34, 0x000a34),
    (0x000a37, 0x000a37),
    (0x000a3a, 0x000a3b),
    (0x000a3d, 0x000a3d),
    (0x000a43, 0x000a46),
    (0x000a49, 0x000a4a),
    (0x000a4e, 0x000a50),
    (0x000a52, 0x000a58),
    (0x000a5d, 0x000a5d),
    (0x000a5f, 0x000a65),
    (0x000a77, 0x000a80),
    (0x000a84, 0x000a84),
    (0x000a8e, 0x000a8e),
    (0x000a92, 0x000a92),
    (0x000aa9, 0x000aa9),
    (0x000ab1, 0x000ab1),
    (0x000ab4, 0x000ab4),
    (0x000aba, 0x000abb),
    (0x000ac6, 0x000ac6),
    (0x000aca, 0x000aca),
    (0x000ace, 0x000acf),
    (0x000ad1, 0x000adf),
    (0x000ae4, 0x000ae5),
    (0x000af2, 0x000af8),
    (0x000b00, 0x000b00),
    (0x000b04, 0x000b04),
    (0x000b0d, 0x000b0e),
    (0x000b11, 0x000b12),
    (0x000b29, 0x000b29),
    (0x000b31, 0x000b31),
    (0x000b34, 0x000b34),
    (0x000b3a, 0x000b3b),
    (0x000b45, 0x000b46),
    (0x000b49, 0x000b4a),
    (0x000b4e, 0x000b54),
    (0x000b58, 0x000b5b),
    (0x000b5e, 0x000b5e),
    (0x000b64, 0x000b65),
    (0x000b78, 0x000b81),
    (0x000b84, 0x000b84),
    (0x000b8b, 0x000b8d),
    (0x000b91, 0x000b91),
    (0x000b96, 0x000b98),
    (0x000b9b, 0x000b9b),
    (0x000b9d, 0x000b9d),
    (0x000ba0, 0x000ba2),
    (0x000ba5, 0x000ba7),
    (0x000bab, 0x000bad),
    (0x000bba, 0x000bbd),
    (0x000bc3, 0x000bc5),
    (0x000bc9, 0x000bc9),
    (0x000bce, 0x000bcf),
    (0x000bd1, 0x000bd6),
    (0x000bd8, 0x000be5),
    (0x000bfb, 0x000bff),
    (0x000c0d, 0x000c0d),
    (0x000c11, 0x000c11),
    (0x000c29, 0x000c29),
    (0x000c3a, 0x000c3b),
    (0x000c45, 0x000c45),
    (0x000c49, 0x000c49),
    (0x000c4e, 0x000c54),
    (0x000c57, 0x000c57),
    (0x000c5b, 0x000c5c),
    (0x000c5e, 0x000c5f),
    (0x000c64, 0x000c65),
    (0x000c70, 0x000c76),
    (0x000c8d, 0x000c8d),
    (0x000c91, 0x000c91),
    (0x000ca9, 0x000ca9),
    (0x000cb4, 0x000cb4),
    (0x000cba, 0x000cbb),
    (0x000cc5, 0x000cc5),
    (0x000cc9, 0x000cc9),
    (0x000cce, 0x000cd4),
    (0x000cd7, 0x000cdc),
    (0x000cdf, 0x000cdf),
    (0x000ce4, 0x000ce5),
    (0x000cf0, 0x000cf0),
    (0x000cf4, 0x000cff),
    (0x000d0d, 0x000d0d),
    (0x000d11, 0x000d11),
    (0x000d45, 0x000d45),
    (0x000d49, 0x000d49),
    (0x000d50, 0x000d53),
    (0x000d64, 0x000d65),
    (0x000d80, 0x000d80),
    (0x000d84, 0x000d84),
    (0x000d97, 0x000d99),
    (0x000db2, 0x000db2),
    (0x000dbc, 0x000dbc),
    (0x000dbe, 0x000dbf),
    (0x000dc7, 0x000dc9),
    (0x000dcb, 0x000dce),
    (0x000dd5, 0x000dd5),
    (0x000dd7, 0x000dd7),
    (0x000de0, 0x000de5),
    (0x000df0, 0x000df1),
    (0x000df5, 0x000e00),
    (0x000e3b, 0x000e3e),
    (0x000e5c, 0x000e80),
    (0x000e83, 0x000e83),
    (0x000e85, 0x000e85),
    (0x000e8b, 0x000e8b),
    (0x000ea4, 0x000ea4),
    (0x000ea6, 0x000ea6),
    (0x000ebe, 0x000ebf),
    (0x000ec5, 0x000ec5),
    (0x000ec7, 0x000ec7),
    (0x000ecf, 0x000ecf),
    (0x000eda, 0x000edb),
    (0x000ee0, 0x000eff),
    (0x000f48, 0x000f48),
    (0x000f6d, 0x000f70),
    (0x000f98, 0x000f98),
    (0x000fbd, 0x000fbd),
    (0x000fcd, 0x000fcd),
    (0x000fdb, 0x000fff),
    (0x0010c6, 0x0010c6),
    (0x0010c8, 0x0010cc),
    (0x0010ce, 0x0010cf),
    (0x001249, 0x001249),
    (0x00124e, 0x00124f),
    (0x001257, 0x001257),
    (0x001259, 0x001259),
    (0x00125e, 0x00125f),
    (0x001289, 0x001289),
    (0x00128e, 0x00128f),
    (0x0012b1, 0x0012b1),
    (0x0012b6, 0x0012b7),
    (0x0012bf, 0x0012bf),
    (0x0012c1, 0x0012c1),
    (0x0012c6, 0x0012c7),
    (0x0012d7, 0x0012d7),
    (0x001311, 0x001311),
    (0x001316, 0x001317),
    (0x00135b, 0x00135c),
    (0x00137d, 0x00137f),
    (0x00139a, 0x00139f),
    (0x0013f6, 0x0013f7),
    (0x0013fe, 0x0013ff),
    (0x001680, 0x001680),
    (0x00169d, 0x00169f),
    (0x0016f9, 0x0016ff),
    (0x001716, 0x00171e),
    (0x001737, 0x00173f),
    (0x001754, 0x00175f),
    (0x00176d, 0x00176d),
    (0x001771, 0x001771),
    (0x001774, 0x00177f),
    (0x0017de, 0x0017df),
    (0x0017ea, 0x0017ef),
    (0x0017fa, 0x0017ff),
    (0x00180e, 0x00180e),
    (0x00181a, 0x00181f),
    (0x001879, 0x00187f),
    (0x0018ab, 0x0018af),
    (0x0018f6, 0x0018ff),
    (0x00191f, 0x00191f),
    (0x00192c, 0x00192f),
    (0x00193c, 0x00193f),
    (0x001941, 0x001943),
    (0x00196e, 0x00196f),
    (0x001975, 0x00197f),
    (0x0019ac, 0x0019af),
    (0x0019ca, 0x0019cf),
    (0x0019db, 0x0019dd),
    (0x001a1c, 0x001a1d),
    (0x001a5f, 0x001a5f),
    (0x001a7d, 0x001a7e),
    (0x001a8a, 0x001a8f),
    (0x001a9a, 0x001a9f),
    (0x001aae, 0x001aaf),
    (0x001acf, 0x001aff),
    (0x001b4d, 0x001b4f),
    (0x001b7f, 0x001b7f),
    (0x001bf4, 0x001bfb),
    (0x001c38, 0x001c3a),
    (0x001c4a, 0x001c4c),
    (0x001c89, 0x001c8f),
    (0x001cbb, 0x001cbc),
    (0x001cc8, 0x001ccf),
    (0x001cfb, 0x001cff),
    (0x001f16, 0x001f17),
    (0x001f1e, 0x001f1f),
    (0x001f46, 0x001f47),
    (0x001f4e, 0x001f4f),
    (0x001f58, 0x001f58),
    (0x001f5a, 0x001f5a),
    (0x001f5c, 0x001f5c),
    (0x001f5e, 0x001f5e),
    (0x001f7e, 0x001f7f),
    (0x001fb5, 0x001fb5),
    (0x001fc5, 0x001fc5),
    (0x001fd4, 0x001fd5),
    (0x001fdc, 0x001fdc),
    (0x001ff0, 0x001ff1),
    (0x001ff5, 0x001ff5),
    (0x001fff, 0x00200f),
    (0x002028, 0x00202f),
    (0x00205f, 0x00206f),
    (0x002072, 0x002073),
    (0x00208f, 0x00208f),
    (0x00209d, 0x00209f),
    (0x0020c1, 0x0020cf),
    (0x0020f1, 0x0020ff),
    (0x00218c, 0x00218f),
    (0x002427, 0x00243f),
    (0x00244b, 0x00245f),
    (0x002b74, 0x002b75),
    (0x002b96, 0x002b96),
    (0x002cf4, 0x002cf8),
    (0x002d26, 0x002d26),
    (0x002d28, 0x002d2c),
    (0x002d2e, 0x002d2f),
    (0x002d68, 0x002d6e),
    (0x002d71, 0x002d7e),
    (0x002d97, 0x002d9f),
    (0x002da7, 0x002da7),
    (0x002daf, 0x002daf),
    (0x002db7, 0x002db7),
    (0x002dbf, 0x002dbf),
    (0x002dc7, 0x002dc7),
    (0x002dcf, 0x002dcf),
    (0x002dd7, 0x002dd7),
    (0x002ddf, 0x002ddf),
    (0x002e5e, 0x002e7f),
    (0x002e9a, 0x002e9a),
    (0x002ef4, 0x002eff),
    (0x002fd6, 0x002fef),
    (0x003000, 0x003000),
    (0x003040, 0x003040),
    (0x003097, 0x003098),
    (0x003100, 0x003104),
    (0x003130, 0x003130),
    (0x00318f, 0x00318f),
    (0x0031e4, 0x0031ee),
    (0x00321f, 0x00321f),
    (0x00a48d, 0x00a48f),
    (0x00a4c7, 0x00a4cf),
    (0x00a62c, 0x00a63f),
    (0x00a6f8, 0x00a6ff),
    (0x00a7cb, 0x00a7cf),
    (0x00a7d2, 0x00a7d2),
    (0x00a7d4, 0x00a7d4),
    (0x00a7da, 0x00a7f1),
    (0x00a82d, 0x00a82f),
    (0x00a83a, 0x00a83f),
    (0x00a878, 0x00a87f),
    (0x00a8c6, 0x00a8cd),
    (0x00a8da, 0x00a8df),
    (0x00a954, 0x00a95e),
    (0x00a97d, 0x00a97f),
    (0x00a9ce, 0x00a9ce),
    (0x00a9da, 0x00a9dd),
    (0x00a9ff, 0x00a9ff),
    (0x00aa37, 0x00aa3f),
    (0x00aa4e, 0x00aa4f),
    (0x00aa5a, 0x00aa5b),
    (0x00aac3, 0x00aada),
    (0x00aaf7, 0x00ab00),
    (0x00ab07, 0x00ab08),
    (0x00ab0f, 0x00ab10),
    (0x00ab17, 0x00ab1f),
    (0x00ab27, 0x00ab27),
    (0x00ab2f, 0x00ab2f),
    (0x00ab6c, 0x00ab6f),
    (0x00abee, 0x00abef),
    (0x00abfa, 0x00abff),
    (0x00d7a4, 0x00d7af),
    (0x00d7c7, 0x00d7ca),
    (0x00d7fc, 0x00f8ff),
    (0x00fa6e, 0x00fa6f),
    (0x00fada, 0x00faff),
    (0x00fb07, 0x00fb12),
    (0x00fb18, 0x00fb1c),
    (0x00fb37, 0x00fb37),
    (0x00fb3d, 0x00fb3d),
    (0x00fb3f, 0x00fb3f),
    (0x00fb42, 0x00fb42),
    (0x00fb45, 0x00fb45),
    (0x00fbc3, 0x00fbd2),
    (0x00fd90, 0x00fd91),
    (0x00fdc8, 0x00fdce),
    (0x00fdd0, 0x00fdef),
    (0x00fe1a, 0x00fe1f),
    (0x00fe53, 0x00fe53),
    (0x00fe67, 0x00fe67),
    (0x00fe6c, 0x00fe6f),
    (0x00fe75, 0x00fe75),
    (0x00fefd, 0x00ff00),
    (0x00ffbf, 0x00ffc1),
    (0x00ffc8, 0x00ffc9),
    (0x00ffd0, 0x00ffd1),
    (0x00ffd8, 0x00ffd9),
    (0x00ffdd, 0x00ffdf),
    (0x00ffe7, 0x00ffe7),
    (0x00ffef, 0x00fffb),
    (0x00fffe, 0x00ffff),
    (0x01000c, 0x01000c),
    (0x010027, 0x010027),
    (0x01003b, 0x01003b),
    (0x01003e, 0x01003e),
    (0x01004e, 0x01004f),
    (0x01005e, 0x01007f),
    (0x0100fb, 0x0100ff),
    (0x010103, 0x010106),
    (0x010134, 0x010136),
    (0x01018f, 0x01018f),
    (0x01019d, 0x01019f),
    (0x0101a1, 0x0101cf),
    (0x0101fe, 0x01027f),
    (0x01029d, 0x01029f),
    (0x0102d1, 0x0102df),
    (0x0102fc, 0x0102ff),
    (0x010324, 0x01032c),
    (0x01034b, 0x01034f),
    (0x01037b, 0x01037f),
    (0x01039e, 0x01039e),
    (0x0103c4, 0x0103c7),
    (0x0103d6, 0x0103ff),
    (0x01049e, 0x01049f),
    (0x0104aa, 0x0104af),
    (0x0104d4, 0x0104d7),
    (0x0104fc, 0x0104ff),
    (0x010528, 0x01052f),
    (0x010564, 0x01056e),
    (0x01057b, 0x01057b),
    (0x01058b, 0x01058b),
    (0x010593, 0x010593),
    (0x010596, 0x010596),
    (0x0105a2, 0x0105a2),
    (0x0105b2, 0x0105b2),
    (0x0105ba, 0x0105ba),
    (0x0105bd, 0x0105ff),
    (0x010737, 0x01073f),
    (0x010756, 0x01075f),
    (0x010768, 0x01077f),
    (0x010786, 0x010786),
    (0x0107b1, 0x0107b1),
    (0x0107bb, 0x0107ff),
    (0x010806, 0x010807),
    (0x010809, 0x010809),
    (0x010836, 0x010836),
    (0x010839, 0x01083b),
    (0x01083d, 0x01083e),
    (0x010856, 0x010856),
    (0x01089f, 0x0108a6),
    (0x0108b0, 0x0108df),
    (0x0108f3, 0x0108f3),
    (0x0108f6, 0x0108fa),
    (0x01091c, 0x01091e),
    (0x01093a, 0x01093e),
    (0x010940, 0x01097f),
    (0x0109b8, 0x0109bb),
    (0x0109d0, 0x0109d1),
    (0x010a04, 0x010a04),
    (0x010a07, 0x010a0b),
    (0x010a14, 0x010a14),
    (0x010a18, 0x010a18),
    (0x010a36, 0x010a37),
    (0x010a3b, 0x010a3e),
    (0x010a49, 0x010a4f),
    (0x010a59, 0x010a5f),
    (0x010aa0, 0x010abf),
    (0x010ae7, 0x010aea),
    (0x010af7, 0x010aff),
    (0x010b36, 0x010b38),
    (0x010b56, 0x010b57),
    (0x010b73, 0x010b77),
    (0x010b92, 0x010b98),
    (0x010b9d, 0x010ba8),
    (0x010bb0, 0x010bff),
    (0x010c49, 0x010c7f),
    (0x010cb3, 0x010cbf),
    (0x010cf3, 0x010cf9),
    (0x010d28, 0x010d2f),
    (0x010d3a, 0x010e5f),
    (0x010e7f, 0x010e7f),
    (0x010eaa, 0x010eaa),
    (0x010eae, 0x010eaf),
    (0x010eb2, 0x010efc),
    (0x010f28, 0x010f2f),
    (0x010f5a, 0x010f6f),
    (0x010f8a, 0x010faf),
    (0x010fcc, 0x010fdf),
    (0x010ff7, 0x010fff),
    (0x01104e, 0x011051),
    (0x011076, 0x01107e),
    (0x0110bd, 0x0110bd),
    (0x0110c3, 0x0110cf),
    (0x0110e9, 0x0110ef),
    (0x0110fa, 0x0110ff),
    (0x011135, 0x011135),
    (0x011148, 0x01114f),
    (0x011177, 0x01117f),
    (0x0111e0, 0x0111e0),
    (0x0111f5, 0x0111ff),
    (0x011212, 0x011212),
    (0x011242, 0x01127f),
    (0x011287, 0x011287),
    (0x011289, 0x011289),
    (0x01128e, 0x01128e),
    (0x01129e, 0x01129e),
    (0x0112aa, 0x0112af),
    (0x0112eb, 0x0112ef),
    (0x0112fa, 0x0112ff),
    (0x011304, 0x011304),
    (0x01130d, 0x01130e),
    (0x011311, 0x011312),
    (0x011329, 0x011329),
    (0x011331, 0x011331),
    (0x011334, 0x011334),
    (0x01133a, 0x01133a),
    (0x011345, 0x011346),
    (0x011349, 0x01134a),
    (0x01134e, 0x01134f),
    (0x011351, 0x011356),
    (0x011358, 0x01135c),
    (0x011364, 0x011365),
    (0x01136d, 0x01136f),
    (0x011375, 0x0113ff),
    (0x01145c, 0x01145c),
    (0x011462, 0x01147f),
    (0x0114c8, 0x0114cf),
    (0x0114da, 0x01157f),
    (0x0115b6, 0x0115b7),
    (0x0115de, 0x0115ff),
    (0x011645, 0x01164f),
    (0x01165a, 0x01165f),
    (0x01166d, 0x01167f),
    (0x0116ba, 0x0116bf),
    (0x0116ca, 0x0116ff),
    (0x01171b, 0x01171c),
    (0x01172c, 0x01172f),
    (0x011747, 0x0117ff),
    (0x01183c, 0x01189f),
    (0x0118f3, 0x0118fe),
    (0x011907, 0x011908),
    (0x01190a, 0x01190b),
    (0x011914, 0x011914),
    (0x011917, 0x011917),
    (0x011936, 0x011936),
    (0x011939, 0x01193a),
    (0x011947, 0x01194f),
    (0x01195a, 0x01199f),
    (0x0119a8, 0x0119a9),
    (0x0119d8, 0x0119d9),
    (0x0119e5, 0x0119ff),
    (0x011a48, 0x011a4f),
    (0x011aa3, 0x011aaf),
    (0x011af9, 0x011aff),
    (0x011b0a, 0x011bff),
    (0x011c09, 0x011c09),
    (0x011c37, 0x011c37),
    (0x011c46, 0x011c4f),
    (0x011c6d, 0x011c6f),
    (0x011c90, 0x011c91),
    (0x011ca8, 0x011ca8),
    (0x011cb7, 0x011cff),
    (0x011d07, 0x011d07),
    (0x011d0a, 0x011d0a),
    (0x011d37, 0x011d39),
    (0x011d3b, 0x011d3b),
    (0x011d3e, 0x011d3e),
    (0x011d48, 0x011d4f),
    (0x011d5a, 0x011d5f),
    (0x011d66, 0x011d66),
    (0x011d69, 0x011d69),
    (0x011d8f, 0x011d8f),
    (0x011d92, 0x011d92),
    (0x011d99, 0x011d9f),
    (0x011daa, 0x011edf),
    (0x011ef9, 0x011eff),
    (0x011f11, 0x011f11),
    (0x011f3b, 0x011f3d),
    (0x011f5a, 0x011faf),
    (0x011fb1, 0x011fbf),
    (0x011ff2, 0x011ffe),
    (0x01239a, 0x0123ff),
    (0x01246f, 0x01246f),
    (0x012475, 0x01247f),
    (0x012544, 0x012f8f),
    (0x012ff3, 0x012fff),
    (0x013430, 0x01343f),
    (0x013456, 0x0143ff),
    (0x014647, 0x0167ff),
    (0x016a39, 0x016a3f),
    (0x016a5f, 0x016a5f),
    (0x016a6a, 0x016a6d),
    (0x016abf, 0x016abf),
    (0x016aca, 0x016acf),
    (0x016aee, 0x016aef),
    (0x016af6, 0x016aff),
    (0x016b46, 0x016b4f),
    (0x016b5a, 0x016b5a),
    (0x016b62, 0x016b62),
    (0x016b78, 0x016b7c),
    (0x016b90, 0x016e3f),
    (0x016e9b, 0x016eff),
    (0x016f4b, 0x016f4e),
    (0x016f88, 0x016f8e),
    (0x016fa0, 0x016fdf),
    (0x016fe5, 0x016fef),
    (0x016ff2, 0x016fff),
    (0x0187f8, 0x0187ff),
    (0x018cd6, 0x018cff),
    (0x018d09, 0x01afef),
    (0x01aff4, 0x01aff4),
    (0x01affc, 0x01affc),
    (0x01afff, 0x01afff),
    (0x01b123, 0x01b131),
    (0x01b133, 0x01b14f),
    (0x01b153, 0x01b154),
    (0x01b156, 0x01b163),
    (0x01b168, 0x01b16f),
    (0x01b2fc, 0x01bbff),
    (0x01bc6b, 0x01bc6f),
    (0x01bc7d, 0x01bc7f),
    (0x01bc89, 0x01bc8f),
    (0x01bc9a, 0x01bc9b),
    (0x01bca0, 0x01ceff),
    (0x01cf2e, 0x01cf2f),
    (0x01cf47, 0x01cf4f),
    (0x01cfc4, 0x01cfff),
    (0x01d0f6, 0x01d0ff),
    (0x01d127, 0x01d128),
    (0x01d173, 0x01d17a),
    (0x01d1eb, 0x01d1ff),
    (0x01d246, 0x01d2bf),
    (0x01d2d4, 0x01d2df),
    (0x01d2f4, 0x01d2ff),
    (0x01d357, 0x01d35f),
    (0x01d379, 0x01d3ff),
    (0x01d455, 0x01d455),
    (0x01d49d, 0x01d49d),
    (0x01d4a0, 0x01d4a1),
    (0x01d4a3, 0x01d4a4),
    (0x01d4a7, 0x01d4a8),
    (0x01d4ad, 0x01d4ad),
    (0x01d4ba, 0x01d4ba),
    (0x01d4bc, 0x01d4bc),
    (0x01d4c4, 0x01d4c4),
    (0x01d506, 0x01d506),
    (0x01d50b, 0x01d50c),
    (0x01d515, 0x01d515),
    (0x01d51d, 0x01d51d),
    (0x01d53a, 0x01d53a),
    (0x01d53f, 0x01d53f),
    (0x01d545, 0x01d545),
    (0x01d547, 0x01d549),
    (0x01d551, 0x01d551),
    (0x01d6a6, 0x01d6a7),
    (0x01d7cc, 0x01d7cd),
    (0x01da8c, 0x01da9a),
    (0x01daa0, 0x01daa0),
    (0x01dab0, 0x01deff),
    (0x01df1f, 0x01df24),
    (0x01df2b, 0x01dfff),
    (0x01e007, 0x01e007),
    (0x01e019, 0x01e01a),
    (0x01e022, 0x01e022),
    (0x01e025, 0x01e025),
    (0x01e02b, 0x01e02f),
    (0x01e06e, 0x01e08e),
    (0x01e090, 0x01e0ff),
    (0x01e12d, 0x01e12f),
    (0x01e13e, 0x01e13f),
    (0x01e14a, 0x01e14d),
    (0x01e150, 0x01e28f),
    (0x01e2af, 0x01e2bf),
    (0x01e2fa, 0x01e2fe),
    (0x01e300, 0x01e4cf),
    (0x01e4fa, 0x01e7df),
    (0x01e7e7, 0x01e7e7),
    (0x01e7ec, 0x01e7ec),
    (0x01e7ef, 0x01e7ef),
    (0x01e7ff, 0x01e7ff),
    (0x01e8c5, 0x01e8c6),
    (0x01e8d7, 0x01e8ff),
    (0x01e94c, 0x01e94f),
    (0x01e95a, 0x01e95d),
    (0x01e960, 0x01ec70),
    (0x01ecb5, 0x01ed00),
    (0x01ed3e, 0x01edff),
    (0x01ee04, 0x01ee04),
    (0x01ee20, 0x01ee20),
    (0x01ee23, 0x01ee23),
    (0x01ee25, 0x01ee26),
    (0x01ee28, 0x01ee28),
    (0x01ee33, 0x01ee33),
    (0x01ee38, 0x01ee38),
    (0x01ee3a, 0x01ee3a),
    (0x01ee3c, 0x01ee41),
    (0x01ee43, 0x01ee46),
    (0x01ee48, 0x01ee48),
    (0x01ee4a, 0x01ee4a),
    (0x01ee4c, 0x01ee4c),
    (0x01ee50, 0x01ee50),
    (0x01ee53, 0x01ee53),
    (0x01ee55, 0x01ee56),
    (0x01ee58, 0x01ee58),
    (0x01ee5a, 0x01ee5a),
    (0x01ee5c, 0x01ee5c),
    (0x01ee5e, 0x01ee5e),
    (0x01ee60, 0x01ee60),
    (0x01ee63, 0x01ee63),
    (0x01ee65, 0x01ee66),
    (0x01ee6b, 0x01ee6b),
    (0x01ee73, 0x01ee73),
    (0x01ee78, 0x01ee78),
    (0x01ee7d, 0x01ee7d),
    (0x01ee7f, 0x01ee7f),
    (0x01ee8a, 0x01ee8a),
    (0x01ee9c, 0x01eea0),
    (0x01eea4, 0x01eea4),
    (0x01eeaa, 0x01eeaa),
    (0x01eebc, 0x01eeef),
    (0x01eef2, 0x01efff),
    (0x01f02c, 0x01f02f),
    (0x01f094, 0x01f09f),
    (0x01f0af, 0x01f0b0),
    (0x01f0c0, 0x01f0c0),
    (0x01f0d0, 0x01f0d0),
    (0x01f0f6, 0x01f0ff),
    (0x01f1ae, 0x01f1e5),
    (0x01f203, 0x01f20f),
    (0x01f23c, 0x01f23f),
    (0x01f249, 0x01f24f),
    (0x01f252, 0x01f25f),
    (0x01f266, 0x01f2ff),
    (0x01f6d8, 0x01f6db),
    (0x01f6ed, 0x01f6ef),
    (0x01f6fd, 0x01f6ff),
    (0x01f777, 0x01f77a),
    (0x01f7da, 0x01f7df),
    (0x01f7ec, 0x01f7ef),
    (0x01f7f1, 0x01f7ff),
    (0x01f80c, 0x01f80f),
    (0x01f848, 0x01f84f),
    (0x01f85a, 0x01f85f),
    (0x01f888, 0x01f88f),
    (0x01f8ae, 0x01f8af),
    (0x01f8b2, 0x01f8ff),
    (0x01fa54, 0x01fa5f),
    (0x01fa6e, 0x01fa6f),
    (0x01fa7d, 0x01fa7f),
    (0x01fa89, 0x01fa8f),
    (0x01fabe, 0x01fabe),
    (0x01fac6, 0x01facd),
    (0x01fadc, 0x01fadf),
    (0x01fae9, 0x01faef),
    (0x01faf9, 0x01faff),
    (0x01fb93, 0x01fb93),
    (0x01fbcb, 0x01fbef),
    (0x01fbfa, 0x01ffff),
    (0x02a6e0, 0x02a6ff),
    (0x02b73a, 0x02b73f),
    (0x02b81e, 0x02b81f),
    (0x02cea2, 0x02ceaf),
    (0x02ebe1, 0x02ebef),
    (0x02ee5e, 0x02f7ff),
    (0x02fa1e, 0x02ffff),
    (0x03134b, 0x03134f),
    (0x0323b0, 0x0e00ff),
    (0x0e01f0, 0x10ffff),
];

/// Per-character NFKC: ASCII passes through, non-ASCII characters with a
/// pure-ASCII NFKD expansion are replaced, and anything else survives as-is.
fn nfkc_ascii(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match nfkd_expansion(character) {
            Some(expansion) => out.push_str(&expansion),
            None => out.push(character),
        }
    }
    out
}

/// The full NFKD expansion of `character`, when it is pure ASCII.
fn nfkd_expansion(character: char) -> Option<String> {
    let codepoint = character as u32;
    let index = NFKD_ASCII_RUNS.partition_point(|&(start, _, _)| start <= codepoint);
    if index == 0 {
        return None;
    }
    let (start, end, first) = NFKD_ASCII_RUNS[index - 1];
    if codepoint > end {
        return None;
    }
    let offset = codepoint - start;
    if offset == 0 {
        return Some(first.to_string());
    }
    let mut expansion = first.to_string();
    let last = expansion.pop()?;
    expansion.push(char::from_u32(last as u32 + offset)?);
    Some(expansion)
}

/// Runs of codepoints whose full NFKD decomposition is pure ASCII. Each entry
/// is `(first_codepoint, last_codepoint, expansion_of_first)`; within a run
/// each following codepoint's expansion advances the last character by one.
/// Generated from `unicodedata.normalize("NFKD", ...)` (Unicode 15.1.0).
static NFKD_ASCII_RUNS: &[(u32, u32, &str)] = &[
    (0x0000a0, 0x0000a0, " "),
    (0x0000aa, 0x0000aa, "a"),
    (0x0000b2, 0x0000b3, "2"),
    (0x0000b9, 0x0000b9, "1"),
    (0x0000ba, 0x0000ba, "o"),
    (0x000132, 0x000132, "IJ"),
    (0x000133, 0x000133, "ij"),
    (0x00017f, 0x00017f, "s"),
    (0x0001c7, 0x0001c7, "LJ"),
    (0x0001c8, 0x0001c8, "Lj"),
    (0x0001c9, 0x0001c9, "lj"),
    (0x0001ca, 0x0001ca, "NJ"),
    (0x0001cb, 0x0001cb, "Nj"),
    (0x0001cc, 0x0001cc, "nj"),
    (0x0001f1, 0x0001f1, "DZ"),
    (0x0001f2, 0x0001f2, "Dz"),
    (0x0001f3, 0x0001f3, "dz"),
    (0x0002b0, 0x0002b0, "h"),
    (0x0002b2, 0x0002b2, "j"),
    (0x0002b3, 0x0002b3, "r"),
    (0x0002b7, 0x0002b7, "w"),
    (0x0002b8, 0x0002b8, "y"),
    (0x0002e1, 0x0002e1, "l"),
    (0x0002e2, 0x0002e2, "s"),
    (0x0002e3, 0x0002e3, "x"),
    (0x00037e, 0x00037e, ";"),
    (0x001d2c, 0x001d2c, "A"),
    (0x001d2e, 0x001d2e, "B"),
    (0x001d30, 0x001d31, "D"),
    (0x001d33, 0x001d34, "G"),
    (0x001d35, 0x001d36, "I"),
    (0x001d37, 0x001d38, "K"),
    (0x001d39, 0x001d3a, "M"),
    (0x001d3c, 0x001d3c, "O"),
    (0x001d3e, 0x001d3e, "P"),
    (0x001d3f, 0x001d3f, "R"),
    (0x001d40, 0x001d41, "T"),
    (0x001d42, 0x001d42, "W"),
    (0x001d43, 0x001d43, "a"),
    (0x001d47, 0x001d47, "b"),
    (0x001d48, 0x001d49, "d"),
    (0x001d4d, 0x001d4d, "g"),
    (0x001d4f, 0x001d4f, "k"),
    (0x001d50, 0x001d50, "m"),
    (0x001d52, 0x001d52, "o"),
    (0x001d56, 0x001d56, "p"),
    (0x001d57, 0x001d58, "t"),
    (0x001d5b, 0x001d5b, "v"),
    (0x001d62, 0x001d62, "i"),
    (0x001d63, 0x001d63, "r"),
    (0x001d64, 0x001d65, "u"),
    (0x001d9c, 0x001d9c, "c"),
    (0x001da0, 0x001da0, "f"),
    (0x001dbb, 0x001dbb, "z"),
    (0x001fef, 0x001fef, "`"),
    (0x002000, 0x002000, " "),
    (0x002001, 0x002001, " "),
    (0x002002, 0x002002, " "),
    (0x002003, 0x002003, " "),
    (0x002004, 0x002004, " "),
    (0x002005, 0x002005, " "),
    (0x002006, 0x002006, " "),
    (0x002007, 0x002007, " "),
    (0x002008, 0x002008, " "),
    (0x002009, 0x002009, " "),
    (0x00200a, 0x00200a, " "),
    (0x002024, 0x002024, "."),
    (0x002025, 0x002025, ".."),
    (0x002026, 0x002026, "..."),
    (0x00202f, 0x00202f, " "),
    (0x00203c, 0x00203c, "!!"),
    (0x002047, 0x002047, "??"),
    (0x002048, 0x002048, "?!"),
    (0x002049, 0x002049, "!?"),
    (0x00205f, 0x00205f, " "),
    (0x002070, 0x002070, "0"),
    (0x002071, 0x002071, "i"),
    (0x002074, 0x002075, "4"),
    (0x002076, 0x002077, "6"),
    (0x002078, 0x002079, "8"),
    (0x00207a, 0x00207a, "+"),
    (0x00207c, 0x00207c, "="),
    (0x00207d, 0x00207e, "("),
    (0x00207f, 0x00207f, "n"),
    (0x002080, 0x002081, "0"),
    (0x002082, 0x002083, "2"),
    (0x002084, 0x002085, "4"),
    (0x002086, 0x002087, "6"),
    (0x002088, 0x002089, "8"),
    (0x00208a, 0x00208a, "+"),
    (0x00208c, 0x00208c, "="),
    (0x00208d, 0x00208e, "("),
    (0x002090, 0x002090, "a"),
    (0x002091, 0x002091, "e"),
    (0x002092, 0x002092, "o"),
    (0x002093, 0x002093, "x"),
    (0x002095, 0x002095, "h"),
    (0x002096, 0x002097, "k"),
    (0x002098, 0x002099, "m"),
    (0x00209a, 0x00209a, "p"),
    (0x00209b, 0x00209c, "s"),
    (0x0020a8, 0x0020a8, "Rs"),
    (0x002100, 0x002100, "a/c"),
    (0x002101, 0x002101, "a/s"),
    (0x002102, 0x002102, "C"),
    (0x002105, 0x002105, "c/o"),
    (0x002106, 0x002106, "c/u"),
    (0x00210a, 0x00210a, "g"),
    (0x00210b, 0x00210b, "H"),
    (0x00210c, 0x00210c, "H"),
    (0x00210d, 0x00210d, "H"),
    (0x00210e, 0x00210e, "h"),
    (0x002110, 0x002110, "I"),
    (0x002111, 0x002111, "I"),
    (0x002112, 0x002112, "L"),
    (0x002113, 0x002113, "l"),
    (0x002115, 0x002115, "N"),
    (0x002116, 0x002116, "No"),
    (0x002119, 0x00211a, "P"),
    (0x00211b, 0x00211b, "R"),
    (0x00211c, 0x00211c, "R"),
    (0x00211d, 0x00211d, "R"),
    (0x002120, 0x002120, "SM"),
    (0x002121, 0x002121, "TEL"),
    (0x002122, 0x002122, "TM"),
    (0x002124, 0x002124, "Z"),
    (0x002128, 0x002128, "Z"),
    (0x00212a, 0x00212a, "K"),
    (0x00212c, 0x00212d, "B"),
    (0x00212f, 0x00212f, "e"),
    (0x002130, 0x002131, "E"),
    (0x002133, 0x002133, "M"),
    (0x002134, 0x002134, "o"),
    (0x002139, 0x002139, "i"),
    (0x00213b, 0x00213b, "FAX"),
    (0x002145, 0x002145, "D"),
    (0x002146, 0x002147, "d"),
    (0x002148, 0x002149, "i"),
    (0x002160, 0x002160, "I"),
    (0x002161, 0x002161, "II"),
    (0x002162, 0x002162, "III"),
    (0x002163, 0x002163, "IV"),
    (0x002164, 0x002164, "V"),
    (0x002165, 0x002165, "VI"),
    (0x002166, 0x002166, "VII"),
    (0x002167, 0x002167, "VIII"),
    (0x002168, 0x002168, "IX"),
    (0x002169, 0x002169, "X"),
    (0x00216a, 0x00216a, "XI"),
    (0x00216b, 0x00216b, "XII"),
    (0x00216c, 0x00216c, "L"),
    (0x00216d, 0x00216e, "C"),
    (0x00216f, 0x00216f, "M"),
    (0x002170, 0x002170, "i"),
    (0x002171, 0x002171, "ii"),
    (0x002172, 0x002172, "iii"),
    (0x002173, 0x002173, "iv"),
    (0x002174, 0x002174, "v"),
    (0x002175, 0x002175, "vi"),
    (0x002176, 0x002176, "vii"),
    (0x002177, 0x002177, "viii"),
    (0x002178, 0x002178, "ix"),
    (0x002179, 0x002179, "x"),
    (0x00217a, 0x00217a, "xi"),
    (0x00217b, 0x00217b, "xii"),
    (0x00217c, 0x00217c, "l"),
    (0x00217d, 0x00217e, "c"),
    (0x00217f, 0x00217f, "m"),
    (0x002460, 0x002461, "1"),
    (0x002462, 0x002463, "3"),
    (0x002464, 0x002465, "5"),
    (0x002466, 0x002467, "7"),
    (0x002468, 0x002468, "9"),
    (0x002469, 0x00246a, "10"),
    (0x00246b, 0x00246c, "12"),
    (0x00246d, 0x00246e, "14"),
    (0x00246f, 0x002470, "16"),
    (0x002471, 0x002472, "18"),
    (0x002473, 0x002473, "20"),
    (0x002474, 0x002474, "(1)"),
    (0x002475, 0x002475, "(2)"),
    (0x002476, 0x002476, "(3)"),
    (0x002477, 0x002477, "(4)"),
    (0x002478, 0x002478, "(5)"),
    (0x002479, 0x002479, "(6)"),
    (0x00247a, 0x00247a, "(7)"),
    (0x00247b, 0x00247b, "(8)"),
    (0x00247c, 0x00247c, "(9)"),
    (0x00247d, 0x00247d, "(10)"),
    (0x00247e, 0x00247e, "(11)"),
    (0x00247f, 0x00247f, "(12)"),
    (0x002480, 0x002480, "(13)"),
    (0x002481, 0x002481, "(14)"),
    (0x002482, 0x002482, "(15)"),
    (0x002483, 0x002483, "(16)"),
    (0x002484, 0x002484, "(17)"),
    (0x002485, 0x002485, "(18)"),
    (0x002486, 0x002486, "(19)"),
    (0x002487, 0x002487, "(20)"),
    (0x002488, 0x002488, "1."),
    (0x002489, 0x002489, "2."),
    (0x00248a, 0x00248a, "3."),
    (0x00248b, 0x00248b, "4."),
    (0x00248c, 0x00248c, "5."),
    (0x00248d, 0x00248d, "6."),
    (0x00248e, 0x00248e, "7."),
    (0x00248f, 0x00248f, "8."),
    (0x002490, 0x002490, "9."),
    (0x002491, 0x002491, "10."),
    (0x002492, 0x002492, "11."),
    (0x002493, 0x002493, "12."),
    (0x002494, 0x002494, "13."),
    (0x002495, 0x002495, "14."),
    (0x002496, 0x002496, "15."),
    (0x002497, 0x002497, "16."),
    (0x002498, 0x002498, "17."),
    (0x002499, 0x002499, "18."),
    (0x00249a, 0x00249a, "19."),
    (0x00249b, 0x00249b, "20."),
    (0x00249c, 0x00249c, "(a)"),
    (0x00249d, 0x00249d, "(b)"),
    (0x00249e, 0x00249e, "(c)"),
    (0x00249f, 0x00249f, "(d)"),
    (0x0024a0, 0x0024a0, "(e)"),
    (0x0024a1, 0x0024a1, "(f)"),
    (0x0024a2, 0x0024a2, "(g)"),
    (0x0024a3, 0x0024a3, "(h)"),
    (0x0024a4, 0x0024a4, "(i)"),
    (0x0024a5, 0x0024a5, "(j)"),
    (0x0024a6, 0x0024a6, "(k)"),
    (0x0024a7, 0x0024a7, "(l)"),
    (0x0024a8, 0x0024a8, "(m)"),
    (0x0024a9, 0x0024a9, "(n)"),
    (0x0024aa, 0x0024aa, "(o)"),
    (0x0024ab, 0x0024ab, "(p)"),
    (0x0024ac, 0x0024ac, "(q)"),
    (0x0024ad, 0x0024ad, "(r)"),
    (0x0024ae, 0x0024ae, "(s)"),
    (0x0024af, 0x0024af, "(t)"),
    (0x0024b0, 0x0024b0, "(u)"),
    (0x0024b1, 0x0024b1, "(v)"),
    (0x0024b2, 0x0024b2, "(w)"),
    (0x0024b3, 0x0024b3, "(x)"),
    (0x0024b4, 0x0024b4, "(y)"),
    (0x0024b5, 0x0024b5, "(z)"),
    (0x0024b6, 0x0024b7, "A"),
    (0x0024b8, 0x0024b9, "C"),
    (0x0024ba, 0x0024bb, "E"),
    (0x0024bc, 0x0024bd, "G"),
    (0x0024be, 0x0024bf, "I"),
    (0x0024c0, 0x0024c1, "K"),
    (0x0024c2, 0x0024c3, "M"),
    (0x0024c4, 0x0024c5, "O"),
    (0x0024c6, 0x0024c7, "Q"),
    (0x0024c8, 0x0024c9, "S"),
    (0x0024ca, 0x0024cb, "U"),
    (0x0024cc, 0x0024cd, "W"),
    (0x0024ce, 0x0024cf, "Y"),
    (0x0024d0, 0x0024d1, "a"),
    (0x0024d2, 0x0024d3, "c"),
    (0x0024d4, 0x0024d5, "e"),
    (0x0024d6, 0x0024d7, "g"),
    (0x0024d8, 0x0024d9, "i"),
    (0x0024da, 0x0024db, "k"),
    (0x0024dc, 0x0024dd, "m"),
    (0x0024de, 0x0024df, "o"),
    (0x0024e0, 0x0024e1, "q"),
    (0x0024e2, 0x0024e3, "s"),
    (0x0024e4, 0x0024e5, "u"),
    (0x0024e6, 0x0024e7, "w"),
    (0x0024e8, 0x0024e9, "y"),
    (0x0024ea, 0x0024ea, "0"),
    (0x002a74, 0x002a74, "::="),
    (0x002a75, 0x002a75, "=="),
    (0x002a76, 0x002a76, "==="),
    (0x002c7c, 0x002c7c, "j"),
    (0x002c7d, 0x002c7d, "V"),
    (0x003000, 0x003000, " "),
    (0x003250, 0x003250, "PTE"),
    (0x003251, 0x003252, "21"),
    (0x003253, 0x003254, "23"),
    (0x003255, 0x003256, "25"),
    (0x003257, 0x003258, "27"),
    (0x003259, 0x003259, "29"),
    (0x00325a, 0x00325b, "30"),
    (0x00325c, 0x00325d, "32"),
    (0x00325e, 0x00325f, "34"),
    (0x0032b1, 0x0032b2, "36"),
    (0x0032b3, 0x0032b4, "38"),
    (0x0032b5, 0x0032b6, "40"),
    (0x0032b7, 0x0032b8, "42"),
    (0x0032b9, 0x0032ba, "44"),
    (0x0032bb, 0x0032bc, "46"),
    (0x0032bd, 0x0032be, "48"),
    (0x0032bf, 0x0032bf, "50"),
    (0x0032cc, 0x0032cc, "Hg"),
    (0x0032cd, 0x0032cd, "erg"),
    (0x0032ce, 0x0032ce, "eV"),
    (0x0032cf, 0x0032cf, "LTD"),
    (0x003371, 0x003371, "hPa"),
    (0x003372, 0x003372, "da"),
    (0x003373, 0x003373, "AU"),
    (0x003374, 0x003374, "bar"),
    (0x003375, 0x003375, "oV"),
    (0x003376, 0x003376, "pc"),
    (0x003377, 0x003377, "dm"),
    (0x003378, 0x003379, "dm2"),
    (0x00337a, 0x00337a, "IU"),
    (0x003380, 0x003380, "pA"),
    (0x003381, 0x003381, "nA"),
    (0x003383, 0x003383, "mA"),
    (0x003384, 0x003384, "kA"),
    (0x003385, 0x003385, "KB"),
    (0x003386, 0x003386, "MB"),
    (0x003387, 0x003387, "GB"),
    (0x003388, 0x003388, "cal"),
    (0x003389, 0x003389, "kcal"),
    (0x00338a, 0x00338a, "pF"),
    (0x00338b, 0x00338b, "nF"),
    (0x00338e, 0x00338e, "mg"),
    (0x00338f, 0x00338f, "kg"),
    (0x003390, 0x003390, "Hz"),
    (0x003391, 0x003391, "kHz"),
    (0x003392, 0x003392, "MHz"),
    (0x003393, 0x003393, "GHz"),
    (0x003394, 0x003394, "THz"),
    (0x003396, 0x003396, "ml"),
    (0x003397, 0x003397, "dl"),
    (0x003398, 0x003398, "kl"),
    (0x003399, 0x003399, "fm"),
    (0x00339a, 0x00339a, "nm"),
    (0x00339c, 0x00339c, "mm"),
    (0x00339d, 0x00339d, "cm"),
    (0x00339e, 0x00339e, "km"),
    (0x00339f, 0x00339f, "mm2"),
    (0x0033a0, 0x0033a0, "cm2"),
    (0x0033a1, 0x0033a1, "m2"),
    (0x0033a2, 0x0033a2, "km2"),
    (0x0033a3, 0x0033a3, "mm3"),
    (0x0033a4, 0x0033a4, "cm3"),
    (0x0033a5, 0x0033a5, "m3"),
    (0x0033a6, 0x0033a6, "km3"),
    (0x0033a9, 0x0033a9, "Pa"),
    (0x0033aa, 0x0033aa, "kPa"),
    (0x0033ab, 0x0033ab, "MPa"),
    (0x0033ac, 0x0033ac, "GPa"),
    (0x0033ad, 0x0033ad, "rad"),
    (0x0033b0, 0x0033b0, "ps"),
    (0x0033b1, 0x0033b1, "ns"),
    (0x0033b3, 0x0033b3, "ms"),
    (0x0033b4, 0x0033b4, "pV"),
    (0x0033b5, 0x0033b5, "nV"),
    (0x0033b7, 0x0033b7, "mV"),
    (0x0033b8, 0x0033b8, "kV"),
    (0x0033b9, 0x0033b9, "MV"),
    (0x0033ba, 0x0033ba, "pW"),
    (0x0033bb, 0x0033bb, "nW"),
    (0x0033bd, 0x0033bd, "mW"),
    (0x0033be, 0x0033be, "kW"),
    (0x0033bf, 0x0033bf, "MW"),
    (0x0033c2, 0x0033c2, "a.m."),
    (0x0033c3, 0x0033c3, "Bq"),
    (0x0033c4, 0x0033c5, "cc"),
    (0x0033c7, 0x0033c7, "Co."),
    (0x0033c8, 0x0033c8, "dB"),
    (0x0033c9, 0x0033c9, "Gy"),
    (0x0033ca, 0x0033ca, "ha"),
    (0x0033cb, 0x0033cb, "HP"),
    (0x0033cc, 0x0033cc, "in"),
    (0x0033cd, 0x0033cd, "KK"),
    (0x0033ce, 0x0033ce, "KM"),
    (0x0033cf, 0x0033cf, "kt"),
    (0x0033d0, 0x0033d1, "lm"),
    (0x0033d2, 0x0033d2, "log"),
    (0x0033d3, 0x0033d3, "lx"),
    (0x0033d4, 0x0033d4, "mb"),
    (0x0033d5, 0x0033d5, "mil"),
    (0x0033d6, 0x0033d6, "mol"),
    (0x0033d7, 0x0033d7, "PH"),
    (0x0033d8, 0x0033d8, "p.m."),
    (0x0033d9, 0x0033d9, "PPM"),
    (0x0033da, 0x0033da, "PR"),
    (0x0033db, 0x0033db, "sr"),
    (0x0033dc, 0x0033dc, "Sv"),
    (0x0033dd, 0x0033dd, "Wb"),
    (0x0033ff, 0x0033ff, "gal"),
    (0x00a7f2, 0x00a7f2, "C"),
    (0x00a7f3, 0x00a7f3, "F"),
    (0x00a7f4, 0x00a7f4, "Q"),
    (0x00fb00, 0x00fb00, "ff"),
    (0x00fb01, 0x00fb01, "fi"),
    (0x00fb02, 0x00fb02, "fl"),
    (0x00fb03, 0x00fb03, "ffi"),
    (0x00fb04, 0x00fb04, "ffl"),
    (0x00fb05, 0x00fb05, "st"),
    (0x00fb06, 0x00fb06, "st"),
    (0x00fb29, 0x00fb29, "+"),
    (0x00fe10, 0x00fe10, ","),
    (0x00fe13, 0x00fe14, ":"),
    (0x00fe15, 0x00fe15, "!"),
    (0x00fe16, 0x00fe16, "?"),
    (0x00fe19, 0x00fe19, "..."),
    (0x00fe30, 0x00fe30, ".."),
    (0x00fe33, 0x00fe33, "_"),
    (0x00fe34, 0x00fe34, "_"),
    (0x00fe35, 0x00fe36, "("),
    (0x00fe37, 0x00fe37, "{"),
    (0x00fe38, 0x00fe38, "}"),
    (0x00fe47, 0x00fe47, "["),
    (0x00fe48, 0x00fe48, "]"),
    (0x00fe4d, 0x00fe4d, "_"),
    (0x00fe4e, 0x00fe4e, "_"),
    (0x00fe4f, 0x00fe4f, "_"),
    (0x00fe50, 0x00fe50, ","),
    (0x00fe52, 0x00fe52, "."),
    (0x00fe54, 0x00fe54, ";"),
    (0x00fe55, 0x00fe55, ":"),
    (0x00fe56, 0x00fe56, "?"),
    (0x00fe57, 0x00fe57, "!"),
    (0x00fe59, 0x00fe5a, "("),
    (0x00fe5b, 0x00fe5b, "{"),
    (0x00fe5c, 0x00fe5c, "}"),
    (0x00fe5f, 0x00fe5f, "#"),
    (0x00fe60, 0x00fe60, "&"),
    (0x00fe61, 0x00fe62, "*"),
    (0x00fe63, 0x00fe63, "-"),
    (0x00fe64, 0x00fe64, "<"),
    (0x00fe65, 0x00fe65, ">"),
    (0x00fe66, 0x00fe66, "="),
    (0x00fe68, 0x00fe68, "\\"),
    (0x00fe69, 0x00fe6a, "$"),
    (0x00fe6b, 0x00fe6b, "@"),
    (0x00ff01, 0x00ff02, "!"),
    (0x00ff03, 0x00ff04, "#"),
    (0x00ff05, 0x00ff06, "%"),
    (0x00ff07, 0x00ff08, "'"),
    (0x00ff09, 0x00ff0a, ")"),
    (0x00ff0b, 0x00ff0c, "+"),
    (0x00ff0d, 0x00ff0e, "-"),
    (0x00ff0f, 0x00ff10, "/"),
    (0x00ff11, 0x00ff12, "1"),
    (0x00ff13, 0x00ff14, "3"),
    (0x00ff15, 0x00ff16, "5"),
    (0x00ff17, 0x00ff18, "7"),
    (0x00ff19, 0x00ff1a, "9"),
    (0x00ff1b, 0x00ff1c, ";"),
    (0x00ff1d, 0x00ff1e, "="),
    (0x00ff1f, 0x00ff20, "?"),
    (0x00ff21, 0x00ff22, "A"),
    (0x00ff23, 0x00ff24, "C"),
    (0x00ff25, 0x00ff26, "E"),
    (0x00ff27, 0x00ff28, "G"),
    (0x00ff29, 0x00ff2a, "I"),
    (0x00ff2b, 0x00ff2c, "K"),
    (0x00ff2d, 0x00ff2e, "M"),
    (0x00ff2f, 0x00ff30, "O"),
    (0x00ff31, 0x00ff32, "Q"),
    (0x00ff33, 0x00ff34, "S"),
    (0x00ff35, 0x00ff36, "U"),
    (0x00ff37, 0x00ff38, "W"),
    (0x00ff39, 0x00ff3a, "Y"),
    (0x00ff3b, 0x00ff3c, "["),
    (0x00ff3d, 0x00ff3e, "]"),
    (0x00ff3f, 0x00ff40, "_"),
    (0x00ff41, 0x00ff42, "a"),
    (0x00ff43, 0x00ff44, "c"),
    (0x00ff45, 0x00ff46, "e"),
    (0x00ff47, 0x00ff48, "g"),
    (0x00ff49, 0x00ff4a, "i"),
    (0x00ff4b, 0x00ff4c, "k"),
    (0x00ff4d, 0x00ff4e, "m"),
    (0x00ff4f, 0x00ff50, "o"),
    (0x00ff51, 0x00ff52, "q"),
    (0x00ff53, 0x00ff54, "s"),
    (0x00ff55, 0x00ff56, "u"),
    (0x00ff57, 0x00ff58, "w"),
    (0x00ff59, 0x00ff5a, "y"),
    (0x00ff5b, 0x00ff5c, "{"),
    (0x00ff5d, 0x00ff5e, "}"),
    (0x0107a5, 0x0107a5, "q"),
    (0x01d400, 0x01d401, "A"),
    (0x01d402, 0x01d403, "C"),
    (0x01d404, 0x01d405, "E"),
    (0x01d406, 0x01d407, "G"),
    (0x01d408, 0x01d409, "I"),
    (0x01d40a, 0x01d40b, "K"),
    (0x01d40c, 0x01d40d, "M"),
    (0x01d40e, 0x01d40f, "O"),
    (0x01d410, 0x01d411, "Q"),
    (0x01d412, 0x01d413, "S"),
    (0x01d414, 0x01d415, "U"),
    (0x01d416, 0x01d417, "W"),
    (0x01d418, 0x01d419, "Y"),
    (0x01d41a, 0x01d41b, "a"),
    (0x01d41c, 0x01d41d, "c"),
    (0x01d41e, 0x01d41f, "e"),
    (0x01d420, 0x01d421, "g"),
    (0x01d422, 0x01d423, "i"),
    (0x01d424, 0x01d425, "k"),
    (0x01d426, 0x01d427, "m"),
    (0x01d428, 0x01d429, "o"),
    (0x01d42a, 0x01d42b, "q"),
    (0x01d42c, 0x01d42d, "s"),
    (0x01d42e, 0x01d42f, "u"),
    (0x01d430, 0x01d431, "w"),
    (0x01d432, 0x01d433, "y"),
    (0x01d434, 0x01d435, "A"),
    (0x01d436, 0x01d437, "C"),
    (0x01d438, 0x01d439, "E"),
    (0x01d43a, 0x01d43b, "G"),
    (0x01d43c, 0x01d43d, "I"),
    (0x01d43e, 0x01d43f, "K"),
    (0x01d440, 0x01d441, "M"),
    (0x01d442, 0x01d443, "O"),
    (0x01d444, 0x01d445, "Q"),
    (0x01d446, 0x01d447, "S"),
    (0x01d448, 0x01d449, "U"),
    (0x01d44a, 0x01d44b, "W"),
    (0x01d44c, 0x01d44d, "Y"),
    (0x01d44e, 0x01d44f, "a"),
    (0x01d450, 0x01d451, "c"),
    (0x01d452, 0x01d453, "e"),
    (0x01d454, 0x01d454, "g"),
    (0x01d456, 0x01d457, "i"),
    (0x01d458, 0x01d459, "k"),
    (0x01d45a, 0x01d45b, "m"),
    (0x01d45c, 0x01d45d, "o"),
    (0x01d45e, 0x01d45f, "q"),
    (0x01d460, 0x01d461, "s"),
    (0x01d462, 0x01d463, "u"),
    (0x01d464, 0x01d465, "w"),
    (0x01d466, 0x01d467, "y"),
    (0x01d468, 0x01d469, "A"),
    (0x01d46a, 0x01d46b, "C"),
    (0x01d46c, 0x01d46d, "E"),
    (0x01d46e, 0x01d46f, "G"),
    (0x01d470, 0x01d471, "I"),
    (0x01d472, 0x01d473, "K"),
    (0x01d474, 0x01d475, "M"),
    (0x01d476, 0x01d477, "O"),
    (0x01d478, 0x01d479, "Q"),
    (0x01d47a, 0x01d47b, "S"),
    (0x01d47c, 0x01d47d, "U"),
    (0x01d47e, 0x01d47f, "W"),
    (0x01d480, 0x01d481, "Y"),
    (0x01d482, 0x01d483, "a"),
    (0x01d484, 0x01d485, "c"),
    (0x01d486, 0x01d487, "e"),
    (0x01d488, 0x01d489, "g"),
    (0x01d48a, 0x01d48b, "i"),
    (0x01d48c, 0x01d48d, "k"),
    (0x01d48e, 0x01d48f, "m"),
    (0x01d490, 0x01d491, "o"),
    (0x01d492, 0x01d493, "q"),
    (0x01d494, 0x01d495, "s"),
    (0x01d496, 0x01d497, "u"),
    (0x01d498, 0x01d499, "w"),
    (0x01d49a, 0x01d49b, "y"),
    (0x01d49c, 0x01d49c, "A"),
    (0x01d49e, 0x01d49f, "C"),
    (0x01d4a2, 0x01d4a2, "G"),
    (0x01d4a5, 0x01d4a6, "J"),
    (0x01d4a9, 0x01d4aa, "N"),
    (0x01d4ab, 0x01d4ac, "P"),
    (0x01d4ae, 0x01d4af, "S"),
    (0x01d4b0, 0x01d4b1, "U"),
    (0x01d4b2, 0x01d4b3, "W"),
    (0x01d4b4, 0x01d4b5, "Y"),
    (0x01d4b6, 0x01d4b7, "a"),
    (0x01d4b8, 0x01d4b9, "c"),
    (0x01d4bb, 0x01d4bb, "f"),
    (0x01d4bd, 0x01d4be, "h"),
    (0x01d4bf, 0x01d4c0, "j"),
    (0x01d4c1, 0x01d4c2, "l"),
    (0x01d4c3, 0x01d4c3, "n"),
    (0x01d4c5, 0x01d4c6, "p"),
    (0x01d4c7, 0x01d4c8, "r"),
    (0x01d4c9, 0x01d4ca, "t"),
    (0x01d4cb, 0x01d4cc, "v"),
    (0x01d4cd, 0x01d4ce, "x"),
    (0x01d4cf, 0x01d4cf, "z"),
    (0x01d4d0, 0x01d4d1, "A"),
    (0x01d4d2, 0x01d4d3, "C"),
    (0x01d4d4, 0x01d4d5, "E"),
    (0x01d4d6, 0x01d4d7, "G"),
    (0x01d4d8, 0x01d4d9, "I"),
    (0x01d4da, 0x01d4db, "K"),
    (0x01d4dc, 0x01d4dd, "M"),
    (0x01d4de, 0x01d4df, "O"),
    (0x01d4e0, 0x01d4e1, "Q"),
    (0x01d4e2, 0x01d4e3, "S"),
    (0x01d4e4, 0x01d4e5, "U"),
    (0x01d4e6, 0x01d4e7, "W"),
    (0x01d4e8, 0x01d4e9, "Y"),
    (0x01d4ea, 0x01d4eb, "a"),
    (0x01d4ec, 0x01d4ed, "c"),
    (0x01d4ee, 0x01d4ef, "e"),
    (0x01d4f0, 0x01d4f1, "g"),
    (0x01d4f2, 0x01d4f3, "i"),
    (0x01d4f4, 0x01d4f5, "k"),
    (0x01d4f6, 0x01d4f7, "m"),
    (0x01d4f8, 0x01d4f9, "o"),
    (0x01d4fa, 0x01d4fb, "q"),
    (0x01d4fc, 0x01d4fd, "s"),
    (0x01d4fe, 0x01d4ff, "u"),
    (0x01d500, 0x01d501, "w"),
    (0x01d502, 0x01d503, "y"),
    (0x01d504, 0x01d505, "A"),
    (0x01d507, 0x01d508, "D"),
    (0x01d509, 0x01d50a, "F"),
    (0x01d50d, 0x01d50e, "J"),
    (0x01d50f, 0x01d510, "L"),
    (0x01d511, 0x01d512, "N"),
    (0x01d513, 0x01d514, "P"),
    (0x01d516, 0x01d517, "S"),
    (0x01d518, 0x01d519, "U"),
    (0x01d51a, 0x01d51b, "W"),
    (0x01d51c, 0x01d51c, "Y"),
    (0x01d51e, 0x01d51f, "a"),
    (0x01d520, 0x01d521, "c"),
    (0x01d522, 0x01d523, "e"),
    (0x01d524, 0x01d525, "g"),
    (0x01d526, 0x01d527, "i"),
    (0x01d528, 0x01d529, "k"),
    (0x01d52a, 0x01d52b, "m"),
    (0x01d52c, 0x01d52d, "o"),
    (0x01d52e, 0x01d52f, "q"),
    (0x01d530, 0x01d531, "s"),
    (0x01d532, 0x01d533, "u"),
    (0x01d534, 0x01d535, "w"),
    (0x01d536, 0x01d537, "y"),
    (0x01d538, 0x01d539, "A"),
    (0x01d53b, 0x01d53c, "D"),
    (0x01d53d, 0x01d53e, "F"),
    (0x01d540, 0x01d541, "I"),
    (0x01d542, 0x01d543, "K"),
    (0x01d544, 0x01d544, "M"),
    (0x01d546, 0x01d546, "O"),
    (0x01d54a, 0x01d54b, "S"),
    (0x01d54c, 0x01d54d, "U"),
    (0x01d54e, 0x01d54f, "W"),
    (0x01d550, 0x01d550, "Y"),
    (0x01d552, 0x01d553, "a"),
    (0x01d554, 0x01d555, "c"),
    (0x01d556, 0x01d557, "e"),
    (0x01d558, 0x01d559, "g"),
    (0x01d55a, 0x01d55b, "i"),
    (0x01d55c, 0x01d55d, "k"),
    (0x01d55e, 0x01d55f, "m"),
    (0x01d560, 0x01d561, "o"),
    (0x01d562, 0x01d563, "q"),
    (0x01d564, 0x01d565, "s"),
    (0x01d566, 0x01d567, "u"),
    (0x01d568, 0x01d569, "w"),
    (0x01d56a, 0x01d56b, "y"),
    (0x01d56c, 0x01d56d, "A"),
    (0x01d56e, 0x01d56f, "C"),
    (0x01d570, 0x01d571, "E"),
    (0x01d572, 0x01d573, "G"),
    (0x01d574, 0x01d575, "I"),
    (0x01d576, 0x01d577, "K"),
    (0x01d578, 0x01d579, "M"),
    (0x01d57a, 0x01d57b, "O"),
    (0x01d57c, 0x01d57d, "Q"),
    (0x01d57e, 0x01d57f, "S"),
    (0x01d580, 0x01d581, "U"),
    (0x01d582, 0x01d583, "W"),
    (0x01d584, 0x01d585, "Y"),
    (0x01d586, 0x01d587, "a"),
    (0x01d588, 0x01d589, "c"),
    (0x01d58a, 0x01d58b, "e"),
    (0x01d58c, 0x01d58d, "g"),
    (0x01d58e, 0x01d58f, "i"),
    (0x01d590, 0x01d591, "k"),
    (0x01d592, 0x01d593, "m"),
    (0x01d594, 0x01d595, "o"),
    (0x01d596, 0x01d597, "q"),
    (0x01d598, 0x01d599, "s"),
    (0x01d59a, 0x01d59b, "u"),
    (0x01d59c, 0x01d59d, "w"),
    (0x01d59e, 0x01d59f, "y"),
    (0x01d5a0, 0x01d5a1, "A"),
    (0x01d5a2, 0x01d5a3, "C"),
    (0x01d5a4, 0x01d5a5, "E"),
    (0x01d5a6, 0x01d5a7, "G"),
    (0x01d5a8, 0x01d5a9, "I"),
    (0x01d5aa, 0x01d5ab, "K"),
    (0x01d5ac, 0x01d5ad, "M"),
    (0x01d5ae, 0x01d5af, "O"),
    (0x01d5b0, 0x01d5b1, "Q"),
    (0x01d5b2, 0x01d5b3, "S"),
    (0x01d5b4, 0x01d5b5, "U"),
    (0x01d5b6, 0x01d5b7, "W"),
    (0x01d5b8, 0x01d5b9, "Y"),
    (0x01d5ba, 0x01d5bb, "a"),
    (0x01d5bc, 0x01d5bd, "c"),
    (0x01d5be, 0x01d5bf, "e"),
    (0x01d5c0, 0x01d5c1, "g"),
    (0x01d5c2, 0x01d5c3, "i"),
    (0x01d5c4, 0x01d5c5, "k"),
    (0x01d5c6, 0x01d5c7, "m"),
    (0x01d5c8, 0x01d5c9, "o"),
    (0x01d5ca, 0x01d5cb, "q"),
    (0x01d5cc, 0x01d5cd, "s"),
    (0x01d5ce, 0x01d5cf, "u"),
    (0x01d5d0, 0x01d5d1, "w"),
    (0x01d5d2, 0x01d5d3, "y"),
    (0x01d5d4, 0x01d5d5, "A"),
    (0x01d5d6, 0x01d5d7, "C"),
    (0x01d5d8, 0x01d5d9, "E"),
    (0x01d5da, 0x01d5db, "G"),
    (0x01d5dc, 0x01d5dd, "I"),
    (0x01d5de, 0x01d5df, "K"),
    (0x01d5e0, 0x01d5e1, "M"),
    (0x01d5e2, 0x01d5e3, "O"),
    (0x01d5e4, 0x01d5e5, "Q"),
    (0x01d5e6, 0x01d5e7, "S"),
    (0x01d5e8, 0x01d5e9, "U"),
    (0x01d5ea, 0x01d5eb, "W"),
    (0x01d5ec, 0x01d5ed, "Y"),
    (0x01d5ee, 0x01d5ef, "a"),
    (0x01d5f0, 0x01d5f1, "c"),
    (0x01d5f2, 0x01d5f3, "e"),
    (0x01d5f4, 0x01d5f5, "g"),
    (0x01d5f6, 0x01d5f7, "i"),
    (0x01d5f8, 0x01d5f9, "k"),
    (0x01d5fa, 0x01d5fb, "m"),
    (0x01d5fc, 0x01d5fd, "o"),
    (0x01d5fe, 0x01d5ff, "q"),
    (0x01d600, 0x01d601, "s"),
    (0x01d602, 0x01d603, "u"),
    (0x01d604, 0x01d605, "w"),
    (0x01d606, 0x01d607, "y"),
    (0x01d608, 0x01d609, "A"),
    (0x01d60a, 0x01d60b, "C"),
    (0x01d60c, 0x01d60d, "E"),
    (0x01d60e, 0x01d60f, "G"),
    (0x01d610, 0x01d611, "I"),
    (0x01d612, 0x01d613, "K"),
    (0x01d614, 0x01d615, "M"),
    (0x01d616, 0x01d617, "O"),
    (0x01d618, 0x01d619, "Q"),
    (0x01d61a, 0x01d61b, "S"),
    (0x01d61c, 0x01d61d, "U"),
    (0x01d61e, 0x01d61f, "W"),
    (0x01d620, 0x01d621, "Y"),
    (0x01d622, 0x01d623, "a"),
    (0x01d624, 0x01d625, "c"),
    (0x01d626, 0x01d627, "e"),
    (0x01d628, 0x01d629, "g"),
    (0x01d62a, 0x01d62b, "i"),
    (0x01d62c, 0x01d62d, "k"),
    (0x01d62e, 0x01d62f, "m"),
    (0x01d630, 0x01d631, "o"),
    (0x01d632, 0x01d633, "q"),
    (0x01d634, 0x01d635, "s"),
    (0x01d636, 0x01d637, "u"),
    (0x01d638, 0x01d639, "w"),
    (0x01d63a, 0x01d63b, "y"),
    (0x01d63c, 0x01d63d, "A"),
    (0x01d63e, 0x01d63f, "C"),
    (0x01d640, 0x01d641, "E"),
    (0x01d642, 0x01d643, "G"),
    (0x01d644, 0x01d645, "I"),
    (0x01d646, 0x01d647, "K"),
    (0x01d648, 0x01d649, "M"),
    (0x01d64a, 0x01d64b, "O"),
    (0x01d64c, 0x01d64d, "Q"),
    (0x01d64e, 0x01d64f, "S"),
    (0x01d650, 0x01d651, "U"),
    (0x01d652, 0x01d653, "W"),
    (0x01d654, 0x01d655, "Y"),
    (0x01d656, 0x01d657, "a"),
    (0x01d658, 0x01d659, "c"),
    (0x01d65a, 0x01d65b, "e"),
    (0x01d65c, 0x01d65d, "g"),
    (0x01d65e, 0x01d65f, "i"),
    (0x01d660, 0x01d661, "k"),
    (0x01d662, 0x01d663, "m"),
    (0x01d664, 0x01d665, "o"),
    (0x01d666, 0x01d667, "q"),
    (0x01d668, 0x01d669, "s"),
    (0x01d66a, 0x01d66b, "u"),
    (0x01d66c, 0x01d66d, "w"),
    (0x01d66e, 0x01d66f, "y"),
    (0x01d670, 0x01d671, "A"),
    (0x01d672, 0x01d673, "C"),
    (0x01d674, 0x01d675, "E"),
    (0x01d676, 0x01d677, "G"),
    (0x01d678, 0x01d679, "I"),
    (0x01d67a, 0x01d67b, "K"),
    (0x01d67c, 0x01d67d, "M"),
    (0x01d67e, 0x01d67f, "O"),
    (0x01d680, 0x01d681, "Q"),
    (0x01d682, 0x01d683, "S"),
    (0x01d684, 0x01d685, "U"),
    (0x01d686, 0x01d687, "W"),
    (0x01d688, 0x01d689, "Y"),
    (0x01d68a, 0x01d68b, "a"),
    (0x01d68c, 0x01d68d, "c"),
    (0x01d68e, 0x01d68f, "e"),
    (0x01d690, 0x01d691, "g"),
    (0x01d692, 0x01d693, "i"),
    (0x01d694, 0x01d695, "k"),
    (0x01d696, 0x01d697, "m"),
    (0x01d698, 0x01d699, "o"),
    (0x01d69a, 0x01d69b, "q"),
    (0x01d69c, 0x01d69d, "s"),
    (0x01d69e, 0x01d69f, "u"),
    (0x01d6a0, 0x01d6a1, "w"),
    (0x01d6a2, 0x01d6a3, "y"),
    (0x01d7ce, 0x01d7cf, "0"),
    (0x01d7d0, 0x01d7d1, "2"),
    (0x01d7d2, 0x01d7d3, "4"),
    (0x01d7d4, 0x01d7d5, "6"),
    (0x01d7d6, 0x01d7d7, "8"),
    (0x01d7d8, 0x01d7d9, "0"),
    (0x01d7da, 0x01d7db, "2"),
    (0x01d7dc, 0x01d7dd, "4"),
    (0x01d7de, 0x01d7df, "6"),
    (0x01d7e0, 0x01d7e1, "8"),
    (0x01d7e2, 0x01d7e3, "0"),
    (0x01d7e4, 0x01d7e5, "2"),
    (0x01d7e6, 0x01d7e7, "4"),
    (0x01d7e8, 0x01d7e9, "6"),
    (0x01d7ea, 0x01d7eb, "8"),
    (0x01d7ec, 0x01d7ed, "0"),
    (0x01d7ee, 0x01d7ef, "2"),
    (0x01d7f0, 0x01d7f1, "4"),
    (0x01d7f2, 0x01d7f3, "6"),
    (0x01d7f4, 0x01d7f5, "8"),
    (0x01d7f6, 0x01d7f7, "0"),
    (0x01d7f8, 0x01d7f9, "2"),
    (0x01d7fa, 0x01d7fb, "4"),
    (0x01d7fc, 0x01d7fd, "6"),
    (0x01d7fe, 0x01d7ff, "8"),
    (0x01f100, 0x01f100, "0."),
    (0x01f101, 0x01f101, "0,"),
    (0x01f102, 0x01f102, "1,"),
    (0x01f103, 0x01f103, "2,"),
    (0x01f104, 0x01f104, "3,"),
    (0x01f105, 0x01f105, "4,"),
    (0x01f106, 0x01f106, "5,"),
    (0x01f107, 0x01f107, "6,"),
    (0x01f108, 0x01f108, "7,"),
    (0x01f109, 0x01f109, "8,"),
    (0x01f10a, 0x01f10a, "9,"),
    (0x01f110, 0x01f110, "(A)"),
    (0x01f111, 0x01f111, "(B)"),
    (0x01f112, 0x01f112, "(C)"),
    (0x01f113, 0x01f113, "(D)"),
    (0x01f114, 0x01f114, "(E)"),
    (0x01f115, 0x01f115, "(F)"),
    (0x01f116, 0x01f116, "(G)"),
    (0x01f117, 0x01f117, "(H)"),
    (0x01f118, 0x01f118, "(I)"),
    (0x01f119, 0x01f119, "(J)"),
    (0x01f11a, 0x01f11a, "(K)"),
    (0x01f11b, 0x01f11b, "(L)"),
    (0x01f11c, 0x01f11c, "(M)"),
    (0x01f11d, 0x01f11d, "(N)"),
    (0x01f11e, 0x01f11e, "(O)"),
    (0x01f11f, 0x01f11f, "(P)"),
    (0x01f120, 0x01f120, "(Q)"),
    (0x01f121, 0x01f121, "(R)"),
    (0x01f122, 0x01f122, "(S)"),
    (0x01f123, 0x01f123, "(T)"),
    (0x01f124, 0x01f124, "(U)"),
    (0x01f125, 0x01f125, "(V)"),
    (0x01f126, 0x01f126, "(W)"),
    (0x01f127, 0x01f127, "(X)"),
    (0x01f128, 0x01f128, "(Y)"),
    (0x01f129, 0x01f129, "(Z)"),
    (0x01f12b, 0x01f12b, "C"),
    (0x01f12c, 0x01f12c, "R"),
    (0x01f12d, 0x01f12d, "CD"),
    (0x01f12e, 0x01f12e, "WZ"),
    (0x01f130, 0x01f131, "A"),
    (0x01f132, 0x01f133, "C"),
    (0x01f134, 0x01f135, "E"),
    (0x01f136, 0x01f137, "G"),
    (0x01f138, 0x01f139, "I"),
    (0x01f13a, 0x01f13b, "K"),
    (0x01f13c, 0x01f13d, "M"),
    (0x01f13e, 0x01f13f, "O"),
    (0x01f140, 0x01f141, "Q"),
    (0x01f142, 0x01f143, "S"),
    (0x01f144, 0x01f145, "U"),
    (0x01f146, 0x01f147, "W"),
    (0x01f148, 0x01f149, "Y"),
    (0x01f14a, 0x01f14a, "HV"),
    (0x01f14b, 0x01f14b, "MV"),
    (0x01f14c, 0x01f14c, "SD"),
    (0x01f14d, 0x01f14d, "SS"),
    (0x01f14e, 0x01f14e, "PPV"),
    (0x01f14f, 0x01f14f, "WC"),
    (0x01f16a, 0x01f16b, "MC"),
    (0x01f16c, 0x01f16c, "MR"),
    (0x01f190, 0x01f190, "DJ"),
    (0x01fbf0, 0x01fbf1, "0"),
    (0x01fbf2, 0x01fbf3, "2"),
    (0x01fbf4, 0x01fbf5, "4"),
    (0x01fbf6, 0x01fbf7, "6"),
    (0x01fbf8, 0x01fbf9, "8"),
];

/// `os.path.expanduser` for POSIX.
fn expanduser(path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_string();
    }
    let rest = &path[1..];
    let (user, tail) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let home = if user.is_empty() {
        match env::var_os("HOME") {
            Some(home) => Some(PathBuf::from(home)),
            None => passwd_field(PasswdQuery::Uid(current_uid())),
        }
    } else {
        passwd_field(PasswdQuery::Name(user.to_string()))
    };
    match home {
        Some(home) => {
            let mut expanded = home.to_string_lossy().into_owned();
            expanded.push_str(tail);
            expanded
        }
        None => path.to_string(),
    }
}

fn current_uid() -> String {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                let value = line.strip_prefix("Uid:")?;
                value.split_whitespace().next().map(str::to_string)
            })
        })
        .unwrap_or_default()
}

/// `os.path.expandvars` for POSIX (CPython 3.13): `$NAME`/`${NAME}` are
/// replaced only when the variable exists; unset names and malformed
/// references stay verbatim.
fn expandvars(path: &str) -> String {
    let characters: Vec<char> = path.chars().collect();
    let mut out = String::with_capacity(path.len());
    let mut index = 0;
    while index < characters.len() {
        if characters[index] != '$' {
            out.push(characters[index]);
            index += 1;
            continue;
        }
        let (name, end) = if characters.get(index + 1) == Some(&'{') {
            let Some(close) = (index + 2..characters.len()).find(|&i| characters[i] == '}') else {
                out.push('$');
                index += 1;
                continue;
            };
            (
                characters[index + 2..close].iter().collect::<String>(),
                close + 1,
            )
        } else {
            let mut end = index + 1;
            while end < characters.len() && is_word_character(characters[end]) {
                end += 1;
            }
            if end == index + 1 {
                out.push('$');
                index += 1;
                continue;
            }
            (characters[index + 1..end].iter().collect::<String>(), end)
        };
        match env::var_os(&name) {
            Some(value) => {
                out.push_str(&value.to_string_lossy());
                index = end;
            }
            None => {
                for character in &characters[index..end] {
                    out.push(*character);
                }
                index = end;
            }
        }
    }
    out
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// `pathlib.Path.resolve(strict=False)` for POSIX: resolve symlinks for the
/// existing prefix and normalize `.`/`..`/`//` lexically, resolving relative
/// paths against the current directory.
fn resolve_non_strict(path: &Path) -> PathBuf {
    // Components are queued as owned paths ("/", ".", "..", or a name) so the
    // queue can outlive the borrowed inputs.
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    let mut enqueue = |source: &Path| {
        for component in source.components() {
            queue.push_back(PathBuf::from(component.as_os_str()));
        }
    };
    if path.is_absolute() {
        enqueue(path);
    } else {
        if let Ok(current_directory) = env::current_dir() {
            enqueue(&current_directory);
        }
        enqueue(path);
    }
    let mut resolved = PathBuf::new();
    let mut symlink_hops = 0;
    while let Some(component) = queue.pop_front() {
        if component == Path::new("/") {
            resolved = PathBuf::from(std::path::MAIN_SEPARATOR_STR);
        } else if component == Path::new(".") {
            // skip
        } else if component == Path::new("..") {
            resolved.pop();
        } else {
            resolved.push(&component);
            if symlink_hops > 40 {
                continue;
            }
            if let Ok(metadata) = fs::symlink_metadata(&resolved) {
                if metadata.file_type().is_symlink() {
                    symlink_hops += 1;
                    if let Ok(target) = fs::read_link(&resolved) {
                        resolved.pop();
                        for part in target.components().rev() {
                            queue.push_front(PathBuf::from(part.as_os_str()));
                        }
                    }
                }
            }
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfkc_maps_compatibility_characters_to_ascii() {
        assert_eq!(normalize_space_name("Ｗｏｒｋ-２").unwrap(), "work-2");
        assert_eq!(normalize_space_name("ﬁle").unwrap(), "file");
        assert_eq!(normalize_space_name("ⅵ").unwrap(), "vi");
    }

    #[test]
    fn non_ascii_whitespace_is_stripped_like_python() {
        assert_eq!(normalize_space_name("\u{2028}a\u{2029}").unwrap(), "a");
        assert_eq!(
            normalize_space_name("\u{2028}").unwrap_err(),
            MemoroError::Configuration(format!("Space name is empty. {SPACE_NAME_GUIDANCE}"))
        );
    }

    #[test]
    fn py_repr_matches_python_string_formatting() {
        assert_eq!(py_repr("名字"), "'名字'");
        assert_eq!(py_repr("a'b"), "\"a'b\"");
        assert_eq!(py_repr("a\"b"), "'a\"b'");
        assert_eq!(py_repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(py_repr("a\tb"), "'a\\tb'");
        assert_eq!(py_repr("a\u{a0}b"), "'a\\xa0b'");
        assert_eq!(py_repr("\u{2028}"), "'\\u2028'");
        assert_eq!(py_repr("\u{e000}"), "'\\ue000'");
        assert_eq!(py_repr("\u{10ffff}"), "'\\U0010ffff'");
    }

    #[test]
    fn expandvars_only_replaces_known_variables() {
        env::set_var("MEMORO_EXPANDVARS_PROBE", "value");
        assert_eq!(expandvars("$MEMORO_EXPANDVARS_PROBE"), "value");
        assert_eq!(expandvars("${MEMORO_EXPANDVARS_PROBE}x"), "valuex");
        assert_eq!(expandvars("$MEMORO_UNSET_VAR"), "$MEMORO_UNSET_VAR");
        assert_eq!(expandvars("$$"), "$$");
        assert_eq!(expandvars("${"), "${");
        env::remove_var("MEMORO_EXPANDVARS_PROBE");
    }

    #[test]
    fn resolve_non_strict_normalizes_lexically() {
        assert_eq!(
            resolve_non_strict(Path::new("/tmp/../a/./b//c")),
            PathBuf::from("/a/b/c")
        );
    }
}
