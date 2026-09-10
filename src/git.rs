//! Git-backed storage backend for Memoro memories.
//!
//! Mirrors `python/src/memoro/git.py`: validation rules, constants, algorithms
//! and English error messages are kept identical. Where the Python version
//! shells out to the `git` CLI, this implementation uses `git2` (vendored
//! libgit2); remote operations run on a background thread bounded by a
//! deadline, replacing the subprocess timeouts.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use git2::{
    AutotagOption, FetchOptions, FetchPrune, ObjectType, PushOptions, RemoteCallbacks, Repository,
    RepositoryInitOptions, ResetType, Signature, StatusOptions,
};

use crate::errors::MemoroError;

pub const GIT_IDENTITY_NAME: &str = "Memoro";
pub const GIT_IDENTITY_EMAIL: &str = "memoro@localhost";
pub const PUSH_TIMEOUT_SECONDS: u64 = 15;
pub const SYNC_CONFLICT_REF: &str = "refs/memoro/sync-conflict";
pub const MEMORY_TOP_LEVEL_DIRS: [&str; 3] = ["persona", "playbooks", "projects"];

const REGULAR_FILE_MODES: [i32; 2] = [0o100644, 0o100755];

/// Environment variables the Python implementation strips before running Git.
const REPOSITORY_CONTROL_ENV: [&str; 23] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_ASKPASS",
    "GIT_CEILING_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_DEFAULT_HASH",
    "GIT_DIR",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_EXEC_PATH",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_INTERNAL_SUPER_PREFIX",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_QUARANTINE_PATH",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_SSH_VARIANT",
    "GIT_TEMPLATE_DIR",
    "GIT_WORK_TREE",
    "SSH_ASKPASS",
];

/// Outcome of a best-effort push, mirroring `PushOutcome` in `python/src/memoro/git.py`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    pub attempted: bool,
    pub succeeded: bool,
    pub reason: String,
}

#[derive(Debug)]
pub struct GitRepository {
    path: PathBuf,
}

impl GitRepository {
    pub fn new(path: &Path) -> Self {
        GitRepository {
            path: resolve_lazy(path),
        }
    }

    /// Resolved repository worktree path (`self.path` in the Python class).
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn initialize(path: &Path) -> Result<Self, MemoroError> {
        let resolved = resolve_lazy(path);
        if resolved.exists() && !resolved.is_dir() {
            return Err(MemoroError::Repository(format!(
                "Memory repository path {} is not a directory. Move that file or choose a \
                 different --home, then restart Memoro.",
                resolved.display()
            )));
        }
        if let Err(error) = fs::create_dir_all(&resolved) {
            return Err(MemoroError::Repository(format!(
                "Git operation 'init' failed in {}. Inspect the repository and Git \
                 configuration, then retry. {error}",
                resolved.display()
            )));
        }
        let repository = GitRepository { path: resolved };
        if directory_is_empty(&repository.path) {
            let mut options = RepositoryInitOptions::new();
            options.initial_head("main");
            Repository::init_opts(&repository.path, &options)
                .map_err(|_| repository.git_op_failed("init"))?;
        }
        repository.validate_independent()?;
        let repo = repository.open_repo("config").and_then(|repo| {
            repo.config()
                .map_err(|_| repository.git_op_failed("config"))
        });
        let mut config = repo?;
        let name = env_identity("MEMORO_GIT_NAME", GIT_IDENTITY_NAME);
        let email = env_identity("MEMORO_GIT_EMAIL", GIT_IDENTITY_EMAIL);
        config
            .set_str("user.name", &name)
            .map_err(|_| repository.git_op_failed("config"))?;
        config
            .set_str("user.email", &email)
            .map_err(|_| repository.git_op_failed("config"))?;
        config
            .set_bool("commit.gpgSign", false)
            .map_err(|_| repository.git_op_failed("config"))?;
        Ok(repository)
    }

    pub fn open(path: &Path) -> Result<Self, MemoroError> {
        let resolved = resolve_lazy(path);
        if !resolved.is_dir() {
            return Err(MemoroError::Repository(format!(
                "Memory repository {} does not exist. Start Memoro or run 'memoro sync setup \
                 REPOSITORY_URL' first.",
                resolved.display()
            )));
        }
        let repository = GitRepository { path: resolved };
        repository.validate_independent()?;
        Ok(repository)
    }

