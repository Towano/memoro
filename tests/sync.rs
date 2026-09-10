//! Tests for the synchronization orchestration layer (`src/sync.rs`).
//!
//! Remotes are local bare repositories, and timeouts use a stalled `git://`
//! server, mirroring the construction techniques of `tests/git.rs`.

use std::fs;
use std::io::Read;
use std::net::TcpListener;
use std::path::Path;
use std::thread;
use std::time::Duration;

use git2::{Repository, RepositoryInitOptions};

use memoro::git::GitRepository;
use memoro::sync::{SyncAction, SyncEngine};

const MEMORY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const OTHER_MEMORY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const LOCAL_MEMORY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const TIMEOUT: u64 = 15;

fn write_memory(repository: &GitRepository, memory_id: &str, body: &str) -> String {
    let relative_path = format!("persona/{memory_id}.md");
    let target = repository
        .worktree_path(&relative_path)
        .expect("worktree path");
    fs::create_dir_all(target.parent().expect("parent directory")).expect("create directory");
    fs::write(&target, body).expect("write memory");
    repository.stage(&relative_path).expect("stage memory");
    repository
        .commit("memory(persona): create \"Fact\"", &relative_path)
        .expect("commit memory")
}

fn init_bare(path: &Path) -> String {
    let mut options = RepositoryInitOptions::new();
    options.bare(true);
    Repository::init_opts(path, &options).expect("init bare repository");
    path.to_str().expect("UTF-8 path").to_string()
}

/// Serves a `git://` endpoint that accepts the request and then never
/// responds, standing in for an unreachable remote.
fn stall_server_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stall listener");
    let address = listener.local_addr().expect("stall address");
    thread::spawn(move || {
        if let Ok((mut connection, _)) = listener.accept() {
            let mut buffer = [0u8; 1024];
            let _ = connection.read(&mut buffer);
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        }
    });
    format!("git://{address}/memoro.git")
}

/// A repository `a` seeded with one pushed commit plus a synchronized engine
/// `b` sitting on that same commit, both talking to one bare remote.
struct Pair {
    a: GitRepository,
    b: SyncEngine,
}

fn linked_pair(root: &Path) -> Pair {
    let a = GitRepository::initialize(&root.join("a")).expect("initialize a");
    write_memory(&a, MEMORY_ID, "body");
    let url = init_bare(&root.join("remote.git"));
    a.set_remote_url("origin", &url)
        .expect("configure remote a");
    a.push("origin", "main", TIMEOUT).expect("seed remote");
    let b = GitRepository::initialize(&root.join("b")).expect("initialize b");
    b.set_remote_url("origin", &url)
        .expect("configure remote b");
    let engine = SyncEngine::new(b);
    let initial = engine
        .pull("origin", "main", TIMEOUT)
        .expect("initial pull");
    assert_eq!(initial.action, SyncAction::FastForwarded);
    Pair { a, b: engine }
}

#[test]
fn test_pull_fast_forwards_an_empty_repository() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = GitRepository::initialize(&root.path().join("source")).expect("source");
    let commit = write_memory(&source, MEMORY_ID, "body");
    let url = init_bare(&root.path().join("remote.git"));
    source.set_remote_url("origin", &url).expect("add remote");
    source.push("origin", "main", TIMEOUT).expect("seed remote");
    let target = GitRepository::initialize(&root.path().join("target")).expect("target");
    target.set_remote_url("origin", &url).expect("add origin");
    let engine = SyncEngine::new(target);

    let outcome = engine.pull("origin", "main", TIMEOUT).expect("pull");

    assert_eq!(outcome.action, SyncAction::FastForwarded);
    assert_eq!(
        engine.repository().head().expect("head"),
        Some(commit.clone())
    );
    assert_eq!(
        engine
            .repository()
            .memory_paths_at_commit(&commit)
            .expect("memory paths"),
        vec![format!("persona/{MEMORY_ID}.md")]
    );
    assert_eq!(
        engine.repository().sync_conflict_commit().expect("marker"),
        None
    );
}

