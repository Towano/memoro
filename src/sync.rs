//! Backup synchronization orchestration for Memoro memories.
//!
//! New module with no Python counterpart: [`SyncEngine`] composes the fixed
//! [`GitRepository`] surface (`fetch`, `is_ancestor`, `reset_to`,
//! `mark_sync_conflict`, `clear_sync_conflict`, `verify_push`, `push`) into
//! the pull/push workflows the service and server layers expose. Pull only
//! ever fast-forwards the local branch (never merges, never force-pushes,
//! never discards local commits); diverged histories are recorded with the
//! sync-conflict marker instead. Push never pulls implicitly: a remote that
//! cannot be fast-forwarded is reported as a "pull first" rejection.

use serde::Serialize;

use crate::errors::MemoroError;
use crate::git::GitRepository;

/// Action taken by a [`SyncEngine::pull`] or [`SyncEngine::push`] run.
///
/// Serialized in kebab-case: `"fast-forwarded"`, `"no-change"`, `"conflict"`,
/// `"pushed"`, `"no-commit"`, `"timeout"`, `"failed"`,
/// `"rejected-non-fast-forward"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncAction {
    FastForwarded,
    NoChange,
    Conflict,
    Pushed,
    NoCommit,
    Timeout,
    Failed,
    RejectedNonFastForward,
}

/// Snapshot of the local branch versus its remote tracking branch, taken by
/// [`SyncEngine::status`].
#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub remote: String,
    pub branch: String,
    pub local_head: Option<String>,
    pub remote_head: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub conflict_commit: Option<String>,
}

/// Result of a single [`SyncEngine::pull`] or [`SyncEngine::push`] run.
#[derive(Debug, Clone, Serialize)]
pub struct SyncOutcome {
    pub action: SyncAction,
    pub remote: String,
    pub branch: String,
    pub local_head: Option<String>,
    pub remote_head: Option<String>,
    pub message: String,
}

/// Orchestrates backup synchronization for one memory repository.
///
/// The engine assumes `branch` is the currently checked-out branch of the
/// wrapped repository, matching how the service layer drives it.
#[derive(Debug)]
pub struct SyncEngine {
    repository: GitRepository,
}

impl SyncEngine {
    pub fn new(repository: GitRepository) -> Self {
        Self { repository }
    }

    /// The wrapped repository.
    pub fn repository(&self) -> &GitRepository {
        &self.repository
    }

    pub fn status(
        &self,
        remote: &str,
        branch: &str,
        timeout_secs: u64,
    ) -> Result<SyncStatus, MemoroError> {
        let remote_head = self.repository.fetch(remote, branch, timeout_secs)?;
        let local_head = self.branch_head(branch)?;
        let (ahead, behind) = self.ahead_behind(local_head.as_deref(), remote_head.as_deref())?;
        let conflict_commit = self.repository.sync_conflict_commit()?;
        Ok(SyncStatus {
            remote: remote.to_string(),
            branch: branch.to_string(),
            local_head,
            remote_head,
            ahead,
            behind,
            conflict_commit,
        })
    }

    pub fn pull(
        &self,
        remote: &str,
        branch: &str,
        timeout_secs: u64,
    ) -> Result<SyncOutcome, MemoroError> {
        let remote_head = self.repository.fetch(remote, branch, timeout_secs)?;
        let local_head = self.branch_head(branch)?;
        let (action, message) = match (&local_head, &remote_head) {
            (_, None) => (
                SyncAction::NoChange,
                format!("Remote branch {branch} does not exist; nothing to pull."),
            ),
            (None, Some(remote_commit)) => {
                self.repository.reset_to(remote_commit)?;
                self.repository.clear_sync_conflict()?;
                (
                    SyncAction::FastForwarded,
                    format!("Fast-forwarded local branch {branch} to the remote commit."),
                )
            }
            (Some(local_commit), Some(remote_commit)) => {
                if local_commit == remote_commit {
                    (
                        SyncAction::NoChange,
                        format!("Local branch {branch} already matches the remote."),
                    )
                } else if self.repository.is_ancestor(local_commit, remote_commit)? {
                    self.repository.reset_to(remote_commit)?;
                    self.repository.clear_sync_conflict()?;
                    (
                        SyncAction::FastForwarded,
                        format!("Fast-forwarded local branch {branch} to the remote commit."),
                    )
                } else if self.repository.is_ancestor(remote_commit, local_commit)? {
                    (
                        SyncAction::NoChange,
                        format!("Local branch {branch} is ahead of the remote; nothing to pull."),
                    )
                } else {
                    self.repository.mark_sync_conflict(local_commit)?;
                    (
                        SyncAction::Conflict,
                        format!(
                            "Local branch {branch} diverged from the remote. Memoro kept local \
                             commit {local_commit} and marked a sync conflict; resolve the \
                             histories manually, then retry."
                        ),
                    )
                }
            }
        };
        Ok(outcome(
            action,
            remote,
            branch,
            local_head,
            remote_head,
            message,
        ))
    }

