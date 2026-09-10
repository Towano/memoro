//! Mirrors `python/tests/test_store.py` case by case.
//!
//! The Python suite injects fixed clocks/IDs through constructor callables and
//! simulates Git failures with monkeypatching. The Rust port injects the same
//! clocks/IDs through `with_clock`/`with_id_factory`, simulates the failing
//! `git commit` with `with_commit_hook`, verifies snapshot caching by removing
//! the memory blob object (a re-read would fail while the cache serves), and
//! replaces the caplog assertion with a captured child-process probe.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};

use memoro::errors::MemoroError;
use memoro::git::GitRepository;
use memoro::markdown::{memory_revision, serialize_memory};
use memoro::models::{Memory, PatchEdit};
use memoro::store::MemoryStore;

const FIRST_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const SECOND_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const THIRD_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";

fn space_repository(root: &Path) -> GitRepository {
    GitRepository::initialize(&root.join("space")).expect("initialize repository")
}

/// A second handle onto the same repository, for assertions after a store has
/// taken ownership of the first one.
fn store_handle(repository: &GitRepository) -> GitRepository {
    GitRepository::open(repository.path()).expect("open repository handle")
}

fn moment(hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 22, hour, minute, second)
        .single()
        .expect("valid timestamp")
}

fn first_time() -> DateTime<Utc> {
    moment(1, 2, 3)
}

fn second_time() -> DateTime<Utc> {
    moment(2, 3, 4)
}

fn third_time() -> DateTime<Utc> {
    moment(3, 4, 5)
}

fn store_with(repository: GitRepository, times: &[DateTime<Utc>], ids: &[&str]) -> MemoryStore {
    let remaining_times = Arc::new(Mutex::new(times.to_vec()));
    let remaining_ids = Arc::new(Mutex::new(
        ids.iter().map(|id| (*id).to_string()).collect::<Vec<_>>(),
    ));
    MemoryStore::new(repository)
        .with_clock(Box::new(move || {
            remaining_times.lock().expect("time queue").remove(0)
        }))
        .with_id_factory(Box::new(move || {
            remaining_ids.lock().expect("id queue").remove(0)
        }))
}

fn store(repository: GitRepository) -> MemoryStore {
    store_with(repository, &[first_time()], &[FIRST_ID])
}

/// A store whose every commit fails, mirroring the monkeypatched
/// `fail_commit` in the Python suite.
fn failing_store(repository: GitRepository, message: &str) -> MemoryStore {
    let message = message.to_string();
    store_with(repository, &[first_time()], &[FIRST_ID]).with_commit_hook(Box::new(
        move |_message, _relative_path| Err(MemoroError::Repository(message.clone())),
    ))
}