#[test]
fn test_pull_marks_divergence_as_conflict_without_losing_local() {
    let root = tempfile::tempdir().expect("temp dir");
    let pair = linked_pair(root.path());
    write_memory(&pair.a, OTHER_MEMORY_ID, "remote-side");
    pair.a
        .push("origin", "main", TIMEOUT)
        .expect("push remote side");
    let local = write_memory(pair.b.repository(), LOCAL_MEMORY_ID, "local-side");

    let outcome = pair.b.pull("origin", "main", TIMEOUT).expect("pull");

    assert_eq!(outcome.action, SyncAction::Conflict);
    assert_eq!(
        pair.b.repository().head().expect("head"),
        Some(local.clone())
    );
    assert_eq!(
        pair.b.repository().sync_conflict_commit().expect("marker"),
        Some(local.clone())
    );
    assert!(pair
        .b
        .repository()
        .read_at_commit(&local, &format!("persona/{LOCAL_MEMORY_ID}.md"))
        .expect("read local memory")
        .contains("local-side"));
    assert!(pair
        .b
        .repository()
        .path()
        .join(format!("persona/{LOCAL_MEMORY_ID}.md"))
        .exists());
}

#[test]
fn test_pull_is_no_change_when_heads_match() {
    let root = tempfile::tempdir().expect("temp dir");
    let pair = linked_pair(root.path());
    let base = pair.b.repository().head().expect("head").expect("commit");

    let outcome = pair.b.pull("origin", "main", TIMEOUT).expect("pull");

    assert_eq!(outcome.action, SyncAction::NoChange);
    assert_eq!(
        pair.b.repository().head().expect("head"),
        Some(base.clone())
    );
}

#[test]
fn test_pull_is_no_change_when_local_is_ahead() {
    let root = tempfile::tempdir().expect("temp dir");
    let pair = linked_pair(root.path());
    let local = write_memory(pair.b.repository(), LOCAL_MEMORY_ID, "local-side");

    let outcome = pair.b.pull("origin", "main", TIMEOUT).expect("pull");

    assert_eq!(outcome.action, SyncAction::NoChange);
    assert_eq!(
        pair.b.repository().head().expect("head"),
        Some(local.clone())
    );
}

#[test]
fn test_pull_clears_stale_conflict_marker_on_fast_forward() {
    let root = tempfile::tempdir().expect("temp dir");
    let pair = linked_pair(root.path());
    let base = pair.b.repository().head().expect("head").expect("commit");
    pair.b
        .repository()
        .mark_sync_conflict(&base)
        .expect("mark stale conflict");
    assert_eq!(
        pair.b.repository().sync_conflict_commit().expect("marker"),
        Some(base.clone())
    );
    let remote_commit = write_memory(&pair.a, OTHER_MEMORY_ID, "remote-side");
    pair.a
        .push("origin", "main", TIMEOUT)
        .expect("push remote side");

    let outcome = pair.b.pull("origin", "main", TIMEOUT).expect("pull");

    assert_eq!(outcome.action, SyncAction::FastForwarded);
    assert_eq!(
        pair.b.repository().head().expect("head"),
        Some(remote_commit.clone())
    );
    assert_eq!(
        pair.b.repository().sync_conflict_commit().expect("marker"),
        None
    );
}

#[test]
fn test_push_to_clean_remote_succeeds() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = GitRepository::initialize(&root.path().join("space")).expect("initialize");
    let commit = write_memory(&repository, MEMORY_ID, "body");
    let url = init_bare(&root.path().join("backup.git"));
    repository
        .set_remote_url("origin", &url)
        .expect("add remote");
    let engine = SyncEngine::new(repository);

    let outcome = engine.push("origin", "main", TIMEOUT).expect("push");

    assert_eq!(outcome.action, SyncAction::Pushed);
    let remote_repo = Repository::open_bare(root.path().join("backup.git")).expect("open bare");
    let remote_head = remote_repo
        .find_reference("refs/heads/main")
        .expect("remote main");
    assert_eq!(
        remote_head.target().expect("remote head").to_string(),
        commit
    );
}