    pub fn push(
        &self,
        remote: &str,
        branch: &str,
        timeout_secs: u64,
    ) -> Result<SyncOutcome, MemoroError> {
        let Some(local_head) = self.repository.head()? else {
            return Ok(outcome(
                SyncAction::NoCommit,
                remote,
                branch,
                None,
                None,
                "No local commit to push.".to_string(),
            ));
        };
        let tracked_remote_head = self
            .repository
            .resolve_commit(&format!("refs/remotes/{remote}/{branch}"))?;
        if let Err(error) = self.repository.verify_push(remote, branch, timeout_secs) {
            // `verify_push` distinguishes its failures only through the
            // message text; those messages are pinned by `tests/git.rs`.
            let text = error.to_string();
            let (action, message) = if text.contains("timed out while checking write access") {
                (
                    SyncAction::Timeout,
                    "Git timed out while checking write access to the synchronized repository. \
                     Check the network and credentials, then retry."
                        .to_string(),
                )
            } else if text.contains("history incompatible") {
                (
                    SyncAction::RejectedNonFastForward,
                    format!(
                        "The synchronized repository has commits that local branch {branch} does \
                         not include. Pull first, then push again."
                    ),
                )
            } else {
                (
                    SyncAction::Failed,
                    format!(
                        "Git could not verify write access for local branch {branch}. Confirm \
                         that the authenticated account can push to the repository and that \
                         branch rules allow the push, then retry."
                    ),
                )
            };
            return Ok(outcome(
                action,
                remote,
                branch,
                Some(local_head),
                tracked_remote_head,
                message,
            ));
        }
        let pushed = self.repository.push(remote, branch, timeout_secs)?;
        let (action, message) = match pushed.reason.as_str() {
            "pushed" => (
                SyncAction::Pushed,
                format!("Pushed local branch {branch} to the remote."),
            ),
            "no-commit" => (SyncAction::NoCommit, "No local commit to push.".to_string()),
            "timeout" => (
                SyncAction::Timeout,
                "Git timed out while pushing to the synchronized repository. Check the network \
                 and credentials, then retry."
                    .to_string(),
            ),
            _ => (
                SyncAction::Failed,
                "Git could not push to the synchronized repository. Check the network, \
                 credentials, and remote address, then retry."
                    .to_string(),
            ),
        };
        // A successful update means the remote branch now points at the
        // pushed commit; libgit2 does not refresh remote-tracking refs.
        let remote_head = if action == SyncAction::Pushed {
            Some(local_head.clone())
        } else {
            tracked_remote_head
        };
        Ok(outcome(
            action,
            remote,
            branch,
            Some(local_head),
            remote_head,
            message,
        ))
    }

    fn branch_head(&self, branch: &str) -> Result<Option<String>, MemoroError> {
        self.repository
            .resolve_commit(&format!("refs/heads/{branch}"))
    }

    fn ahead_behind(
        &self,
        local_head: Option<&str>,
        remote_head: Option<&str>,
    ) -> Result<(usize, usize), MemoroError> {
        let (Some(local), Some(remote)) = (local_head, remote_head) else {
            return Ok((0, 0));
        };
        let compare_failed = || {
            MemoroError::Repository(format!(
                "Git could not compare commits in {}. Inspect the repository, then retry.",
                self.repository.path().display()
            ))
        };
        let repo = git2::Repository::open(self.repository.path()).map_err(|_| compare_failed())?;
        let local_oid = git2::Oid::from_str(local).map_err(|_| compare_failed())?;
        let remote_oid = git2::Oid::from_str(remote).map_err(|_| compare_failed())?;
        repo.graph_ahead_behind(local_oid, remote_oid)
            .map_err(|_| compare_failed())
    }
}

fn outcome(
    action: SyncAction,
    remote: &str,
    branch: &str,
    local_head: Option<String>,
    remote_head: Option<String>,
    message: String,
) -> SyncOutcome {
    SyncOutcome {
        action,
        remote: remote.to_string(),
        branch: branch.to_string(),
        local_head,
        remote_head,
        message,
    }
}