fn tags(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn edit(old_text: &str, new_text: &str) -> PatchEdit {
    PatchEdit {
        old_text: old_text.to_string(),
        new_text: new_text.to_string(),
    }
}

fn revision_of(memory: &Memory) -> String {
    memory_revision(memory).expect("memory revision")
}

fn commit_document(repository: &GitRepository, memory: &Memory) {
    let target = repository
        .worktree_path(&memory.relative_path)
        .expect("worktree path");
    fs::create_dir_all(target.parent().expect("parent directory")).expect("create directory");
    let document = serialize_memory(memory).expect("serialize memory");
    fs::write(&target, document).expect("write document");
    repository
        .stage(&memory.relative_path)
        .expect("stage document");
    repository
        .commit("test: add conflicting fixture", &memory.relative_path)
        .expect("commit document");
}

fn commit_subjects(repository: &GitRepository) -> Vec<String> {
    let repo = git2::Repository::open(repository.path()).expect("open repository");
    let mut commit = repo.head().expect("head").peel_to_commit().expect("commit");
    let mut subjects = Vec::new();
    loop {
        subjects.push(
            commit
                .summary()
                .ok()
                .flatten()
                .unwrap_or_default()
                .to_string(),
        );
        match commit.parent(0) {
            Ok(parent) => commit = parent,
            Err(_) => break,
        }
    }
    subjects
}

fn assert_error_message(error: &MemoroError, pattern: &str) {
    assert!(
        error.to_string().contains(pattern),
        "expected {error:?} to contain {pattern:?}"
    );
}

#[test]
fn test_create_then_replace_preserves_identity_and_creation_time() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store_with(repository, &[first_time(), second_time()], &[FIRST_ID]);

    let created = store
        .create(
            "  Release   notes ",
            "Release note policy.",
            "first\r\nbody",
            "project",
            Some("Memoro"),
            &[],
        )
        .expect("create memory");
    let updated = store
        .replace(
            &created.memory.id,
            &revision_of(&created.memory),
            "Updated release note policy.",
            "updated body",
            &[],
        )
        .expect("replace memory");

    assert_eq!(created.operation, "create");
    assert_eq!(created.memory.kind, "project");
    assert_eq!(created.memory.project.as_deref(), Some("memoro"));
    assert_eq!(
        created.memory.relative_path,
        format!("projects/memoro/{FIRST_ID}.md")
    );
    assert_eq!(updated.operation, "replace");
    assert_eq!(updated.memory.id, created.memory.id);
    assert_eq!(updated.memory.id, FIRST_ID);
    assert_eq!(updated.memory.relative_path, created.memory.relative_path);
    assert_eq!(updated.memory.created_at, created.memory.created_at);
    assert!(updated.memory.updated_at > created.memory.updated_at);
    assert_eq!(updated.memory.body, "updated body");
    assert_eq!(updated.previous_commit, Some(created.commit.clone()));
    assert_ne!(updated.commit, created.commit);
    assert_eq!(
        store.snapshot().expect("snapshot").memories,
        vec![updated.memory.clone()]
    );
}

#[test]
fn test_same_title_in_different_kinds_creates_distinct_memories() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store_with(
        repository,
        &[first_time(), second_time(), third_time()],
        &[FIRST_ID, SECOND_ID, THIRD_ID],
    );

    let persona = store
        .create("Fact", "A persona fact.", "persona", "persona", None, &[])
        .expect("persona memory");
    let project = store
        .create(
            "fact",
            "A project fact.",
            "project",
            "project",
            Some("memoro"),
            &[],
        )
        .expect("project memory");
    let playbook = store
        .create(
            "FACT",
            "A playbook fact.",
            "playbook",
            "playbook",
            None,
            &[],
        )
        .expect("playbook memory");

    let identifiers = HashSet::from([
        persona.memory.id.clone(),
        project.memory.id.clone(),
        playbook.memory.id.clone(),
    ]);
    assert_eq!(identifiers.len(), 3);
    let locations: HashSet<(String, Option<String>)> = store
        .snapshot()
        .expect("snapshot")
        .memories
        .into_iter()
        .map(|memory| (memory.kind, memory.project))
        .collect();
    assert_eq!(
        locations,
        HashSet::from([
            ("persona".to_string(), None),
            ("project".to_string(), Some("memoro".to_string())),
            ("playbook".to_string(), None),
        ])
    );
}

#[test]
fn test_same_title_in_different_project_slugs_creates_distinct_memories() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store_with(
        repository,
        &[first_time(), second_time()],
        &[FIRST_ID, SECOND_ID],
    );

    let first = store
        .create(
            "Fact",
            "A fact for app-one.",
            "one",
            "project",
            Some("app-one"),
            &[],
        )
        .expect("first memory");
    let second = store
        .create(
            "fact",
            "A fact for app-two.",
            "two",
            "project",
            Some("app-two"),
            &[],
        )
        .expect("second memory");

    assert_ne!(first.memory.id, second.memory.id);
    let error = store
        .create(
            "FACT",
            "A conflicting fact.",
            "three",
            "project",
            Some("app-one"),
            &[],
        )
        .expect_err("conflicting title");
    assert!(matches!(error, MemoroError::MemoryConflict(_)));
    assert_error_message(&error, "use patch or replace");
}

