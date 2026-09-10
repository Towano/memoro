//! Mirrors `python/tests/test_git.py` case by case.
//!
//! The Python suite drives `MemoryStore` to produce commits and monkeypatches
//! `_run` for timeout scenarios. The Rust port keeps every test's intent:
//! commits are produced through `GitRepository::stage`/`commit` directly
//! (the store module is implemented by another worker), and timeouts use a
//! stalled `git://` server combined with a short deadline, which is the
//! observable equivalent of the monkeypatched hanging subprocess.

use std::fs;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use git2::{Repository, RepositoryInitOptions};

use memoro::errors::MemoroError;
use memoro::git::{
    git_environment_from, GitRepository, PushOutcome, GIT_IDENTITY_EMAIL, GIT_IDENTITY_NAME,
    PUSH_TIMEOUT_SECONDS,
};

const MEMORY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const OTHER_MEMORY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

fn space_repository(root: &Path) -> GitRepository {
    GitRepository::initialize(&root.join("space")).expect("initialize repository")
}

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

fn write_one(repository: &GitRepository) -> String {
    write_memory(repository, MEMORY_ID, "body")
}

fn init_bare(path: &Path) -> String {
    let mut options = RepositoryInitOptions::new();
    options.bare(true);
    Repository::init_opts(path, &options).expect("init bare repository");
    path_string(path)
}

fn path_string(path: &Path) -> String {
    path.to_str().expect("UTF-8 path").to_string()
}

/// Serves a `git://` endpoint that accepts the request and then never
/// responds, standing in for the unreachable/slow remote of the Python tests.
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

#[test]
fn test_initialize_creates_main_repository_with_local_identity() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("space");

    let repository = GitRepository::initialize(&path).expect("initialize");

    assert_eq!(repository.current_branch().expect("current branch"), "main");
    let repo = Repository::open(&path).expect("open repository");
    let config = repo.config().expect("read config");
    assert_eq!(
        config.get_string("user.name").expect("user.name"),
        GIT_IDENTITY_NAME
    );
    assert_eq!(
        config.get_string("user.email").expect("user.email"),
        GIT_IDENTITY_EMAIL
    );
    assert!(!config.get_bool("commit.gpgSign").expect("commit.gpgSign"));
}

#[test]
fn test_initialize_accepts_an_empty_directory() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("space");
    fs::create_dir_all(&path).expect("create directory");

    let repository = GitRepository::initialize(&path).expect("initialize");

    assert_eq!(
        PathBuf::from(repository.path()),
        fs::canonicalize(&path).expect("resolved path")
    );
    assert!(path.join(".git").is_dir());
}

#[test]
fn test_initialize_refuses_nonempty_non_repository_without_overwriting() {
    let root = tempfile::tempdir().expect("temp dir");
    let path = root.path().join("space");
    fs::create_dir_all(&path).expect("create directory");
    let marker = path.join("keep-me.txt");
    fs::write(&marker, "user data").expect("write marker");

    let error = GitRepository::initialize(&path).expect_err("initialize refused");

    assert!(matches!(error, MemoroError::Repository(_)));
    assert!(error
        .to_string()
        .contains("not an independent Git repository"));
    assert_eq!(
        fs::read_to_string(&marker).expect("read marker"),
        "user data"
    );
    assert!(!path.join(".git").exists());
}

#[test]
fn test_create_and_replace_create_two_commits() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let relative_path = format!("persona/{MEMORY_ID}.md");

    let first = write_one(&repository);
    let target = repository
        .worktree_path(&relative_path)
        .expect("worktree path");
    fs::write(&target, "two").expect("replace memory");
    repository.stage(&relative_path).expect("stage replacement");
    let second = repository
        .commit("memory(persona): replace \"Fact\"", &relative_path)
        .expect("commit replacement");

    // The Python test asserts store-level chaining; the git-level equivalent is
    // that the second commit's parent is the first, and the first is a root.
    assert_eq!(second, repository.head().expect("head").expect("commit id"));
    assert_eq!(
        repository.resolve_commit("HEAD^").expect("resolve parent"),
        Some(first.clone())
    );
    let repo = Repository::open(repository.path()).expect("open repository");
    let mut commit = repo.head().expect("head").peel_to_commit().expect("commit");
    let mut subjects: Vec<String> = Vec::new();
    let mut count = 0usize;
    loop {
        count += 1;
        subjects.push(commit.message().unwrap_or_default().to_string());
        match commit.parent(0) {
            Ok(parent) => commit = parent,
            Err(_) => break,
        }
    }
    assert_eq!(count, 2);
    assert_eq!(
        subjects,
        vec![
            "memory(persona): replace \"Fact\"",
            "memory(persona): create \"Fact\"",
        ]
    );
}

