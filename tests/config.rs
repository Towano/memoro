//! Mirrors `python/tests/test_config.py` case by case; error messages are
//! asserted verbatim where Python's tests match on wording.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use memoro::config::{
    load_spaces, normalize_space_name, resolve_home, save_spaces, RuntimePaths, SpaceSettings,
    DEFAULT_SPACE, LOCAL_CONFIG_NAME, SPACE_NAME_GUIDANCE,
};
use memoro::errors::MemoroError;

fn environment(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// `Path.resolve(strict=False)` for fixtures below an existing tempdir.
fn resolved_under(existing: &Path, name: &str) -> PathBuf {
    fs::canonicalize(existing).unwrap().join(name)
}

#[test]
fn home_flag_takes_priority_over_environment() {
    let temporary = tempfile::tempdir().unwrap();
    let cli_home = temporary.path().join("from-cli");
    let env_home = temporary.path().join("from-environment");
    let env = environment(&[("MEMORO_HOME", env_home.to_str().unwrap())]);

    assert_eq!(
        resolve_home(Some(cli_home.to_str().unwrap()), Some(&env)).unwrap(),
        resolved_under(temporary.path(), "from-cli")
    );
}

#[test]
fn home_uses_environment_then_default() {
    let temporary = tempfile::tempdir().unwrap();
    let env_home = temporary.path().join("from-environment");
    let env = environment(&[("MEMORO_HOME", env_home.to_str().unwrap())]);
    let empty = environment(&[]);

    assert_eq!(
        resolve_home(None, Some(&env)).unwrap(),
        resolved_under(temporary.path(), "from-environment")
    );

    let mut expected = fs::canonicalize(std::env::var("HOME").unwrap())
        .unwrap()
        .join(".memoro");
    if let Ok(real) = fs::canonicalize(&expected) {
        expected = real;
    }
    assert_eq!(resolve_home(None, Some(&empty)).unwrap(), expected);
    assert_eq!(
        resolve_home(None, Some(&empty)).unwrap(),
        default_home_resolved()
    );
}

/// Independent recomputation of `DEFAULT_HOME.expanduser().resolve(strict=False)`.
fn default_home_resolved() -> PathBuf {
    let home = fs::canonicalize(std::env::var("HOME").unwrap()).unwrap();
    let candidate = home.join(".memoro");
    fs::canonicalize(&candidate).unwrap_or(candidate)
}

#[test]
fn home_rejects_an_explicit_empty_value() {
    let cases = [
        (
            Some(""),
            environment(&[]),
            "--home is empty. Provide a directory for Memoro data.",
        ),
        (
            Some("   "),
            environment(&[("MEMORO_HOME", "ignored")]),
            "--home is empty. Provide a directory for Memoro data.",
        ),
        (
            None,
            environment(&[("MEMORO_HOME", "\t")]),
            "MEMORO_HOME is empty. Provide a directory for Memoro data.",
        ),
    ];
    for (cli_home, env, message) in cases {
        assert_eq!(
            resolve_home(cli_home, Some(&env)).unwrap_err(),
            MemoroError::Configuration(message.to_string())
        );
    }
}

#[test]
fn runtime_paths_locate_space_repositories() {
    let temporary = tempfile::tempdir().unwrap();
    let paths = RuntimePaths::new(temporary.path().to_path_buf());

    assert_eq!(paths.spaces(), temporary.path().join("spaces"));
    assert_eq!(
        paths.space("personal").unwrap(),
        temporary.path().join("spaces").join("personal")
    );
    assert_eq!(
        paths.space("  Work ").unwrap(),
        temporary.path().join("spaces").join("work")
    );
    assert_eq!(
        paths.space("../escape").unwrap_err(),
        MemoroError::Configuration(format!(
            "Space name '../escape' is invalid. {SPACE_NAME_GUIDANCE}"
        ))
    );
}

#[test]
fn space_names_are_normalized_and_validated() {
    assert_eq!(normalize_space_name("personal").unwrap(), "personal");
    assert_eq!(normalize_space_name("  Work-2 ").unwrap(), "work-2");

    assert_eq!(
        normalize_space_name("   ").unwrap_err(),
        MemoroError::Configuration(format!("Space name is empty. {SPACE_NAME_GUIDANCE}"))
    );
    assert_eq!(
        normalize_space_name("all").unwrap_err(),
        MemoroError::Configuration(
            "Space name 'all' is reserved. Choose a different name.".to_string()
        )
    );
    for invalid in [
        "a b",
        "a_b",
        "a/b",
        "a.b",
        "x".repeat(33).as_str(),
        "名字",
        "-a",
        "a-",
        "-a-",
        "--",
    ] {
        assert_eq!(
            normalize_space_name(invalid).unwrap_err(),
            MemoroError::Configuration(format!(
                "Space name '{}' is invalid. {SPACE_NAME_GUIDANCE}",
                invalid
            ))
        );
    }
}

#[test]
fn load_spaces_defaults_to_the_implicit_personal_space() {
    let registry = load_spaces(Path::new("/nonexistent-missing-home")).unwrap();

    assert_eq!(
        registry,
        BTreeMap::from([(DEFAULT_SPACE.to_string(), SpaceSettings { readonly: false })])
    );
}

#[test]
fn save_and_load_round_trip_merges_the_personal_space() {
    let home = tempfile::tempdir().unwrap();
    let registry = BTreeMap::from([("work".to_string(), SpaceSettings { readonly: true })]);

    let path = save_spaces(home.path(), &registry).unwrap();

    assert_eq!(path, home.path().join(LOCAL_CONFIG_NAME));
    assert_eq!(
        load_spaces(home.path()).unwrap(),
        BTreeMap::from([
            ("personal".to_string(), SpaceSettings { readonly: false }),
            ("work".to_string(), SpaceSettings { readonly: true }),
        ])
    );
}

#[test]
fn save_spaces_writes_canonical_json() {
    let home = tempfile::tempdir().unwrap();

    let path = save_spaces(
        home.path(),
        &BTreeMap::from([
            ("work".to_string(), SpaceSettings { readonly: true }),
            ("archive".to_string(), SpaceSettings { readonly: false }),
        ]),
    )
    .unwrap();
    let text = fs::read_to_string(&path).unwrap();

    let expected = concat!(
        "{\n",
        "  \"spaces\": {\n",
        "    \"archive\": {\n",
        "      \"readonly\": false\n",
        "    },\n",
        "    \"work\": {\n",
        "      \"readonly\": true\n",
        "    }\n",
        "  }\n",
        "}\n",
    );
    assert_eq!(text, expected);
}

#[test]
fn personal_space_can_never_be_readonly() {
    let home = tempfile::tempdir().unwrap();

    assert_eq!(
        save_spaces(
            home.path(),
            &BTreeMap::from([("personal".to_string(), SpaceSettings { readonly: true })])
        )
        .unwrap_err(),
        MemoroError::Configuration(
            "The default space 'personal' is always writable. Set readonly to false before \
             saving the space registry."
                .to_string()
        )
    );

    fs::write(
        home.path().join(LOCAL_CONFIG_NAME),
        "{\"spaces\": {\"personal\": {\"readonly\": true}}}",
    )
    .unwrap();
    assert_eq!(
        load_spaces(home.path()).unwrap_err(),
        MemoroError::Configuration(format!(
            "Memoro local configuration {} marks the default space 'personal' as readonly. The \
             default space is always writable; remove that entry or set readonly to false, then \
             retry.",
            home.path().join(LOCAL_CONFIG_NAME).display()
        ))
    );
}

#[test]
fn explicit_writable_personal_entry_is_accepted() {
    let home = tempfile::tempdir().unwrap();
    save_spaces(
        home.path(),
        &BTreeMap::from([("personal".to_string(), SpaceSettings { readonly: false })]),
    )
    .unwrap();

    assert_eq!(
        load_spaces(home.path()).unwrap(),
        BTreeMap::from([("personal".to_string(), SpaceSettings { readonly: false })])
    );
}

#[test]
fn save_spaces_rejects_invalid_names_and_settings() {
    let home = tempfile::tempdir().unwrap();

    assert_eq!(
        save_spaces(
            home.path(),
            &BTreeMap::from([("all".to_string(), SpaceSettings { readonly: false })])
        )
        .unwrap_err(),
        MemoroError::Configuration(
            "Space name 'all' is reserved. Choose a different name.".to_string()
        )
    );
    assert_eq!(
        save_spaces(
            home.path(),
            &BTreeMap::from([("Work".to_string(), SpaceSettings { readonly: false })])
        )
        .unwrap_err(),
        MemoroError::Configuration(
            "Space name 'Work' is not normalized. Use 'work' instead.".to_string()
        )
    );
    assert!(!home.path().join(LOCAL_CONFIG_NAME).exists());
}

#[test]
fn invalid_local_configuration_is_rejected() {
    let contents = [
        "not json",
        "[]",
        "{}",
        "{\"spaces\": {}, \"extra\": true}",
        "{\"spaces\": []}",
        "{\"spaces\": {\"Work\": {\"readonly\": false}}}",
        "{\"spaces\": {\"all\": {\"readonly\": false}}}",
        "{\"spaces\": {\"work\": {}}}",
        "{\"spaces\": {\"work\": {\"readonly\": \"no\"}}}",
        "{\"spaces\": {\"work\": {\"readonly\": false, \"extra\": 1}}}",
    ];
    for content in contents {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join(LOCAL_CONFIG_NAME), content).unwrap();

        let error = load_spaces(home.path()).unwrap_err();
        assert!(
            matches!(error, MemoroError::Configuration(_)),
            "content {content:?} should be rejected"
        );
    }
}