#[test]
fn test_create_validates_kind_and_project_pairing() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));

    let error = store
        .create("Fact", "A fact.", "body", "global", None, &[])
        .expect_err("invalid kind");
    assert!(matches!(error, MemoroError::MemoryValidation(_)));
    assert_error_message(&error, "persona, project, or playbook");
    let error = store
        .create("Fact", "A fact.", "body", "project", None, &[])
        .expect_err("project without slug");
    assert_error_message(&error, "requires a project slug");
    let error = store
        .create("Fact", "A fact.", "body", "persona", Some("memoro"), &[])
        .expect_err("persona with slug");
    assert_error_message(&error, "does not take a project slug");
    let error = store
        .create("Fact", "A fact.", "body", "playbook", Some("memoro"), &[])
        .expect_err("playbook with slug");
    assert_error_message(&error, "does not take a project slug");
    assert_eq!(repository.head().expect("head"), None);
}

#[test]
fn test_create_and_replace_store_canonical_tags() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store_with(
        store_handle(&repository),
        &[first_time(), second_time()],
        &[FIRST_ID],
    );

    let created = store
        .create(
            "Fact",
            "A tagged fact.",
            "body",
            "persona",
            None,
            &tags(&["Zeta", " alpha ", "ALPHA"]),
        )
        .expect("create memory");

    assert_eq!(created.memory.tags, tags(&["alpha", "Zeta"]));
    assert_eq!(
        store.snapshot().expect("snapshot").memories[0].tags,
        tags(&["alpha", "Zeta"])
    );

    let replaced = store
        .replace(
            &created.memory.id,
            &revision_of(&created.memory),
            "A tagged fact.",
            "body",
            &tags(&["beta"]),
        )
        .expect("replace memory");

    assert!(replaced.changed);
    assert_eq!(replaced.memory.tags, tags(&["beta"]));
    assert_eq!(
        store.snapshot().expect("snapshot").memories[0].tags,
        tags(&["beta"])
    );
}

#[test]
fn test_create_rejects_invalid_tags() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));

    let error = store
        .create(
            "Fact",
            "A fact.",
            "body",
            "persona",
            None,
            &tags(&["bad\ntag"]),
        )
        .expect_err("control character tag");
    assert!(matches!(error, MemoroError::MemoryValidation(_)));
    assert_error_message(&error, "tags are invalid");
    let nine_tags: Vec<String> = (0..9).map(|index| format!("tag-{index}")).collect();
    let error = store
        .create("Fact", "A fact.", "body", "persona", None, &nine_tags)
        .expect_err("too many tags");
    assert_error_message(&error, "tags are invalid");
    assert_eq!(repository.head().expect("head"), None);
}

#[test]
fn test_patch_keeps_existing_tags() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store_with(
        store_handle(&repository),
        &[first_time(), second_time()],
        &[FIRST_ID],
    );
    let created = store
        .create(
            "Fact",
            "A tagged fact.",
            "Alpha rule.",
            "persona",
            None,
            &tags(&["keep-me"]),
        )
        .expect("create memory");

    let patched = store
        .patch(
            &created.memory.id,
            &revision_of(&created.memory),
            &[edit("Alpha", "Beta")],
            None,
        )
        .expect("patch memory");

    assert_eq!(patched.memory.tags, tags(&["keep-me"]));
}

#[test]
fn test_commit_messages_label_each_kind() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store_with(
        store_handle(&repository),
        &[first_time(), second_time(), third_time()],
        &[FIRST_ID, SECOND_ID, THIRD_ID],
    );

    store
        .create("One", "A persona fact.", "one", "persona", None, &[])
        .expect("persona memory");
    store
        .create(
            "Two",
            "A project fact.",
            "two",
            "project",
            Some("memoro"),
            &[],
        )
        .expect("project memory");
    store
        .create("Three", "A playbook fact.", "three", "playbook", None, &[])
        .expect("playbook memory");

    assert_eq!(
        commit_subjects(&repository),
        vec![
            "memory(playbook): create \"Three\"",
            "memory(project/memoro): create \"Two\"",
            "memory(persona): create \"One\"",
        ]
    );
}