#[test]
fn test_pushes_exact_commit_to_local_bare_repository() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    let commit = write_one(&repository);
    let url = init_bare(&root.path().join("backup.git"));
    repository
        .set_remote_url("backup", &url)
        .expect("add remote");

    let outcome = repository
        .push("backup", "main", PUSH_TIMEOUT_SECONDS)
        .expect("push outcome");

    assert_eq!(
        outcome,
        PushOutcome {
            attempted: true,
            succeeded: true,
            reason: "pushed".to_string(),
        }
    );
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
fn test_fetch_and_reset_fast_forward_an_empty_repository() {
    let root = tempfile::tempdir().expect("temp dir");
    let source = GitRepository::initialize(&root.path().join("source")).expect("source");
    let commit = write_one(&source);
    let url = init_bare(&root.path().join("remote.git"));
    source.set_remote_url("backup", &url).expect("add remote");
    source
        .push("backup", "main", PUSH_TIMEOUT_SECONDS)
        .expect("seed remote");
    let target = GitRepository::initialize(&root.path().join("target")).expect("target");
    target.set_remote_url("origin", &url).expect("add origin");

    let remote_commit = target
        .fetch("origin", "main", PUSH_TIMEOUT_SECONDS)
        .expect("fetch")
        .expect("remote commit");
    target.reset_to(&remote_commit).expect("reset");

    assert_eq!(remote_commit, commit);
    assert_eq!(target.head().expect("head"), Some(commit));
    // The Python test reads the snapshot through MemoryStore; the store module
    // is another worker's deliverable, so assert the git-level equivalent.
    assert_eq!(
        target
            .memory_paths_at_commit(&remote_commit)
            .expect("memory paths"),
        vec![format!("persona/{MEMORY_ID}.md")]
    );
    assert!(target
        .read_at_commit(&remote_commit, &format!("persona/{MEMORY_ID}.md"))
        .expect("read memory")
        .contains("body"));
}

#[test]
fn test_push_timeout_is_a_best_effort_failure() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    write_one(&repository);
    let url = stall_server_url();
    repository
        .set_remote_url("origin", &url)
        .expect("add remote");

    let outcome = repository.push("origin", "main", 1).expect("push outcome");

    assert_eq!(
        outcome,
        PushOutcome {
            attempted: true,
            succeeded: false,
            reason: "timeout".to_string(),
        }
    );
}

#[test]
fn test_fetch_reports_timeout_and_command_failure() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    repository
        .set_remote_url("origin", &stall_server_url())
        .expect("add remote");

    let error = repository
        .fetch("origin", "main", 1)
        .expect_err("fetch timeout");

    assert!(matches!(error, MemoroError::Repository(_)));
    assert!(error.to_string().contains("timed out while synchronizing"));

    repository
        .set_remote_url("origin", &path_string(&root.path().join("remote.git")))
        .expect("retarget remote");
    let error = repository
        .fetch("origin", "main", PUSH_TIMEOUT_SECONDS)
        .expect_err("fetch failure");

    assert!(error.to_string().contains("could not synchronize"));
}

#[test]
fn test_verify_push_reports_timeout_and_non_fast_forward() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    write_one(&repository);

    let error = repository
        .verify_push(&stall_server_url(), "main", 1)
        .expect_err("verify timeout");
    assert!(matches!(error, MemoroError::Repository(_)));
    assert!(error
        .to_string()
        .contains("timed out while checking write access"));

    // The Python test fakes a "non-fast-forward" rejection; the real
    // equivalent is a remote branch with unrelated local history.
    let url = init_bare(&root.path().join("diverged.git"));
    let other = GitRepository::initialize(&root.path().join("other")).expect("other");
    write_memory(&other, OTHER_MEMORY_ID, "unrelated");
    other.set_remote_url("origin", &url).expect("add remote");
    other
        .push("origin", "main", PUSH_TIMEOUT_SECONDS)
        .expect("seed diverged remote");
    let error = repository
        .verify_push(&url, "main", PUSH_TIMEOUT_SECONDS)
        .expect_err("verify rejection");
    assert!(error.to_string().contains("history incompatible"));
}

#[test]
fn test_push_command_failure_does_not_raise() {
    let root = tempfile::tempdir().expect("temp dir");
    let repository = space_repository(root.path());
    write_one(&repository);
    repository
        .set_remote_url("origin", &path_string(&root.path().join("missing.git")))
        .expect("add remote");

    let outcome = repository
        .push("origin", "main", PUSH_TIMEOUT_SECONDS)
        .expect("push outcome");

    assert_eq!(
        outcome,
        PushOutcome {
            attempted: true,
            succeeded: false,
            reason: "failed".to_string(),
        }
    );
}

#[test]
fn test_git_environment_does_not_override_repository_deploy_key() {
    let environment = git_environment_from(vec![
        ("GIT_SSH".to_string(), "untrusted-ssh".to_string()),
        (
            "GIT_SSH_COMMAND".to_string(),
            "untrusted ssh command".to_string(),
        ),
        ("GIT_SSH_VARIANT".to_string(), "plink".to_string()),
        ("GIT_AUTHOR_NAME".to_string(), "someone".to_string()),
        ("HOME".to_string(), "/home/user".to_string()),
    ]);

    assert!(!environment.contains_key("GIT_SSH"));
    assert!(!environment.contains_key("GIT_SSH_COMMAND"));
    assert!(!environment.contains_key("GIT_SSH_VARIANT"));
    assert!(!environment.contains_key("GIT_AUTHOR_NAME"));
    assert_eq!(
        environment.get("HOME").map(String::as_str),
        Some("/home/user")
    );
    assert_eq!(
        environment.get("GIT_TERMINAL_PROMPT").map(String::as_str),
        Some("0")
    );
}
