//! Locked multi-space application service for Memoro.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::{self, RuntimePaths, SpaceSettings};
use crate::errors::MemoroError;
use crate::git::GitRepository;
use crate::locking::RepositoryLocks;
use crate::markdown::memory_revision;
use crate::models::{Memory, MutationReceipt, PatchEdit};
use crate::search::search_memories;
use crate::store::MemoryStore;
use crate::sync::{SyncEngine, SyncOutcome, SyncStatus};

pub const DEFAULT_SYNC_REMOTE: &str = "origin";
pub const DEFAULT_SYNC_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone, Serialize)]
pub struct SpaceInfo {
    pub space: String,
    pub readonly: bool,
    pub initialized: bool,
    pub head: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryResult {
    pub space: String,
    pub memory: Memory,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryListResult {
    pub space: String,
    pub commit: Option<String>,
    pub memories: Vec<MemoryResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryMutationResult {
    pub space: String,
    pub receipt: MutationReceipt,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySearchHit {
    pub memory: MemoryResult,
    pub score: f64,
    pub snippet: String,
    pub snippet_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySearchResult {
    pub hits: Vec<MemorySearchHit>,
    pub total_matches: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncStatusResult {
    pub space: String,
    pub initialized: bool,
    pub remote: String,
    pub branch: String,
    pub status: Option<SyncStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncResult {
    pub space: String,
    pub initialized: bool,
    pub outcome: SyncOutcome,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MemoryGetRequest {
    pub space: Option<String>,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MemoryListRequest {
    pub space: Option<String>,
    pub kind: Option<String>,
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MemoryCreateRequest {
    pub space: Option<String>,
    pub title: String,
    pub summary: String,
    pub body: String,
    pub kind: String,
    pub project: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MemoryPatchRequest {
    pub space: Option<String>,
    pub id: String,
    pub base_revision: String,
    pub edits: Vec<PatchEdit>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MemoryReplaceRequest {
    pub space: Option<String>,
    pub id: String,
    pub base_revision: String,
    pub summary: String,
    pub body: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MemoryDeleteRequest {
    pub space: Option<String>,
    pub id: String,
    pub expected_title: String,
    pub base_revision: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MemorySearchRequest {
    /// `None` searches every registered space; `Some` searches one space.
    pub space: Option<String>,
    pub query: String,
    pub kind: Option<String>,
    pub project: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SyncRequest {
    pub space: Option<String>,
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub timeout_secs: Option<u64>,
}

/// Application boundary for all memory repository operations.
#[derive(Debug, Clone)]
pub struct Service {
    home: PathBuf,
    paths: RuntimePaths,
    spaces: BTreeMap<String, SpaceSettings>,
}

impl Service {
    pub fn open(home: PathBuf) -> Result<Self, MemoroError> {
        let spaces = config::load_spaces(&home)?;
        Ok(Self {
            paths: RuntimePaths::new(home.clone()),
            home,
            spaces,
        })
    }

    pub fn spaces_list(&self) -> Result<Vec<SpaceInfo>, MemoroError> {
        self.spaces
            .keys()
            .map(|name| {
                self.with_shared(name, |space| {
                    let repository = self.read_repository(&space)?;
                    let initialized = repository.is_some();
                    let head = repository
                        .as_ref()
                        .map(GitRepository::head)
                        .transpose()?
                        .flatten();
                    Ok(SpaceInfo {
                        space: space.name,
                        readonly: space.settings.readonly,
                        initialized,
                        head,
                    })
                })
            })
            .collect()
    }

    pub fn memory_get(&self, request: MemoryGetRequest) -> Result<MemoryResult, MemoroError> {
        let requested_id = request.id;
        self.with_shared_option(request.space.as_deref(), |space| {
            let Some(repository) = self.read_repository(&space)? else {
                return Err(memory_not_found(&space.name, &requested_id));
            };
            let snapshot = MemoryStore::new(repository).snapshot()?;
            let memory = snapshot
                .memories
                .into_iter()
                .find(|memory| memory.id == requested_id)
                .ok_or_else(|| memory_not_found(&space.name, &requested_id))?;
            memory_result(space.name, memory)
        })
    }

    pub fn memory_list(&self, request: MemoryListRequest) -> Result<MemoryListResult, MemoroError> {
        self.with_shared_option(request.space.as_deref(), |space| {
            let Some(repository) = self.read_repository(&space)? else {
                return Ok(MemoryListResult {
                    space: space.name,
                    commit: None,
                    memories: Vec::new(),
                });
            };
            let snapshot = MemoryStore::new(repository).snapshot()?;
            let mut memories: Vec<Memory> = snapshot
                .memories
                .into_iter()
                .filter(|memory| {
                    request
                        .kind
                        .as_deref()
                        .is_none_or(|kind| memory.kind == kind)
                        && request
                            .project
                            .as_deref()
                            .is_none_or(|project| memory.project.as_deref() == Some(project))
                })
                .collect();
            memories.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
            Ok(MemoryListResult {
                space: space.name.clone(),
                commit: snapshot.commit,
                memories: memories
                    .into_iter()
                    .map(|memory| memory_result(space.name.clone(), memory))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
    }

    pub fn memory_create(
        &self,
        request: MemoryCreateRequest,
    ) -> Result<MemoryMutationResult, MemoroError> {
        self.with_exclusive_option(request.space.as_deref(), |space| {
            self.require_writable(&space)?;
            let receipt = MemoryStore::new(self.writable_repository(&space)?).create(
                &request.title,
                &request.summary,
                &request.body,
                &request.kind,
                request.project.as_deref(),
                &request.tags,
            )?;
            mutation_result(space.name, receipt)
        })
    }

    pub fn memory_patch(
        &self,
        request: MemoryPatchRequest,
    ) -> Result<MemoryMutationResult, MemoroError> {
        self.with_exclusive_option(request.space.as_deref(), |space| {
            self.require_writable(&space)?;
            let receipt = MemoryStore::new(self.writable_repository(&space)?).patch(
                &request.id,
                &request.base_revision,
                &request.edits,
                request.summary.as_deref(),
            )?;
            mutation_result(space.name, receipt)
        })
    }

    pub fn memory_replace(
        &self,
        request: MemoryReplaceRequest,
    ) -> Result<MemoryMutationResult, MemoroError> {
        self.with_exclusive_option(request.space.as_deref(), |space| {
            self.require_writable(&space)?;
            let receipt = MemoryStore::new(self.writable_repository(&space)?).replace(
                &request.id,
                &request.base_revision,
                &request.summary,
                &request.body,
                &request.tags,
            )?;
            mutation_result(space.name, receipt)
        })
    }

    pub fn memory_delete(
        &self,
        request: MemoryDeleteRequest,
    ) -> Result<MemoryMutationResult, MemoroError> {
        self.with_exclusive_option(request.space.as_deref(), |space| {
            self.require_writable(&space)?;
            let receipt = MemoryStore::new(self.writable_repository(&space)?).delete(
                &request.id,
                &request.expected_title,
                &request.base_revision,
            )?;
            mutation_result(space.name, receipt)
        })
    }

    pub fn memory_search(
        &self,
        request: MemorySearchRequest,
    ) -> Result<MemorySearchResult, MemoroError> {
        let names = match request.space.as_deref() {
            Some(name) => vec![self.resolve_space(Some(name))?.name],
            None => self.spaces.keys().cloned().collect(),
        };
        let mut entries: Vec<(String, Memory)> = Vec::new();
        for name in names {
            let mut memories = self.with_shared(&name, |space| {
                let Some(repository) = self.read_repository(&space)? else {
                    return Ok(Vec::new());
                };
                Ok(MemoryStore::new(repository)
                    .snapshot()?
                    .memories
                    .into_iter()
                    .map(|memory| (space.name.clone(), memory))
                    .collect())
            })?;
            entries.append(&mut memories);
        }
        let references: Vec<(&str, &Memory)> = entries
            .iter()
            .map(|(space, memory)| (space.as_str(), memory))
            .collect();
        let result = search_memories(
            &references,
            &request.query,
            request.kind.as_deref(),
            request.project.as_deref(),
            request.limit.unwrap_or(crate::search::DEFAULT_LIMIT),
        )?;
        Ok(MemorySearchResult {
            hits: result
                .hits
                .into_iter()
                .map(|hit| {
                    Ok(MemorySearchHit {
                        memory: memory_result(hit.space, hit.memory)?,
                        score: hit.score,
                        snippet: hit.snippet,
                        snippet_truncated: hit.snippet_truncated,
                    })
                })
                .collect::<Result<Vec<_>, MemoroError>>()?,
            total_matches: result.total_matches,
            truncated: result.truncated,
        })
    }

    pub fn sync_status(&self, request: SyncRequest) -> Result<SyncStatusResult, MemoroError> {
        self.with_shared_option(request.space.as_deref(), |space| {
            let remote = request
                .remote
                .clone()
                .unwrap_or_else(|| DEFAULT_SYNC_REMOTE.to_string());
            let timeout_secs = request.timeout_secs.unwrap_or(DEFAULT_SYNC_TIMEOUT_SECS);
            let Some(repository) = self.read_repository(&space)? else {
                return Ok(SyncStatusResult {
                    space: space.name,
                    initialized: false,
                    remote,
                    branch: request.branch.clone().unwrap_or_else(|| "main".to_string()),
                    status: None,
                });
            };
            let branch = sync_branch(&repository, request.branch.as_deref())?;
            let status = SyncEngine::new(repository).status(&remote, &branch, timeout_secs)?;
            Ok(SyncStatusResult {
                space: space.name,
                initialized: true,
                remote,
                branch,
                status: Some(status),
            })
        })
    }

    pub fn sync_pull(&self, request: SyncRequest) -> Result<SyncResult, MemoroError> {
        self.with_exclusive_option(request.space.as_deref(), |space| {
            self.require_writable(&space)?;
            let repository = self.writable_repository(&space)?;
            let remote = request
                .remote
                .clone()
                .unwrap_or_else(|| DEFAULT_SYNC_REMOTE.to_string());
            let branch = sync_branch(&repository, request.branch.as_deref())?;
            let outcome = SyncEngine::new(repository).pull(
                &remote,
                &branch,
                request.timeout_secs.unwrap_or(DEFAULT_SYNC_TIMEOUT_SECS),
            )?;
            Ok(SyncResult {
                space: space.name,
                initialized: true,
                outcome,
            })
        })
    }

    pub fn sync_push(&self, request: SyncRequest) -> Result<SyncResult, MemoroError> {
        self.with_exclusive_option(request.space.as_deref(), |space| {
            self.require_writable(&space)?;
            let repository = self.writable_repository(&space)?;
            let remote = request
                .remote
                .clone()
                .unwrap_or_else(|| DEFAULT_SYNC_REMOTE.to_string());
            let branch = sync_branch(&repository, request.branch.as_deref())?;
            let outcome = SyncEngine::new(repository).push(
                &remote,
                &branch,
                request.timeout_secs.unwrap_or(DEFAULT_SYNC_TIMEOUT_SECS),
            )?;
            Ok(SyncResult {
                space: space.name,
                initialized: true,
                outcome,
            })
        })
    }

    fn with_shared<R>(
        &self,
        requested_space: &str,
        body: impl FnOnce(SpaceAccess) -> Result<R, MemoroError>,
    ) -> Result<R, MemoroError> {
        let space = self.resolve_space(Some(requested_space))?;
        let locks = space.locks.clone();
        locks.shared(|| body(space)).and_then(|result| result)
    }

    fn with_shared_option<R>(
        &self,
        requested_space: Option<&str>,
        body: impl FnOnce(SpaceAccess) -> Result<R, MemoroError>,
    ) -> Result<R, MemoroError> {
        let space = self.resolve_space(requested_space)?;
        let locks = space.locks.clone();
        locks.shared(|| body(space)).and_then(|result| result)
    }

    fn with_exclusive_option<R>(
        &self,
        requested_space: Option<&str>,
        body: impl FnOnce(SpaceAccess) -> Result<R, MemoroError>,
    ) -> Result<R, MemoroError> {
        let space = self.resolve_space(requested_space)?;
        let locks = space.locks.clone();
        locks.exclusive(|| body(space)).and_then(|result| result)
    }

    fn resolve_space(&self, requested_space: Option<&str>) -> Result<SpaceAccess, MemoroError> {
        let name = match requested_space {
            Some(name) => config::normalize_space_name(name)?,
            None => config::DEFAULT_SPACE.to_string(),
        };
        let settings = self.spaces.get(&name).cloned().ok_or_else(|| {
            MemoroError::Configuration(format!(
                "Space '{name}' is not registered in this Memoro home. Add it to config.json before using it."
            ))
        })?;
        let path = self.paths.space(&name)?;
        Ok(SpaceAccess {
            locks: RepositoryLocks::new(self.home.join(".memoro-locks").join(&name)),
            name,
            path,
            settings,
        })
    }

    fn read_repository(&self, space: &SpaceAccess) -> Result<Option<GitRepository>, MemoroError> {
        if space.path.join(".git").exists() {
            GitRepository::open(&space.path).map(Some)
        } else {
            Ok(None)
        }
    }

    fn writable_repository(&self, space: &SpaceAccess) -> Result<GitRepository, MemoroError> {
        match self.read_repository(space)? {
            Some(repository) => Ok(repository),
            None => GitRepository::initialize(&space.path),
        }
    }

    fn require_writable(&self, space: &SpaceAccess) -> Result<(), MemoroError> {
        if space.settings.readonly {
            return Err(MemoroError::Repository(format!(
                "Space '{}' is readonly and does not allow write or sync operations.",
                space.name
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct SpaceAccess {
    locks: RepositoryLocks,
    name: String,
    path: PathBuf,
    settings: SpaceSettings,
}

fn memory_result(space: String, memory: Memory) -> Result<MemoryResult, MemoroError> {
    let revision = memory_revision(&memory).map_err(MemoroError::MemoryValidation)?;
    Ok(MemoryResult {
        space,
        memory,
        revision,
    })
}

fn mutation_result(
    space: String,
    receipt: MutationReceipt,
) -> Result<MemoryMutationResult, MemoroError> {
    let revision = memory_revision(&receipt.memory).map_err(MemoroError::MemoryValidation)?;
    Ok(MemoryMutationResult {
        space,
        receipt,
        revision,
    })
}

fn memory_not_found(space: &str, memory_id: &str) -> MemoroError {
    MemoroError::MemoryNotFound(format!(
        "Memory {memory_id} was not found in the committed snapshot for space '{space}'. List or search memories again, then retry with a current memory ID."
    ))
}

fn sync_branch(
    repository: &GitRepository,
    requested_branch: Option<&str>,
) -> Result<String, MemoroError> {
    match requested_branch {
        Some(branch) => Ok(branch.to_string()),
        None => repository.current_branch(),
    }
}