#[test]
fn test_snapshot_reuses_validated_snapshot_while_head_is_unchanged() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));
    store
        .create(
            "Fact",
            "A cached fact.",
            "cached body",
            "persona",
            None,
            &[],
        )
        .expect("create memory");
    let cached_store = MemoryStore::new(store_handle(&repository));

    let first = cached_store.snapshot().expect("first snapshot");
    // The Python suite counts `memory_paths_at_commit`/`read_at_commit` calls;
    // the equivalent observable proof is deleting the memory blob object from
    // the object store: a snapshot that re-read the commit would now fail,
    // while the cached snapshot still loads.
    let commit = first.commit.clone().expect("commit id");
    let content = repository
        .read_at_commit(&commit, &first.memories[0].relative_path)
        .expect("read committed memory");
    let blob_id = git2::Oid::hash_object(git2::ObjectType::Blob, content.as_bytes())
        .expect("hash memory blob");
    let hex = blob_id.to_string();
    let object_path = repository
        .path()
        .join(".git/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    fs::remove_file(&object_path).expect("remove memory blob object");

    let second = cached_store.snapshot().expect("cached second snapshot");
    assert_eq!(second, first);
}

#[test]
fn test_snapshot_reloads_after_external_commit() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));
    let created = store
        .create("Fact", "A cached fact.", "first body", "persona", None, &[])
        .expect("create memory");

    let first = store.snapshot().expect("first snapshot");
    let mut externally_updated = created.memory.clone();
    externally_updated.body = "externally updated body".to_string();
    externally_updated.updated_at = "2026-08-22T03:04:05Z".to_string();

    commit_document(&repository, &externally_updated);
    let second = store.snapshot().expect("reloaded snapshot");

    assert_ne!(second, first);
    assert_ne!(second.commit, first.commit);
    assert_eq!(second.memories, vec![externally_updated]);
}

#[test]
fn test_snapshot_rejects_duplicate_ids() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));
    let created = store
        .create("First", "The first fact.", "one", "persona", None, &[])
        .expect("create memory");

    let mut duplicate = created.memory.clone();
    duplicate.title = "Second".to_string();
    duplicate.kind = "project".to_string();
    duplicate.project = Some("memoro".to_string());
    duplicate.relative_path = format!("projects/memoro/{}.md", created.memory.id);
    commit_document(&repository, &duplicate);

    let error = store.snapshot().expect_err("duplicate memory ID");
    assert!(matches!(error, MemoroError::MemoryIntegrity(_)));
    assert_error_message(&error, "appears in both");
}

#[test]
fn test_snapshot_rejects_duplicate_normalized_titles() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));
    let created = store
        .create("Straße", "A normalized fact.", "one", "persona", None, &[])
        .expect("create memory");

    let mut duplicate = created.memory.clone();
    duplicate.id = SECOND_ID.to_string();
    duplicate.title = "STRASSE".to_string();
    duplicate.relative_path = format!("persona/{SECOND_ID}.md");
    commit_document(&repository, &duplicate);

    let error = store.snapshot().expect_err("duplicate normalized title");
    assert!(matches!(error, MemoroError::MemoryIntegrity(_)));
    assert_error_message(&error, "duplicate normalized titles");
}

#[test]
fn test_atomic_replace_preserves_old_file_when_replace_fails() {
    let directory = tempfile::tempdir().expect("temp dir");
    // The Python suite monkeypatches `os.replace`; the Rust equivalent is a
    // filename whose generated temporary sibling exceeds the filesystem's
    // 255-byte component limit, so the atomic swap itself fails.
    let file_name = format!("{}.md", "m".repeat(250));
    let target = directory.path().join(file_name);
    fs::write(&target, b"old bytes").expect("write old bytes");

    let error =
        memoro::filesystem::atomic_replace(&target, b"new bytes").expect_err("replace fails");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidFilename);
    assert_eq!(fs::read(&target).expect("read target"), b"old bytes");
    let leftovers: Vec<String> = fs::read_dir(directory.path())
        .expect("list directory")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with('.') && name.ends_with(".tmp"))
        .collect();
    assert_eq!(leftovers, Vec::<String>::new());
}