#[test]
fn test_push_rejects_non_fast_forward_and_suggests_pull() {
    let root = tempfile::tempdir().expect("temp dir");
    let pair = linked_pair(root.path());
    let remote_commit = write_memory(&pair.a, OTHER_MEMORY_ID, "remote-side");
    pair.a
        .push("origin", "main", TIMEOUT)
        .expect("push remote side");
    let local = write_memory(pair.b.repository(), LOCAL_MEMORY_ID, "local-side");

    let outcome = pair
        .b
        .push("origin", "main", TIMEOUT)
        .expect("push outcome");

    assert_eq!(outcome.action, SyncAction::RejectedNonFastForward);
    assert!(outcome.message.contains("Pull first"));
    let remote_repo =
        Repository::open_bare(root.path().join("remote.git")).expect("open bare remote");
    assert_eq!(
        remote_repo
            .find_reference("refs/heads/main")
            .expect("remote main")
            .target()
            .expect("remote head")
            .to_string(),
        remote_commit
    );
    assert_eq!(
        pair.b.repository().head().expect("head"),
        Some(local.clone())
    );
}

#[test]
fn test_status_reports_heads_ahead_behind_and_conflict() {
    let root = tempfile::tempdir().expect("temp dir");
    let pair = linked_pair(root.path());
    let base = pair.b.repository().head().expect("head").expect("commit");

    let synced = pair.b.status("origin", "main", TIMEOUT).expect("status");
    assert_eq!((synced.ahead, synced.behind), (0, 0));
    assert_eq!(synced.local_head, Some(base.clone()));
    assert_eq!(synced.remote_head, Some(base.clone()));
    assert!(synced.conflict_commit.is_none());

    let remote_commit = write_memory(&pair.a, OTHER_MEMORY_ID, "remote-side");
    pair.a
        .push("origin", "main", TIMEOUT)
        .expect("push remote side");
    let local = write_memory(pair.b.repository(), LOCAL_MEMORY_ID, "local-side");

    let diverged = pair.b.status("origin", "main", TIMEOUT).expect("status");
    assert_eq!((diverged.ahead, diverged.behind), (1, 1));
    assert_eq!(diverged.local_head, Some(local.clone()));
    assert_eq!(diverged.remote_head, Some(remote_commit.clone()));
    assert!(diverged.conflict_commit.is_none());

    let outcome = pair.b.pull("origin", "main", TIMEOUT).expect("pull");
    assert_eq!(outcome.action, SyncAction::Conflict);
    let marked = pair.b.status("origin", "main", TIMEOUT).expect("status");
    assert_eq!(marked.conflict_commit, Some(local.clone()));
}

#[test]
fn test_status_and_pull_time_out_on_stalled_remote() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = GitRepository::initialize(&root.path().join("space")).expect("initialize");
    write_memory(&repository, MEMORY_ID, "body");
    repository
        .set_remote_url("origin", &stall_server_url())
        .expect("add remote");
    let engine = SyncEngine::new(repository);

    let status_error = engine
        .status("origin", "main", 1)
        .expect_err("status timeout");
    assert!(matches!(
        status_error,
        memoro::errors::MemoroError::Repository(_)
    ));
    assert!(status_error
        .to_string()
        .contains("timed out while synchronizing"));

    let pull_error = engine.pull("origin", "main", 1).expect_err("pull timeout");
    assert!(pull_error
        .to_string()
        .contains("timed out while synchronizing"));
}

#[test]
fn test_push_times_out_on_stalled_remote() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = GitRepository::initialize(&root.path().join("space")).expect("initialize");
    write_memory(&repository, MEMORY_ID, "body");
    repository
        .set_remote_url("origin", &stall_server_url())
        .expect("add remote");
    let engine = SyncEngine::new(repository);

    let outcome = engine.push("origin", "main", 1).expect("push outcome");

    assert_eq!(outcome.action, SyncAction::Timeout);
}

#[test]
fn test_push_without_commits_reports_no_commit() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = GitRepository::initialize(&root.path().join("space")).expect("initialize");
    let engine = SyncEngine::new(repository);

    let outcome = engine
        .push("origin", "main", TIMEOUT)
        .expect("push outcome");

    assert_eq!(outcome.action, SyncAction::NoCommit);
}

#[test]
fn test_push_reports_failure_when_remote_unreachable() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = GitRepository::initialize(&root.path().join("space")).expect("initialize");
    write_memory(&repository, MEMORY_ID, "body");
    repository
        .set_remote_url("origin", &root.path().join("missing.git").to_string_lossy())
        .expect("add remote");
    let engine = SyncEngine::new(repository);

    let outcome = engine
        .push("origin", "main", TIMEOUT)
        .expect("push outcome");

    assert_eq!(outcome.action, SyncAction::Failed);
}