    /// Mirrors `_validate_independent`: the directory must be the root of its
    /// own (non-bare, non-linked) repository with a plain `.git` directory.
    fn validate_independent(&self) -> Result<(), MemoroError> {
        let rejected = || {
            MemoroError::Repository(format!(
                "Memory directory {} is not an independent Git repository. Memoro left its \
                 contents unchanged. Move them elsewhere or choose a different --home, then \
                 restart Memoro.",
                self.path.display()
            ))
        };
        let git_directory = self.path.join(".git");
        let git_directory_is_plain = fs::symlink_metadata(&git_directory)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);
        let opened = Repository::open(&self.path);
        let independent = match opened {
            Ok(repo) => {
                let workdir_matches = repo
                    .workdir()
                    .map(|workdir| same_path(workdir, &self.path))
                    .unwrap_or(false);
                let gitdir_matches = same_path(repo.path(), &git_directory);
                let common_matches = same_path(repo.commondir(), &git_directory);
                workdir_matches && git_directory_is_plain && gitdir_matches && common_matches
            }
            Err(_) => false,
        };
        if independent {
            Ok(())
        } else {
            Err(rejected())
        }
    }

    pub fn head(&self) -> Result<Option<String>, MemoroError> {
        self.resolve_commit("HEAD")
    }

    pub fn resolve_commit(&self, revision: &str) -> Result<Option<String>, MemoroError> {
        match Repository::open(&self.path) {
            Ok(repo) => match repo.revparse_single(&format!("{revision}^{{commit}}")) {
                Ok(object) => Ok(Some(object.id().to_string())),
                Err(_) => Ok(None),
            },
            Err(_) => Ok(None),
        }
    }

    pub fn current_branch(&self) -> Result<String, MemoroError> {
        let repo = self.open_repo("symbolic-ref")?;
        let not_on_a_branch = || self.not_on_a_branch();
        // `git symbolic-ref --short HEAD` resolves the symbolic target even
        // while the branch is unborn, unlike git2's `Repository::head`.
        let head = repo.find_reference("HEAD").map_err(|_| not_on_a_branch())?;
        let Ok(Some(target)) = head.symbolic_target() else {
            return Err(not_on_a_branch());
        };
        let Some(branch) = target.strip_prefix("refs/heads/") else {
            return Err(not_on_a_branch());
        };
        if branch.is_empty() {
            return Err(not_on_a_branch());
        }
        Ok(branch.to_string())
    }

    pub fn assert_clean(&self) -> Result<(), MemoroError> {
        let repo = self.open_repo("status")?;
        let dirty = || {
            MemoroError::RepositoryDirty(format!(
                "Memory repository {} has uncommitted changes. Memoro did not write anything. \
                 Commit, discard, or move those changes, then retry.",
                self.path.display()
            ))
        };
        let mut options = StatusOptions::new();
        options.include_untracked(true).recurse_untracked_dirs(true);
        let statuses = repo
            .statuses(Some(&mut options))
            .map_err(|_| self.git_op_failed("status"))?;
        if !statuses.is_empty() {
            return Err(dirty());
        }
        let unfinished = || {
            MemoroError::RepositoryDirty(format!(
                "Memory repository {} has an unfinished Git operation. Memoro did not write \
                 anything. Finish or abort that operation, then retry.",
                self.path.display()
            ))
        };
        const OPERATION_PATHS: [&str; 8] = [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "REBASE_HEAD",
            "rebase-apply",
            "rebase-merge",
            "sequencer",
            "BISECT_LOG",
        ];
        let git_directory = repo.path().to_path_buf();
        for name in OPERATION_PATHS {
            if git_directory.join(name).exists() {
                return Err(unfinished());
            }
        }
        Ok(())
    }

    pub fn memory_paths_at_commit(&self, commit: &str) -> Result<Vec<String>, MemoroError> {
        let repo = self.open_repo("ls-tree")?;
        let tree = repo
            .revparse_single(&format!("{commit}^{{commit}}"))
            .and_then(|object| object.peel_to_tree())
            .map_err(|_| self.git_op_failed("ls-tree"))?;
        let mut paths = Vec::new();
        for directory in MEMORY_TOP_LEVEL_DIRS {
            if let Some(entry) = tree.get_name(directory) {
                if entry.kind() == Some(ObjectType::Tree) {
                    let subtree = repo
                        .find_tree(entry.id())
                        .map_err(|_| self.git_op_failed("ls-tree"))?;
                    collect_memory_paths(
                        &repo,
                        &subtree,
                        &format!("{directory}/"),
                        commit,
                        &mut paths,
                    )?;
                }
            }
        }
        paths.sort();
        Ok(paths)
    }

    pub fn read_at_commit(&self, commit: &str, relative_path: &str) -> Result<String, MemoroError> {
        validate_relative_path(relative_path)?;
        let repo = self.open_repo("show")?;
        let Some(tree) = repo
            .revparse_single(&format!("{commit}^{{commit}}"))
            .ok()
            .and_then(|object| object.peel_to_tree().ok())
        else {
            return Err(self.git_op_failed("show"));
        };
        let entry = tree
            .get_path(Path::new(relative_path))
            .map_err(|_| self.git_op_failed("show"))?;
        if entry.kind() != Some(ObjectType::Blob) {
            return Err(self.git_op_failed("show"));
        }
        let blob = repo
            .find_blob(entry.id())
            .map_err(|_| self.git_op_failed("show"))?;
        match String::from_utf8(blob.content().to_vec()) {
            Ok(content) => Ok(content),
            Err(_) => Err(MemoroError::Repository(format!(
                "Committed memory {} is not UTF-8. Repair and commit the file, then retry.",
                py_repr(relative_path)
            ))),
        }
    }

    pub fn worktree_path(&self, relative_path: &str) -> Result<PathBuf, MemoroError> {
        let parts = posix_parts(relative_path);
        validate_relative_path(relative_path)?;
        let mut candidate = self.path.clone();
        for part in &parts {
            candidate.push(part);
        }
        let mut current = self.path.clone();
        for part in &parts[..parts.len().saturating_sub(1)] {
            current.push(part);
            let is_link = fs::symlink_metadata(&current)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false);
            if is_link {
                return Err(MemoroError::Repository(format!(
                    "Memory path {} passes through a filesystem link. Replace the linked \
                     directory with a regular directory before retrying.",
                    py_repr(relative_path)
                )));
            }
        }
        let parent = candidate.parent().unwrap_or(&self.path);
        let resolved_parent = resolve_lazy(parent);
        if resolved_parent.strip_prefix(&self.path).is_err() {
            return Err(MemoroError::Repository(format!(
                "Memory path {} leaves the repository.",
                py_repr(relative_path)
            )));
        }
        Ok(candidate)
    }

    pub fn stage(&self, relative_path: &str) -> Result<(), MemoroError> {
        validate_relative_path(relative_path)?;
        let repo = self.open_repo("add")?;
        let mut index = repo.index().map_err(|_| self.git_op_failed("add"))?;
        let worktree_file = self.path.join(relative_path);
        if fs::symlink_metadata(&worktree_file).is_ok() {
            index
                .add_path(Path::new(relative_path))
                .map_err(|_| self.git_op_failed("add"))?;
        } else if index.get_path(Path::new(relative_path), 0).is_some() {
            index
                .remove_path(Path::new(relative_path))
                .map_err(|_| self.git_op_failed("add"))?;
        } else {
            return Err(self.git_op_failed("add"));
        }
        index.write().map_err(|_| self.git_op_failed("add"))?;
        Ok(())
    }

    pub fn staged_paths(&self) -> Result<Vec<String>, MemoroError> {
        let repo = self.open_repo("diff")?;
        let index = repo.index().map_err(|_| self.git_op_failed("diff"))?;
        let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
        let diff = repo
            .diff_tree_to_index(head_tree.as_ref(), Some(&index), None)
            .map_err(|_| self.git_op_failed("diff"))?;
        let mut paths: Vec<String> = diff
            .deltas()
            .filter_map(|delta| {
                delta
                    .new_file()
                    .path()
                    .or(delta.old_file().path())
                    .and_then(|path| path.to_str())
                    .map(String::from)
            })
            .collect();
        paths.sort();
        Ok(paths)
    }

    pub fn commit(&self, message: &str, relative_path: &str) -> Result<String, MemoroError> {
        let staged = self.staged_paths()?;
        if staged != [relative_path.to_string()] {
            return Err(MemoroError::Repository(
                "Git index contains paths outside the requested memory write. Memoro stopped \
                 before committing; inspect the memory repository index."
                    .to_string(),
            ));
        }
        let repo = self.open_repo("commit")?;
        let signature = self.commit_signature(&repo)?;
        let mut index = repo.index().map_err(|_| self.git_op_failed("commit"))?;
        let tree_id = index
            .write_tree()
            .map_err(|_| self.git_op_failed("commit"))?;
        let tree = repo
            .find_tree(tree_id)
            .map_err(|_| self.git_op_failed("commit"))?;
        let parents: Vec<git2::Commit> = match repo.head() {
            Ok(head) => vec![head
                .peel_to_commit()
                .map_err(|_| self.git_op_failed("commit"))?],
            Err(_) => Vec::new(),
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )
        .map_err(|_| self.git_op_failed("commit"))?;
        let Some(commit) = self.head()? else {
            return Err(MemoroError::Repository(
                "Git commit completed without producing a readable HEAD.".to_string(),
            ));
        };
        if self.commit_paths(&commit)? != [relative_path.to_string()] {
            return Err(MemoroError::Repository(
                "Git produced a commit containing paths outside the requested memory write. \
                 Memoro left the commit unchanged; inspect the memory repository before retrying."
                    .to_string(),
            ));
        }
        Ok(commit)
    }

    pub fn commit_paths(&self, commit: &str) -> Result<Vec<String>, MemoroError> {
        let repo = self.open_repo("diff-tree")?;
        let commit_object = repo
            .revparse_single(&format!("{commit}^{{commit}}"))
            .and_then(|object| object.peel_to_commit())
            .map_err(|_| self.git_op_failed("diff-tree"))?;
        let tree = commit_object
            .tree()
            .map_err(|_| self.git_op_failed("diff-tree"))?;
        let parent_tree = match commit_object.parent(0) {
            Ok(parent) => Some(parent.tree().map_err(|_| self.git_op_failed("diff-tree"))?),
            Err(_) => None,
        };
        let diff = repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
            .map_err(|_| self.git_op_failed("diff-tree"))?;
        let mut paths: Vec<String> = diff
            .deltas()
            .filter_map(|delta| {
                delta
                    .new_file()
                    .path()
                    .or(delta.old_file().path())
                    .and_then(|path| path.to_str())
                    .map(String::from)
            })
            .collect();
        paths.sort();
        Ok(paths)
    }

    pub fn unstage(
        &self,
        relative_path: &str,
        previous_head: Option<&str>,
    ) -> Result<(), MemoroError> {
        match previous_head {
            None => {
                let repo = self.open_repo("rm")?;
                let mut index = repo.index().map_err(|_| self.git_op_failed("rm"))?;
                let _ = index.remove_path(Path::new(relative_path));
                let _ = index.write();
                Ok(())
            }
            Some(head) => {
                let repo = self.open_repo("reset")?;
                let object = repo
                    .revparse_single(&format!("{head}^{{commit}}"))
                    .map_err(|_| self.git_op_failed("reset"))?;
                repo.reset_default(Some(&object), [relative_path])
                    .map_err(|_| self.git_op_failed("reset"))?;
                Ok(())
            }
        }
    }

    pub fn remote_names(&self) -> Result<BTreeSet<String>, MemoroError> {
        let repo = self.open_repo("remote")?;
        let names = repo.remotes().map_err(|_| self.git_op_failed("remote"))?;
        Ok(names
            .iter()
            .filter_map(|name| name.ok().flatten().map(String::from))
            .collect())
    }

    pub fn remote_url(&self, remote: &str) -> Result<Option<String>, MemoroError> {
        if !self.remote_names()?.contains(remote) {
            return Ok(None);
        }
        let repo = self.open_repo("remote")?;
        let config = repo.config().map_err(|_| self.git_op_failed("remote"))?;
        let fetch_urls = config_strings(&config, &format!("remote.{remote}.url"));
        let configured_push_urls = config_strings(&config, &format!("remote.{remote}.pushurl"));
        let push_urls = if configured_push_urls.is_empty() {
            fetch_urls.clone()
        } else {
            configured_push_urls
        };
        if fetch_urls.len() != 1 || push_urls.len() != 1 || fetch_urls != push_urls {
            return Err(MemoroError::Repository(format!(
                "Git remote {} has multiple or separate fetch and push URLs. Memoro left it \
                 unchanged; simplify that remote manually, then retry.",
                py_repr(remote)
            )));
        }
        Ok(fetch_urls.into_iter().next())
    }

    pub fn set_remote_url(&self, remote: &str, url: &str) -> Result<(), MemoroError> {
        let repo = self.open_repo("remote")?;
        if self.remote_names()?.contains(remote) {
            repo.remote_set_url(remote, url)
                .map_err(|_| self.git_op_failed("remote"))?;
        } else {
            repo.remote(remote, url)
                .map_err(|_| self.git_op_failed("remote"))?;
        }
        Ok(())
    }

    pub fn remove_remote(&self, remote: &str) -> Result<(), MemoroError> {
        if self.remote_names()?.contains(remote) {
            let repo = self.open_repo("remote")?;
            repo.remote_delete(remote)
                .map_err(|_| self.git_op_failed("remote"))?;
        }
        Ok(())
    }

    pub fn fetch(
        &self,
        remote: &str,
        branch: &str,
        timeout_secs: u64,
    ) -> Result<Option<String>, MemoroError> {
        if self.remote_url(remote)?.is_none() {
            return Err(MemoroError::Repository(format!(
                "Configured Git remote {} is missing from {}. Run 'memoro sync setup \
                 REPOSITORY_URL' in this Memoro home, then retry.",
                py_repr(remote),
                self.path.display()
            )));
        }
        let path = self.path.clone();
        let remote_name = remote.to_string();
        let outcome = run_with_deadline(timeout_secs, move || -> Result<(), git2::Error> {
            let repo = Repository::open(&path)?;
            let mut remote = repo.find_remote(&remote_name)?;
            let mut refspecs: Vec<String> = remote
                .fetch_refspecs()?
                .iter()
                .filter_map(|spec| spec.ok().flatten().map(String::from))
                .collect();
            if refspecs.is_empty() {
                refspecs.push(format!("+refs/heads/*:refs/remotes/{remote_name}/*"));
            }
            let mut options = FetchOptions::new();
            options
                .prune(FetchPrune::On)
                .download_tags(AutotagOption::None);
            let refspec_refs: Vec<&str> = refspecs.iter().map(String::as_str).collect();
            remote.fetch(&refspec_refs, Some(&mut options), None)?;
            Ok(())
        });
        match outcome {
            None => Err(MemoroError::Repository(format!(
                "Git timed out while synchronizing remote {}. Check the network and remote \
                 address, then retry.",
                py_repr(remote)
            ))),
            Some(Err(_)) => Err(MemoroError::Repository(format!(
                "Git could not synchronize remote {} non-interactively. Check the network, \
                 credentials, SSH host key, and remote address, then retry.",
                py_repr(remote)
            ))),
            Some(Ok(())) => self.resolve_commit(&format!("refs/remotes/{remote}/{branch}")),
        }
    }

    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, MemoroError> {
        let repo = self.open_repo("merge-base")?;
        let compare_failed = || {
            MemoroError::Repository(format!(
                "Git could not compare commits in {}. Inspect the repository, then retry.",
                self.path.display()
            ))
        };
        let resolve = |revision: &str| {
            repo.revparse_single(&format!("{revision}^{{commit}}"))
                .and_then(|object| object.peel_to_commit().map(|commit| commit.id()))
        };
        let ancestor_id = resolve(ancestor).map_err(|_| compare_failed())?;
        let descendant_id = resolve(descendant).map_err(|_| compare_failed())?;
        match repo.merge_base(ancestor_id, descendant_id) {
            Ok(base) => Ok(base == ancestor_id),
            Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(false),
            Err(_) => Err(compare_failed()),
        }
    }

    pub fn reset_to(&self, commit: &str) -> Result<(), MemoroError> {
        self.assert_clean()?;
        let repo = self.open_repo("reset")?;
        let object = repo
            .revparse_single(&format!("{commit}^{{commit}}"))
            .map_err(|_| self.git_op_failed("reset"))?;
        repo.reset(&object, ResetType::Hard, None)
            .map_err(|_| self.git_op_failed("reset"))?;
        Ok(())
    }

    pub fn sync_conflict_commit(&self) -> Result<Option<String>, MemoroError> {
        self.resolve_commit(SYNC_CONFLICT_REF)
    }

    pub fn mark_sync_conflict(&self, commit: &str) -> Result<(), MemoroError> {
        let repo = self.open_repo("update-ref")?;
        let oid = git2::Oid::from_str(commit).map_err(|_| self.git_op_failed("update-ref"))?;
        repo.find_commit(oid)
            .map_err(|_| self.git_op_failed("update-ref"))?;
        repo.reference(SYNC_CONFLICT_REF, oid, true, "")
            .map_err(|_| self.git_op_failed("update-ref"))?;
        Ok(())
    }

    pub fn clear_sync_conflict(&self) -> Result<(), MemoroError> {
        let repo = self.open_repo("update-ref")?;
        let reference = repo.find_reference(SYNC_CONFLICT_REF);
        match reference {
            Ok(mut reference) => reference
                .delete()
                .map_err(|_| self.git_op_failed("update-ref")),
            // `git update-ref -d` succeeds when the ref is already gone.
            Err(_) => Ok(()),
        }
    }

    /// Stores deploy-key settings in the local Git config, mirroring
    /// `configure_deploy_key`. Local-path remotes never consult `core.sshCommand`.
    pub fn configure_deploy_key(
        &self,
        private_key: &Path,
        known_hosts: &Path,
    ) -> Result<(), MemoroError> {
        let command = [
            "ssh",
            "-i",
            &private_key.display().to_string(),
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            &format!("UserKnownHostsFile={}", known_hosts.display()),
            "-o",
            "StrictHostKeyChecking=accept-new",
        ]
        .iter()
        .map(|value| shell_quote(value))
        .collect::<Vec<String>>()
        .join(" ");
        let repo = self.open_repo("config")?;
        let mut config = repo.config().map_err(|_| self.git_op_failed("config"))?;
        config
            .set_str("core.sshCommand", &command)
            .map_err(|_| self.git_op_failed("config"))?;
        config
            .set_str("memoro.syncAuth", "deploy-key")
            .map_err(|_| self.git_op_failed("config"))?;
        config
            .set_str("memoro.deployKeyPath", &private_key.display().to_string())
            .map_err(|_| self.git_op_failed("config"))?;
        Ok(())
    }

    pub fn deploy_key_path(&self) -> Result<Option<PathBuf>, MemoroError> {
        let repo = self.open_repo("config")?;
        let config = repo.config().map_err(|_| self.git_op_failed("config"))?;
        match config.get_string("memoro.syncAuth") {
            Ok(auth) if auth.trim() == "deploy-key" => {}
            _ => return Ok(None),
        }
        let stored = config
            .get_string("memoro.deployKeyPath")
            .unwrap_or_default();
        let trimmed = stored.trim();
        if trimmed.is_empty() {
            return Err(MemoroError::Repository(
                "The memory repository declares deploy-key authentication without a key path. \
                 Run 'memoro sync setup REPOSITORY_URL --deploy-key' to repair it."
                    .to_string(),
            ));
        }
        Ok(Some(resolve_lazy(Path::new(trimmed))))
    }

    pub fn remote_heads(
        &self,
        url: &str,
        timeout_secs: u64,
    ) -> Result<BTreeMap<String, String>, MemoroError> {
        let path = self.path.clone();
        let url = url.to_string();
        let outcome = run_with_deadline(
            timeout_secs,
            move || -> Result<Vec<(String, git2::Oid)>, git2::Error> {
                let repo = Repository::open(&path)?;
                let mut remote = repo.remote_anonymous(&url)?;
                remote.connect(git2::Direction::Fetch)?;
                let heads = remote.list()?;
                Ok(heads
                    .iter()
                    .filter_map(|head| {
                        head.name()
                            .strip_prefix("refs/heads/")
                            .map(|name| (name.to_string(), head.oid()))
                    })
                    .collect())
            },
        );
        match outcome {
            None => Err(MemoroError::Repository(
                "Git timed out while checking the synchronized repository. Check the network and \
                 remote address, then retry."
                    .to_string(),
            )),
            Some(Err(_)) => Err(MemoroError::Repository(
                "Git could not read the synchronized repository non-interactively. Confirm the \
                 repository address and prepare HTTPS credentials in Git Credential Manager or \
                 load the SSH key into an agent, then retry."
                    .to_string(),
            )),
            Some(Ok(heads)) => Ok(heads
                .into_iter()
                .map(|(name, oid)| (name, oid.to_string()))
                .collect()),
        }
    }

    /// Verifies that local `branch` can be pushed to `remote` (a configured
    /// remote name or a raw URL). Uses the reference advertisement plus a
    /// fast-forward check instead of `git push --dry-run`, which libgit2 does
    /// not expose.
    pub fn verify_push(
        &self,
        remote: &str,
        branch: &str,
        timeout_secs: u64,
    ) -> Result<(), MemoroError> {
        let url = match self.remote_names()?.contains(remote) {
            true => self
                .remote_url(remote)?
                .unwrap_or_else(|| remote.to_string()),
            false => remote.to_string(),
        };
        let path = self.path.clone();
        let url_for_thread = url.clone();
        let branch_for_thread = branch.to_string();
        let outcome = run_with_deadline(
            timeout_secs,
            move || -> Result<Option<git2::Oid>, git2::Error> {
                let repo = Repository::open(&path)?;
                let mut anonymous = repo.remote_anonymous(&url_for_thread)?;
                anonymous.connect(git2::Direction::Fetch)?;
                let heads = anonymous.list()?;
                let target = format!("refs/heads/{branch_for_thread}");
                Ok(heads
                    .iter()
                    .find(|head| head.name() == target)
                    .map(|head| head.oid()))
            },
        );
        let verify_failed = || {
            MemoroError::Repository(format!(
                "Git could not verify write access for local branch {}. Confirm that the \
                 authenticated account can push to the repository and that branch rules allow \
                 the push, then retry.",
                py_repr(branch)
            ))
        };
        let remote_head =
            match outcome {
                None => return Err(MemoroError::Repository(
                    "Git timed out while checking write access to the synchronized repository. \
                     Check the network and credentials, then retry."
                        .to_string(),
                )),
                Some(Err(_)) => return Err(verify_failed()),
                Some(Ok(head)) => head,
            };
        let repo = self.open_repo("push")?;
        let local_id = repo
            .revparse_single(&format!("refs/heads/{branch}"))
            .and_then(|object| object.peel_to_commit().map(|commit| commit.id()))
            .map_err(|_| verify_failed())?;
        let Some(remote_id) = remote_head else {
            return Ok(());
        };
        if remote_id == local_id {
            return Ok(());
        }
        match repo.merge_base(local_id, remote_id) {
            Ok(base) if base == remote_id => Ok(()),
            _ => Err(MemoroError::Repository(format!(
                "The synchronized repository has history incompatible with local branch {}. \
                 Memoro did not fetch, merge, or force-push it. Use an empty repository or \
                 integrate the histories manually, then retry.",
                py_repr(branch)
            ))),
        }
    }

    pub fn push(
        &self,
        remote: &str,
        branch: &str,
        timeout_secs: u64,
    ) -> Result<PushOutcome, MemoroError> {
        let Some(candidate) = self.head()? else {
            return Ok(PushOutcome {
                attempted: false,
                succeeded: false,
                reason: "no-commit".to_string(),
            });
        };
        let refspec = format!("{candidate}:refs/heads/{branch}");
        let path = self.path.clone();
        let remote_name = remote.to_string();
        let outcome = run_with_deadline(timeout_secs, move || -> Result<(), git2::Error> {
            let repo = Repository::open(&path)?;
            let mut remote = repo.find_remote(&remote_name)?;
            let rejected = Arc::new(Mutex::new(false));
            let rejected_flag = Arc::clone(&rejected);
            let mut callbacks = RemoteCallbacks::new();
            callbacks.push_update_reference(move |_refname, status| {
                if status.is_some() {
                    *rejected_flag.lock().unwrap() = true;
                }
                Ok(())
            });
            let mut options = PushOptions::new();
            options.remote_callbacks(callbacks);
            remote.push(&[&refspec], Some(&mut options))?;
            if *rejected.lock().unwrap() {
                return Err(git2::Error::from_str("pushed reference was rejected"));
            }
            Ok(())
        });
        match outcome {
            None => Ok(PushOutcome {
                attempted: true,
                succeeded: false,
                reason: "timeout".to_string(),
            }),
            Some(Err(_)) => Ok(PushOutcome {
                attempted: true,
                succeeded: false,
                reason: "failed".to_string(),
            }),
            Some(Ok(())) => Ok(PushOutcome {
                attempted: true,
                succeeded: true,
                reason: "pushed".to_string(),
            }),
        }
    }

    fn commit_signature(&self, repo: &Repository) -> Result<Signature<'static>, MemoroError> {
        let config = repo.config().ok();
        let name = config
            .as_ref()
            .and_then(|config| config.get_string("user.name").ok())
            .or_else(|| env::var("MEMORO_GIT_NAME").ok())
            .unwrap_or_else(|| GIT_IDENTITY_NAME.to_owned());
        let email = config
            .as_ref()
            .and_then(|config| config.get_string("user.email").ok())
            .or_else(|| env::var("MEMORO_GIT_EMAIL").ok())
            .unwrap_or_else(|| GIT_IDENTITY_EMAIL.to_owned());
        Signature::now(&name, &email).map_err(|_| self.git_op_failed("commit"))
    }

    fn open_repo(&self, operation: &str) -> Result<Repository, MemoroError> {
        Repository::open(&self.path).map_err(|_| self.git_op_failed(operation))
    }

    fn git_op_failed(&self, operation: &str) -> MemoroError {
        MemoroError::Repository(format!(
            "Git operation '{operation}' failed in {}. Inspect the repository and Git \
             configuration, then retry.",
            self.path.display()
        ))
    }

    fn not_on_a_branch(&self) -> MemoroError {
        MemoroError::Repository(format!(
            "Memory repository {} is not on a branch. Check out a branch before writing \
             memories.",
            self.path.display()
        ))
    }
}