#[test]
fn test_commit_failure_rolls_back_updated_file_and_git_index() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store_with(
        store_handle(&repository),
        &[first_time(), second_time()],
        &[FIRST_ID],
    );
    let created = store
        .create(
            "Fact",
            "A committed fact.",
            "committed body",
            "persona",
            None,
            &[],
        )
        .expect("create memory");
    let target = repository
        .worktree_path(&created.memory.relative_path)
        .expect("worktree path");
    let previous_bytes = fs::read(&target).expect("read target");
    let previous_head = repository.head().expect("head");

    let failing = failing_store(store_handle(&repository), "simulated commit failure");
    let error = failing
        .replace(
            &created.memory.id,
            &revision_of(&created.memory),
            "A committed fact.",
            "uncommitted body",
            &[],
        )
        .expect_err("replace fails");

    assert!(matches!(error, MemoroError::Repository(_)));
    assert_error_message(&error, "simulated commit failure");
    assert_eq!(fs::read(&target).expect("read target"), previous_bytes);
    assert_eq!(repository.head().expect("head"), previous_head);
    assert_eq!(
        repository.staged_paths().expect("staged paths"),
        Vec::<String>::new()
    );
    assert!(repository.assert_clean().is_ok());
    assert_eq!(
        store.snapshot().expect("snapshot").memories,
        vec![created.memory]
    );
}

#[test]
fn test_commit_failure_removes_a_new_memory_file() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let failing = failing_store(store_handle(&repository), "simulated commit failure");

    let error = failing
        .create("Fact", "A fact.", "body", "persona", None, &[])
        .expect_err("create fails");

    assert_error_message(&error, "simulated commit failure");
    assert_eq!(repository.head().expect("head"), None);
    assert_eq!(
        repository.staged_paths().expect("staged paths"),
        Vec::<String>::new()
    );
    assert!(!repository
        .worktree_path(&format!("persona/{FIRST_ID}.md"))
        .expect("worktree path")
        .exists());
    assert!(repository.assert_clean().is_ok());
}

#[test]
fn test_dirty_repository_refuses_write_but_snapshot_reads_committed_head() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store_with(
        store_handle(&repository),
        &[first_time(), second_time()],
        &[FIRST_ID],
    );
    let created = store
        .create(
            "Fact",
            "A committed fact.",
            "committed body",
            "persona",
            None,
            &[],
        )
        .expect("create memory");
    let target = repository
        .worktree_path(&created.memory.relative_path)
        .expect("worktree path");
    fs::write(&target, "uncommitted edit").expect("write uncommitted edit");

    let snapshot = store.snapshot().expect("committed snapshot");

    assert_eq!(snapshot.memories, vec![created.memory.clone()]);
    let error = store
        .replace(
            &created.memory.id,
            &revision_of(&created.memory),
            "A committed fact.",
            "replacement",
            &[],
        )
        .expect_err("dirty repository");
    assert!(matches!(error, MemoroError::RepositoryDirty(_)));
    assert_error_message(&error, "uncommitted changes");
    assert_eq!(
        fs::read_to_string(&target).expect("read target"),
        "uncommitted edit"
    );
}

#[test]
fn test_storage_does_not_log_memory_body() {
    let secret_body = "PRIVATE-BODY-SENTINEL";
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "test_storage_does_not_log_memory_body_probe",
            "--nocapture",
        ])
        .env("MEMORO_LOG_PROBE", "1")
        .output()
        .expect("spawn probe process");

    assert!(output.status.success(), "probe failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(secret_body),
        "body leaked to stdout: {stdout}"
    );
    assert!(
        !stderr.contains(secret_body),
        "body leaked to stderr: {stderr}"
    );
}

