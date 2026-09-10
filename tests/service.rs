//! Integration tests for the locked multi-space Service boundary.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use git2::{Repository, RepositoryInitOptions};

use memoro::config::{save_spaces, RuntimePaths, SpaceSettings};
use memoro::errors::MemoroError;
use memoro::git::GitRepository;
use memoro::markdown::memory_revision;
use memoro::models::PatchEdit;
use memoro::service::{
    MemoryCreateRequest, MemoryDeleteRequest, MemoryGetRequest, MemoryListRequest,
    MemoryPatchRequest, MemoryReplaceRequest, MemorySearchRequest, Service, SyncRequest,
    DEFAULT_SYNC_REMOTE,
};
use memoro::sync::SyncAction;

fn create_request(title: &str, body: &str) -> MemoryCreateRequest {
    MemoryCreateRequest {
        title: title.to_string(),
        summary: format!("Summary for {title}."),
        body: body.to_string(),
        kind: "persona".to_string(),
        ..Default::default()
    }
}

fn service_with_spaces(home: &Path, spaces: &[(&str, bool)]) -> Service {
    let registry = spaces
        .iter()
        .map(|(name, readonly)| {
            (
                (*name).to_string(),
                SpaceSettings {
                    readonly: *readonly,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    save_spaces(home, &registry).expect("save space registry");
    Service::open(home.to_path_buf()).expect("open service")
}

fn space_path(home: &Path, name: &str) -> std::path::PathBuf {
    RuntimePaths::new(home.to_path_buf())
        .space(name)
        .expect("space path")
}

fn assert_clean(home: &Path, name: &str) {
    GitRepository::open(&space_path(home, name))
        .expect("open space repository")
        .assert_clean()
        .expect("clean repository");
}

fn init_bare(path: &Path) -> String {
    let mut options = RepositoryInitOptions::new();
    options.bare(true);
    Repository::init_opts(path, &options).expect("initialize bare remote");
    path.to_string_lossy().into_owned()
}

fn assert_readonly<T>(operation: &str, result: Result<T, MemoroError>) {
    let error = match result {
        Ok(_) => panic!("{operation} should be rejected"),
        Err(error) => error,
    };
    assert!(matches!(error, MemoroError::Repository(_)));
    assert!(error.to_string().contains("archive"));
    assert!(error.to_string().contains("readonly"));
}

#[test]
fn default_personal_space_is_normalized_and_lazily_initialized() {
    let temporary = tempfile::tempdir().expect("temp directory");
    let home = temporary.path().join("missing-home");
    let service = Service::open(home.clone()).expect("open missing home");

    let spaces = service.spaces_list().expect("list spaces");
    assert_eq!(spaces.len(), 1);
    assert_eq!(spaces[0].space, "personal");
    assert!(!spaces[0].readonly);
    assert!(!spaces[0].initialized);
    assert_eq!(spaces[0].head, None);
    assert!(service
        .memory_list(MemoryListRequest::default())
        .expect("list unopened personal")
        .memories
        .is_empty());
    assert!(matches!(
        service.memory_get(MemoryGetRequest {
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            ..Default::default()
        }),
        Err(MemoroError::MemoryNotFound(_))
    ));
    assert!(!space_path(&home, "personal").exists());
    assert!(home.join(".memoro-locks/personal/repository.lock").exists());

    let created = service
        .memory_create(create_request("Personal fact", "first committed body"))
        .expect("create default personal memory");

    assert_eq!(created.space, "personal");
    assert!(space_path(&home, "personal").join(".git").is_dir());
    assert!(!space_path(&home, "personal").join(".memoro-locks").exists());
    let normalized = service
        .memory_get(MemoryGetRequest {
            space: Some(" Personal ".to_string()),
            id: created.receipt.memory.id,
        })
        .expect("get normalized space");
    assert_eq!(normalized.space, "personal");
    assert_clean(&home, "personal");
}

#[test]
fn registered_spaces_are_isolated_and_searches_span_committed_snapshots() {
    let temporary = tempfile::tempdir().expect("temp directory");
    let home = temporary.path();
    let service = service_with_spaces(home, &[("work", false)]);

    let personal = service
        .memory_create(create_request(
            "Deploy personal",
            "deploy only after checks",
        ))
        .expect("create personal");
    service
        .memory_create(create_request("Personal quiet note", "routine material"))
        .expect("create unrelated personal memory");
    service
        .memory_create(create_request(
            "Personal second quiet note",
            "routine material",
        ))
        .expect("create second unrelated personal memory");
    let work = service
        .memory_create(MemoryCreateRequest {
            space: Some("work".to_string()),
            ..create_request("Deploy work", "deploy only after review")
        })
        .expect("create work");
    service
        .memory_create(MemoryCreateRequest {
            space: Some("work".to_string()),
            ..create_request("Work quiet note", "routine material")
        })
        .expect("create unrelated work memory");
    service
        .memory_create(MemoryCreateRequest {
            space: Some("work".to_string()),
            ..create_request("Work second quiet note", "routine material")
        })
        .expect("create second unrelated work memory");

    assert_ne!(personal.receipt.memory.id, work.receipt.memory.id);
    assert_ne!(space_path(home, "personal"), space_path(home, "work"));
    assert_eq!(
        service
            .memory_list(MemoryListRequest::default())
            .expect("list personal")
            .memories
            .len(),
        3
    );
    assert_eq!(
        service
            .memory_list(MemoryListRequest {
                space: Some("work".to_string()),
                ..Default::default()
            })
            .expect("list work")
            .memories
            .len(),
        3
    );

    let all = service
        .memory_search(MemorySearchRequest {
            query: "deploy".to_string(),
            limit: Some(8),
            ..Default::default()
        })
        .expect("search all spaces");
    assert_eq!(all.total_matches, 2);
    let mut hit_spaces = all
        .hits
        .iter()
        .map(|hit| hit.memory.space.as_str())
        .collect::<Vec<_>>();
    hit_spaces.sort_unstable();
    assert_eq!(hit_spaces, ["personal", "work"]);

    let only_work = service
        .memory_search(MemorySearchRequest {
            space: Some("work".to_string()),
            query: "deploy".to_string(),
            limit: Some(8),
            ..Default::default()
        })
        .expect("search work");
    assert_eq!(only_work.total_matches, 1);
    assert_eq!(only_work.hits[0].memory.memory.id, work.receipt.memory.id);
    assert_clean(home, "personal");
    assert_clean(home, "work");
}

#[test]
fn readonly_space_rejects_writes_and_sync_without_side_effects() {
    let temporary = tempfile::tempdir().expect("temp directory");
    let home = temporary.path();
    let remote_path = home.join("readonly-remote.git");
    let remote_url = init_bare(&remote_path);
    let repository_path = space_path(home, "archive");
    let repository = GitRepository::initialize(&repository_path).expect("initialize archive");
    repository
        .set_remote_url("origin", &remote_url)
        .expect("configure remote");
    let service = service_with_spaces(home, &[("archive", true)]);

    assert!(service
        .memory_list(MemoryListRequest {
            space: Some("archive".to_string()),
            ..Default::default()
        })
        .expect("readonly list")
        .memories
        .is_empty());
    assert!(service
        .memory_search(MemorySearchRequest {
            space: Some("archive".to_string()),
            query: "blocked".to_string(),
            ..Default::default()
        })
        .expect("readonly search")
        .hits
        .is_empty());
    assert!(matches!(
        service.memory_get(MemoryGetRequest {
            space: Some("archive".to_string()),
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        }),
        Err(MemoroError::MemoryNotFound(_))
    ));
    assert!(service
        .sync_status(SyncRequest {
            space: Some("archive".to_string()),
            ..Default::default()
        })
        .expect("readonly sync status")
        .status
        .is_some());

    let create_error = service
        .memory_create(MemoryCreateRequest {
            space: Some("archive".to_string()),
            ..create_request("Blocked", "this must not be committed")
        })
        .expect_err("readonly create");
    assert!(matches!(create_error, MemoroError::Repository(_)));
    assert!(create_error.to_string().contains("archive"));
    assert!(create_error.to_string().contains("readonly"));
    assert_readonly(
        "patch",
        service.memory_patch(MemoryPatchRequest {
            space: Some("archive".to_string()),
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            base_revision: "a".repeat(64),
            edits: vec![PatchEdit {
                old_text: "old".to_string(),
                new_text: "new".to_string(),
            }],
            ..Default::default()
        }),
    );
    assert_readonly(
        "replace",
        service.memory_replace(MemoryReplaceRequest {
            space: Some("archive".to_string()),
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            base_revision: "a".repeat(64),
            summary: "Blocked replacement.".to_string(),
            body: "blocked body".to_string(),
            ..Default::default()
        }),
    );
    assert_readonly(
        "delete",
        service.memory_delete(MemoryDeleteRequest {
            space: Some("archive".to_string()),
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            expected_title: "Blocked".to_string(),
            base_revision: "a".repeat(64),
        }),
    );
    assert_readonly(
        "pull",
        service.sync_pull(SyncRequest {
            space: Some("archive".to_string()),
            ..Default::default()
        }),
    );
    assert_readonly(
        "push",
        service.sync_push(SyncRequest {
            space: Some("archive".to_string()),
            ..Default::default()
        }),
    );

    assert_eq!(repository.head().expect("archive head"), None);
    assert!(Repository::open_bare(&remote_path)
        .expect("open remote")
        .find_reference("refs/heads/main")
        .is_err());
    assert_clean(home, "archive");
}

#[test]
fn non_git_space_directory_is_never_overwritten() {
    let temporary = tempfile::tempdir().expect("temp directory");
    let home = temporary.path();
    let directory = space_path(home, "personal");
    fs::create_dir_all(&directory).expect("create non-git directory");
    let marker = directory.join("keep-me.txt");
    fs::write(&marker, "user data").expect("write marker");
    let service = Service::open(home.to_path_buf()).expect("open service");

    let error = service
        .memory_create(create_request("Unsafe", "must not overwrite"))
        .expect_err("refuse non-git directory");

    assert!(matches!(error, MemoroError::Repository(_)));
    assert!(error
        .to_string()
        .contains("not an independent Git repository"));
    assert_eq!(
        fs::read_to_string(marker).expect("read marker"),
        "user data"
    );
    assert!(!directory.join(".git").exists());
}

#[test]
fn mutation_lifecycle_returns_revisions_and_keeps_repository_clean() {
    let temporary = tempfile::tempdir().expect("temp directory");
    let home = temporary.path();
    let service = Service::open(home.to_path_buf()).expect("open service");

    let created = service
        .memory_create(create_request("Policy", "alpha rule"))
        .expect("create memory");
    assert_eq!(
        created.revision,
        memory_revision(&created.receipt.memory).expect("created revision")
    );
    assert_clean(home, "personal");

    let fetched = service
        .memory_get(MemoryGetRequest {
            id: created.receipt.memory.id.clone(),
            ..Default::default()
        })
        .expect("get memory");
    assert_eq!(fetched.revision, created.revision);
    let listed = service
        .memory_list(MemoryListRequest {
            kind: Some("persona".to_string()),
            ..Default::default()
        })
        .expect("list memories");
    assert_eq!(listed.memories.len(), 1);
    assert_eq!(listed.memories[0].revision, created.revision);
    assert_clean(home, "personal");

    let patched = service
        .memory_patch(MemoryPatchRequest {
            id: created.receipt.memory.id.clone(),
            base_revision: created.revision,
            edits: vec![PatchEdit {
                old_text: "alpha".to_string(),
                new_text: "beta".to_string(),
            }],
            ..Default::default()
        })
        .expect("patch memory");
    assert_eq!(patched.receipt.memory.body, "beta rule");
    assert_clean(home, "personal");

    let replaced = service
        .memory_replace(MemoryReplaceRequest {
            id: patched.receipt.memory.id.clone(),
            base_revision: patched.revision,
            summary: "Replacement policy.".to_string(),
            body: "replacement body".to_string(),
            tags: vec!["stable".to_string()],
            ..Default::default()
        })
        .expect("replace memory");
    assert_eq!(replaced.receipt.memory.body, "replacement body");
    assert_clean(home, "personal");

    service
        .memory_delete(MemoryDeleteRequest {
            id: replaced.receipt.memory.id,
            expected_title: "Policy".to_string(),
            base_revision: replaced.revision,
            ..Default::default()
        })
        .expect("delete memory");
    assert!(service
        .memory_list(MemoryListRequest::default())
        .expect("list after delete")
        .memories
        .is_empty());
    assert_clean(home, "personal");
}

#[test]
fn sync_uses_defaults_and_honors_explicit_remote_branch_and_timeout() {
    let temporary = tempfile::tempdir().expect("temp directory");
    let home = temporary.path();
    let service = Service::open(home.to_path_buf()).expect("open service");
    service
        .memory_create(create_request("Backup", "committed for local backup"))
        .expect("create backup memory");
    let repository = GitRepository::open(&space_path(home, "personal")).expect("open repository");
    let origin_path = home.join("origin.git");
    repository
        .set_remote_url(DEFAULT_SYNC_REMOTE, &init_bare(&origin_path))
        .expect("configure origin");

    let default_push = service
        .sync_push(SyncRequest::default())
        .expect("push with defaults");
    assert_eq!(default_push.outcome.action, SyncAction::Pushed);
    assert_eq!(default_push.outcome.remote, DEFAULT_SYNC_REMOTE);
    assert_eq!(default_push.outcome.branch, "main");
    assert!(Repository::open_bare(&origin_path)
        .expect("open origin")
        .find_reference("refs/heads/main")
        .is_ok());
    assert_clean(home, "personal");

    let default_status = service
        .sync_status(SyncRequest::default())
        .expect("status with defaults");
    assert!(default_status.initialized);
    assert_eq!(default_status.remote, DEFAULT_SYNC_REMOTE);
    assert_eq!(default_status.branch, "main");
    assert!(default_status.status.is_some());
    assert_clean(home, "personal");

    let backup_path = home.join("backup.git");
    repository
        .set_remote_url("backup", &init_bare(&backup_path))
        .expect("configure explicit remote");
    let explicit = SyncRequest {
        remote: Some("backup".to_string()),
        branch: Some("main".to_string()),
        timeout_secs: Some(1),
        ..Default::default()
    };
    let explicit_push = service
        .sync_push(explicit.clone())
        .expect("push explicit request");
    assert_eq!(explicit_push.outcome.action, SyncAction::Pushed);
    assert_eq!(explicit_push.outcome.remote, "backup");
    assert_eq!(explicit_push.outcome.branch, "main");
    let explicit_status = service
        .sync_status(explicit)
        .expect("status explicit request");
    assert_eq!(explicit_status.remote, "backup");
    assert_eq!(explicit_status.branch, "main");
    assert!(explicit_status.status.is_some());
    assert_clean(home, "personal");
}

#[test]
fn sync_pull_fast_forwards_a_registered_space_from_a_local_remote() {
    let temporary = tempfile::tempdir().expect("temp directory");
    let remote_path = temporary.path().join("remote.git");
    let remote_url = init_bare(&remote_path);

    let source_home = temporary.path().join("source-home");
    let source = Service::open(source_home.clone()).expect("open source service");
    let created = source
        .memory_create(create_request("Remote fact", "available after pull"))
        .expect("create source memory");
    GitRepository::open(&space_path(&source_home, "personal"))
        .expect("open source repository")
        .set_remote_url(DEFAULT_SYNC_REMOTE, &remote_url)
        .expect("configure source remote");
    source
        .sync_push(SyncRequest::default())
        .expect("push source memory");

    let target_home = temporary.path().join("target-home");
    let target_repository = GitRepository::initialize(&space_path(&target_home, "personal"))
        .expect("initialize target repository");
    target_repository
        .set_remote_url(DEFAULT_SYNC_REMOTE, &remote_url)
        .expect("configure target remote");
    let target = Service::open(target_home.clone()).expect("open target service");

    let pulled = target
        .sync_pull(SyncRequest::default())
        .expect("pull remote memory");
    assert_eq!(pulled.outcome.action, SyncAction::FastForwarded);
    let fetched = target
        .memory_get(MemoryGetRequest {
            id: created.receipt.memory.id,
            ..Default::default()
        })
        .expect("get pulled memory");
    assert_eq!(fetched.memory.body, "available after pull");
    assert_clean(&source_home, "personal");
    assert_clean(&target_home, "personal");
}