/// Environment scrubbed the way the Python implementation scrubs it for
/// subprocess Git calls. Retained for parity; the in-process git2 usage is not
/// influenced by these variables.
pub fn git_environment() -> HashMap<String, String> {
    git_environment_from(env::vars())
}

pub fn git_environment_from<I>(environment: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut scrubbed: HashMap<String, String> = environment
        .into_iter()
        .filter(|(name, _)| {
            !REPOSITORY_CONTROL_ENV.contains(&name.as_str())
                && !name.starts_with("GIT_AUTHOR_")
                && !name.starts_with("GIT_COMMITTER_")
                && !name.starts_with("GIT_CONFIG_")
        })
        .collect();
    scrubbed.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());
    scrubbed.insert("GCM_INTERACTIVE".to_string(), "Never".to_string());
    scrubbed.insert("SSH_ASKPASS_REQUIRE".to_string(), "never".to_string());
    scrubbed.insert("LC_ALL".to_string(), "C".to_string());
    scrubbed
}

fn collect_memory_paths(
    repo: &Repository,
    tree: &git2::Tree,
    prefix: &str,
    commit: &str,
    paths: &mut Vec<String>,
) -> Result<(), MemoroError> {
    for entry in tree.iter() {
        let raw_name = entry.name_bytes();
        let name = match std::str::from_utf8(raw_name) {
            Ok(name) => name,
            Err(_) => {
                return Err(MemoroError::Repository(format!(
                    "Git tree {commit} contains a non-UTF-8 path or mode. Repair the repository \
                     before retrying."
                )))
            }
        };
        let path = format!("{prefix}{name}");
        if entry.kind() == Some(ObjectType::Tree) {
            let subtree = repo.find_tree(entry.id()).map_err(|_| {
                MemoroError::Repository(format!(
                    "Git tree {commit} contains an unreadable entry. Inspect the memory \
                     repository before retrying."
                ))
            })?;
            collect_memory_paths(repo, &subtree, &format!("{path}/"), commit, paths)?;
            continue;
        }
        if !path.ends_with(".md") {
            continue;
        }
        if entry.kind() != Some(ObjectType::Blob) || !REGULAR_FILE_MODES.contains(&entry.filemode())
        {
            return Err(MemoroError::Repository(format!(
                "Committed memory {} is not a regular file. Replace it with a regular UTF-8 \
                 Markdown file and commit the correction.",
                py_repr(&path)
            )));
        }
        paths.push(path);
    }
    Ok(())
}