// Runs inside the probe process above; the Python equivalent asserts caplog
// never captures the body, and the Rust crate has no logging framework whose
// output could be intercepted in-process.
#[test]
fn test_storage_does_not_log_memory_body_probe() {
    if std::env::var("MEMORO_LOG_PROBE").is_err() {
        return;
    }
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    store(store_handle(&repository))
        .create(
            "Fact",
            "A private fact.",
            "PRIVATE-BODY-SENTINEL",
            "persona",
            None,
            &[],
        )
        .expect("create memory");
}

#[test]
fn test_create_is_idempotent_only_when_existing_content_matches() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));
    let created = store
        .create("Fact", "A fact.", "body", "persona", None, &[])
        .expect("create memory");

    let repeated = store
        .create("fact", "A fact.", "body", "persona", None, &[])
        .expect("idempotent create");

    assert!(!repeated.changed);
    assert_eq!(repeated.memory, created.memory);
    assert_eq!(repeated.commit, created.commit);
    let error = store
        .create("FACT", "A fact.", "different", "persona", None, &[])
        .expect_err("conflicting body");
    assert!(matches!(error, MemoroError::MemoryConflict(_)));
    assert_error_message(&error, "use patch or replace");
    let error = store
        .create(
            "FACT",
            "A fact.",
            "body",
            "persona",
            None,
            &tags(&["extra"]),
        )
        .expect_err("conflicting tags");
    assert_error_message(&error, "use patch or replace");
    assert_eq!(commit_subjects(&repository).len(), 1);
}

#[test]
fn test_create_rejects_a_generated_duplicate_id() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store_with(
        store_handle(&repository),
        &[first_time(), second_time()],
        &[FIRST_ID, FIRST_ID],
    );
    store
        .create("First", "The first memory.", "one", "persona", None, &[])
        .expect("create memory");

    let error = store
        .create("Second", "The second memory.", "two", "persona", None, &[])
        .expect_err("duplicate generated ID");

    assert!(matches!(error, MemoroError::MemoryIntegrity(_)));
    assert_error_message(&error, "already exists");
}

#[test]
fn test_patch_applies_exact_non_overlapping_edits_and_rejects_stale_revision() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store_with(
        store_handle(&repository),
        &[first_time(), second_time()],
        &[FIRST_ID],
    );
    let created = store
        .create(
            "Policy",
            "Rules governing the policy.",
            "Alpha rule.\nBeta rule.",
            "persona",
            None,
            &[],
        )
        .expect("create memory");

    let patched = store
        .patch(
            &created.memory.id,
            &revision_of(&created.memory),
            &[edit("Alpha", "Current alpha"), edit("Beta", "Current beta")],
            None,
        )
        .expect("patch memory");

    assert_eq!(
        patched.memory.body,
        "Current alpha rule.\nCurrent beta rule."
    );
    assert_eq!(patched.memory.id, created.memory.id);
    let error = store
        .patch(
            &created.memory.id,
            &revision_of(&created.memory),
            &[edit("Current alpha", "Other")],
            None,
        )
        .expect_err("stale revision");
    assert!(matches!(error, MemoroError::MemoryConflict(_)));
    assert_error_message(&error, "changed after it was read");
}

#[test]
fn test_patch_rejects_missing_ambiguous_and_overlapping_anchors() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));
    let created = store
        .create(
            "Policy",
            "Rules governing the policy.",
            "repeat repeat and tail",
            "persona",
            None,
            &[],
        )
        .expect("create memory");
    let revision = revision_of(&created.memory);

    let error = store
        .patch(
            &created.memory.id,
            &revision,
            &[edit("missing", "new")],
            None,
        )
        .expect_err("missing anchor");
    assert!(matches!(error, MemoroError::MemoryConflict(_)));
    assert_error_message(&error, "could not find");
    let error = store
        .patch(
            &created.memory.id,
            &revision,
            &[edit("repeat", "new")],
            None,
        )
        .expect_err("ambiguous anchor");
    assert_error_message(&error, "more than once");
    let error = store
        .patch(
            &created.memory.id,
            &revision,
            &[edit("repeat repeat", "new"), edit("repeat and", "other")],
            None,
        )
        .expect_err("overlapping anchors");
    assert_error_message(&error, "overlap");
}