#[test]
fn invalid_local_configuration_error_messages_match_python() {
    type Expected = fn(&Path) -> String;
    let cases: &[(&str, Expected)] = &[
        ("not json", |path: &Path| {
            format!(
                "Memoro local configuration {} is invalid or unreadable. Repair or remove \
                     that file, then retry.",
                path.display()
            )
        }),
        ("[]", |path: &Path| {
            format!(
                "Memoro local configuration {} must contain exactly the spaces field. Repair \
                     or remove that file, then retry.",
                path.display()
            )
        }),
        ("{}", |path: &Path| {
            format!(
                "Memoro local configuration {} must contain exactly the spaces field. Repair \
                     or remove that file, then retry.",
                path.display()
            )
        }),
        ("{\"spaces\": []}", |path: &Path| {
            format!(
                "Memoro local configuration {} has an invalid spaces value. It must map \
                     space names to their settings. Repair or remove that file, then retry.",
                path.display()
            )
        }),
        (
            "{\"spaces\": {\"Work\": {\"readonly\": false}}}",
            |path: &Path| {
                format!(
                    "Memoro local configuration {} has a space name 'Work' that is not \
                     normalized. {SPACE_NAME_GUIDANCE}",
                    path.display()
                )
            },
        ),
        (
            "{\"spaces\": {\"all\": {\"readonly\": false}}}",
            |_path: &Path| "Space name 'all' is reserved. Choose a different name.".to_string(),
        ),
        ("{\"spaces\": {\"work\": {}}}", |path: &Path| {
            format!(
                "Memoro local configuration {} has invalid settings for space 'work'. Each \
                     space must contain exactly a boolean readonly field.",
                path.display()
            )
        }),
        (
            "{\"spaces\": {\"work\": {\"readonly\": \"no\"}}}",
            |path: &Path| {
                format!(
                    "Memoro local configuration {} has invalid settings for space 'work'. Each \
                     space must contain exactly a boolean readonly field.",
                    path.display()
                )
            },
        ),
    ];
    for (content, expected) in cases {
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join(LOCAL_CONFIG_NAME);
        fs::write(&config_path, content).unwrap();

        assert_eq!(
            load_spaces(home.path()).unwrap_err(),
            MemoroError::Configuration(expected(&config_path)),
            "content {content:?}"
        );
    }
}