fn run_with_deadline<T, F>(timeout_secs: u64, operation: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(operation());
    });
    receiver
        .recv_timeout(Duration::from_secs(timeout_secs))
        .ok()
}

fn validate_relative_path(relative_path: &str) -> Result<(), MemoroError> {
    let not_safe = || {
        MemoroError::Repository(format!(
            "Memory path {} is not a safe relative path.",
            py_repr(relative_path)
        ))
    };
    if relative_path.starts_with('/') {
        return Err(not_safe());
    }
    let parts = posix_parts(relative_path);
    if parts.is_empty() || parts.iter().any(|part| part == "..") {
        return Err(not_safe());
    }
    if relative_path.contains('\\') {
        return Err(MemoroError::Repository(format!(
            "Memory path {} contains a backslash.",
            py_repr(relative_path)
        )));
    }
    Ok(())
}

fn posix_parts(relative_path: &str) -> Vec<String> {
    relative_path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(String::from)
        .collect()
}

fn directory_is_empty(path: &Path) -> bool {
    match fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => false,
    }
}

fn config_strings(config: &git2::Config, name: &str) -> Vec<String> {
    let mut values = Vec::new();
    if let Ok(mut entries) = config.multivar(name, None) {
        while let Some(Ok(entry)) = entries.next() {
            if let Ok(value) = entry.value() {
                values.push(value.to_string());
            }
        }
    }
    values
}

fn env_identity(variable: &str, fallback: &str) -> String {
    env::var(variable).unwrap_or_else(|_| fallback.to_owned())
}

fn resolve_lazy(path: &Path) -> PathBuf {
    if let Ok(resolved) = fs::canonicalize(path) {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => match fs::canonicalize(parent) {
            Ok(resolved_parent) => resolved_parent.join(name),
            Err(_) => normalize_lexical(path),
        },
        _ => normalize_lexical(path),
    }
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn same_path(left: &Path, right: &Path) -> bool {
    resolve_lazy(left) == resolve_lazy(right)
}

/// Renders `value` the way Python's `repr()` renders a `str`.
fn py_repr(value: &str) -> String {
    if value.contains('\'') && !value.contains('"') {
        format!("\"{value}\"")
    } else {
        format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// Mirrors `shlex.quote` from the Python standard library.
fn shell_quote(value: &str) -> String {
    let safe = value.chars().all(|c| {
        c.is_alphanumeric()
            || matches!(c, '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-')
    });
    if safe {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}