#[test]
fn test_patch_validation_and_noop_paths_are_explicit() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));
    let created = store
        .create(
            "Policy",
            "Rules governing the policy.",
            "Alpha rule.",
            "persona",
            None,
            &[],
        )
        .expect("create memory");
    let revision = revision_of(&created.memory);

    let unchanged = store
        .patch(
            &created.memory.id,
            &revision,
            &[edit("Alpha", "Alpha")],
            None,
        )
        .expect("unchanged patch");
    assert!(!unchanged.changed);

    let unchanged_replace = store
        .replace(
            &created.memory.id,
            &revision,
            &created.memory.summary,
            &created.memory.body,
            &created.memory.tags,
        )
        .expect("unchanged replace");
    assert!(!unchanged_replace.changed);

    let error = store
        .patch(SECOND_ID, &revision, &[edit("Alpha", "Beta")], None)
        .expect_err("unknown memory");
    assert!(matches!(error, MemoroError::MemoryNotFound(_)));
    assert_error_message(&error, "was not found");
    let error = store
        .patch(&created.memory.id, &revision, &[], None)
        .expect_err("empty edits");
    assert_error_message(&error, "at least one exact edit");
    let error = store
        .patch(&created.memory.id, &revision, &[edit("", "Beta")], None)
        .expect_err("empty anchor");
    assert_error_message(&error, "old_text is empty");
    let error = store
        .patch(
            &created.memory.id,
            &revision,
            &[edit("Alpha", "bad\0text")],
            None,
        )
        .expect_err("control character");
    assert_error_message(&error, "control character");
    let error = store
        .patch(
            &created.memory.id,
            &revision,
            &[edit("Alpha rule.", "\nBeta rule.\n")],
            None,
        )
        .expect_err("non-canonical result");
    assert_error_message(&error, "not canonical");
}

#[test]
fn test_delete_requires_title_and_revision_and_commits_one_file() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));
    let created = store
        .create(
            "Fact",
            "A project fact.",
            "body",
            "project",
            Some("memoro"),
            &[],
        )
        .expect("create memory");
    let revision = revision_of(&created.memory);

    let error = store
        .delete(&created.memory.id, "Other", &revision)
        .expect_err("wrong title");
    assert!(matches!(error, MemoroError::MemoryConflict(_)));
    assert_error_message(&error, "is titled");
    let deleted = store
        .delete(&created.memory.id, "Fact", &revision)
        .expect("delete memory");

    assert_eq!(deleted.operation, "delete");
    assert!(deleted.changed);
    assert_eq!(
        store.snapshot().expect("snapshot").memories,
        Vec::<Memory>::new()
    );
    assert_eq!(
        repository
            .commit_paths(&deleted.commit)
            .expect("commit paths"),
        vec![created.memory.relative_path.clone()]
    );
    assert!(!repository
        .worktree_path(&created.memory.relative_path)
        .expect("worktree path")
        .exists());
}

#[test]
fn test_delete_commit_failure_restores_file_and_git_index() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let store = store(store_handle(&repository));
    let created = store
        .create("Fact", "A recoverable fact.", "body", "persona", None, &[])
        .expect("create memory");
    let target = repository
        .worktree_path(&created.memory.relative_path)
        .expect("worktree path");
    let previous_bytes = fs::read(&target).expect("read target");
    let previous_head = repository.head().expect("head");

    let failing = failing_store(store_handle(&repository), "simulated delete commit failure");
    let error = failing
        .delete(
            &created.memory.id,
            &created.memory.title,
            &revision_of(&created.memory),
        )
        .expect_err("delete fails");

    assert_error_message(&error, "simulated delete commit failure");
    assert_eq!(fs::read(&target).expect("read target"), previous_bytes);
    assert_eq!(repository.head().expect("head"), previous_head);
    assert_eq!(
        repository.staged_paths().expect("staged paths"),
        Vec::<String>::new()
    );
    assert_eq!(
        store.snapshot().expect("snapshot").memories,
        vec![created.memory]
    );
}
