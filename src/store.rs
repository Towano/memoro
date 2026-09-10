//! Git-backed store orchestrating validated memory mutations.
//!
//! Mirrors `python/src/memoro/store.py`: snapshots are validated and cached
//! per HEAD commit, every mutation normalizes its inputs through the pure
//! helpers in [`crate::models`] and wraps their `ValueError` texts into the
//! exact English guidance messages of the Python suite, and writes flow
//! through a stage/commit/rollback protocol that restores the previous
//! worktree bytes and index state when a commit fails.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use crate::errors::MemoroError;
use crate::filesystem::atomic_replace;
use crate::git::GitRepository;
use crate::markdown::{memory_revision, parse_memory, serialize_memory};
use crate::models::{
    format_timestamp, location_label, memory_path, new_ulid, next_update_time, normalize_body,
    normalize_summary, normalize_tags, normalize_title, title_key, utc_now, validate_location,
    validate_revision, validate_ulid, Memory, MemorySnapshot, MutationReceipt, PatchEdit, MAX_TAGS,
};

type Clock = Box<dyn Fn() -> DateTime<Utc> + Send + Sync>;
type IdFactory = Box<dyn Fn() -> String + Send + Sync>;
type CommitHook = Box<dyn Fn(&str, &str) -> Result<String, MemoroError> + Send + Sync>;

#[derive(Debug, Default)]
pub struct SnapshotCache {
    snapshots: Mutex<HashMap<PathBuf, MemorySnapshot>>,
}

impl SnapshotCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, repository_path: &Path, commit: &Option<String>) -> Option<MemorySnapshot> {
        let snapshots = self.snapshots.lock().expect("snapshot cache lock");
        snapshots
            .get(repository_path)
            .filter(|snapshot| snapshot.commit == *commit)
            .cloned()
    }

    fn insert(&self, repository_path: &Path, snapshot: MemorySnapshot) {
        self.snapshots
            .lock()
            .expect("snapshot cache lock")
            .insert(repository_path.to_path_buf(), snapshot);
    }
}

/// Store fronting one memory [`GitRepository`] with validated mutations.
///
/// Mirrors `MemoryStore` in `python/src/memoro/store.py`. The clock and ID
/// factory are injectable for tests; `with_commit_hook` stands in for the
/// Python suite's monkeypatching of `GitRepository.commit`.
pub struct MemoryStore {
    repository: GitRepository,
    clock: Clock,
    id_factory: IdFactory,
    commit_hook: Option<CommitHook>,
    snapshot_cache: Arc<SnapshotCache>,
}

impl MemoryStore {
    pub fn new(repository: GitRepository) -> Self {
        Self::with_snapshot_cache(repository, Arc::new(SnapshotCache::new()))
    }

    pub fn with_snapshot_cache(
        repository: GitRepository,
        snapshot_cache: Arc<SnapshotCache>,
    ) -> Self {
        Self {
            repository,
            clock: Box::new(utc_now),
            id_factory: Box::new(|| new_ulid(None)),
            commit_hook: None,
            snapshot_cache,
        }
    }

    /// Replace the clock used for `created_at`/`updated_at` stamps.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Replace the factory generating new memory IDs.
    pub fn with_id_factory(mut self, id_factory: IdFactory) -> Self {
        self.id_factory = id_factory;
        self
    }

    /// Intercept every Git commit the store would create. Test seam
    /// equivalent to monkeypatching `GitRepository.commit` in the Python
    /// suite; returning `Err` simulates a failing `git commit`.
    pub fn with_commit_hook(mut self, hook: CommitHook) -> Self {
        self.commit_hook = Some(hook);
        self
    }

    pub fn snapshot(&self) -> Result<MemorySnapshot, MemoroError> {
        let commit = self.repository.head()?;
        if let Some(snapshot) = self.snapshot_cache.get(self.repository.path(), &commit) {
            return Ok(snapshot);
        }
        let memories = match &commit {
            None => Vec::new(),
            Some(commit) => {
                let mut memories = Vec::new();
                for path in self.repository.memory_paths_at_commit(commit)? {
                    let text = self.repository.read_at_commit(commit, &path)?;
                    memories.push(parse_memory(&text, &path)?);
                }
                memories
            }
        };
        validate_integrity(&memories)?;
        let snapshot = MemorySnapshot { commit, memories };
        self.snapshot_cache
            .insert(self.repository.path(), snapshot.clone());
        Ok(snapshot)
    }

    pub fn create(
        &self,
        title: &str,
        summary: &str,
        body: &str,
        kind: &str,
        project: Option<&str>,
        tags: &[String],
    ) -> Result<MutationReceipt, MemoroError> {
        let normalized_title = validated("title", normalize_title, title)?;
        let normalized_summary = validated("summary", normalize_summary, summary)?;
        let normalized_body = validated("body", normalize_body, body)?;
        let (normalized_kind, normalized_project) = validated_location(kind, project)?;
        let normalized_tags = validated_tags(tags.to_vec())?;
        let label = location_label(&normalized_kind, normalized_project.as_deref())
            .map_err(MemoroError::MemoryValidation)?;

        let snapshot = self.writable_snapshot()?;
        let normalized_key = title_key_or_error(&normalized_title)?;
        let existing = snapshot.memories.iter().find(|memory| {
            memory.kind == normalized_kind
                && memory.project == normalized_project
                && title_key_or_error(&memory.title).is_ok_and(|key| key == normalized_key)
        });
        if let Some(existing) = existing {
            if existing.summary == normalized_summary
                && existing.body == normalized_body
                && existing.tags == normalized_tags
            {
                return unchanged_receipt("create", existing, &snapshot);
            }
            return Err(MemoroError::MemoryConflict(format!(
                "Memory {} already uses this title in {}. Read it and use patch or replace \
                 instead; no file was changed.",
                existing.id, label
            )));
        }

        let memory_id = validated("id", validate_ulid, &(self.id_factory)())?;
        if snapshot
            .memories
            .iter()
            .any(|memory| memory.id == memory_id)
        {
            return Err(MemoroError::MemoryIntegrity(format!(
                "Generated memory ID {memory_id} already exists. No file was changed; retry."
            )));
        }
        let created_at = format_timestamp((self.clock)());
        let relative_path =
            memory_path(&memory_id, &normalized_kind, normalized_project.as_deref())
                .map_err(MemoroError::MemoryValidation)?;
        let memory = Memory {
            id: memory_id,
            title: normalized_title,
            summary: normalized_summary,
            tags: normalized_tags,
            created_at: created_at.clone(),
            updated_at: created_at,
            body: normalized_body,
            kind: normalized_kind,
            project: normalized_project,
            relative_path,
        };
        self.commit_memory(&memory, None, &snapshot, "create")
    }

    pub fn patch(
        &self,
        memory_id: &str,
        base_revision: &str,
        edits: &[PatchEdit],
        summary: Option<&str>,
    ) -> Result<MutationReceipt, MemoroError> {
        let normalized_id = validated("id", validate_ulid, memory_id)?;
        let normalized_revision = validated("revision", validate_revision, base_revision)?;
        let normalized_summary = match summary {
            None => None,
            Some(value) => Some(validated("summary", normalize_summary, value)?),
        };
        let normalized_edits = validated_edits(edits)?;

        let snapshot = self.writable_snapshot()?;
        let previous = memory_by_id(&snapshot, &normalized_id)?;
        require_revision(previous, &normalized_revision)?;
        let body = apply_edits(&previous.body, &normalized_edits)?;
        let next_summary = match normalized_summary {
            None => previous.summary.clone(),
            Some(value) => value,
        };
        if body == previous.body && next_summary == previous.summary {
            return unchanged_receipt("patch", previous, &snapshot);
        }
        let memory = self.updated_memory(previous, &next_summary, &previous.tags, &body);
        self.commit_memory(&memory, Some(previous), &snapshot, "patch")
    }

    pub fn replace(
        &self,
        memory_id: &str,
        base_revision: &str,
        summary: &str,
        body: &str,
        tags: &[String],
    ) -> Result<MutationReceipt, MemoroError> {
        let normalized_id = validated("id", validate_ulid, memory_id)?;
        let normalized_revision = validated("revision", validate_revision, base_revision)?;
        let normalized_summary = validated("summary", normalize_summary, summary)?;
        let normalized_body = validated("body", normalize_body, body)?;
        let normalized_tags = validated_tags(tags.to_vec())?;

        let snapshot = self.writable_snapshot()?;
        let previous = memory_by_id(&snapshot, &normalized_id)?;
        require_revision(previous, &normalized_revision)?;
        if normalized_summary == previous.summary
            && normalized_body == previous.body
            && normalized_tags == previous.tags
        {
            return unchanged_receipt("replace", previous, &snapshot);
        }
        let memory = self.updated_memory(
            previous,
            &normalized_summary,
            &normalized_tags,
            &normalized_body,
        );
        self.commit_memory(&memory, Some(previous), &snapshot, "replace")
    }

    pub fn delete(
        &self,
        memory_id: &str,
        expected_title: &str,
        base_revision: &str,
    ) -> Result<MutationReceipt, MemoroError> {
        let normalized_id = validated("id", validate_ulid, memory_id)?;
        let normalized_title = validated("title", normalize_title, expected_title)?;
        let normalized_revision = validated("revision", validate_revision, base_revision)?;

        let snapshot = self.writable_snapshot()?;
        let previous = memory_by_id(&snapshot, &normalized_id)?;
        if previous.title != normalized_title {
            return Err(MemoroError::MemoryConflict(format!(
                "Memory {} is titled {}, not {}. Read the memory again and confirm the intended \
                 deletion; no file was changed.",
                normalized_id,
                py_repr(&previous.title),
                py_repr(&normalized_title)
            )));
        }
        require_revision(previous, &normalized_revision)?;
        let commit =
            self.commit_target(&previous.relative_path, None, &snapshot, "delete", previous)?;
        Ok(MutationReceipt {
            memory: previous.clone(),
            operation: "delete".to_string(),
            changed: true,
            previous_memory: Some(previous.clone()),
            previous_commit: snapshot.commit.clone(),
            commit,
        })
    }

    fn writable_snapshot(&self) -> Result<MemorySnapshot, MemoroError> {
        self.repository.assert_clean()?;
        self.repository.current_branch()?;
        self.snapshot()
    }

    fn updated_memory(
        &self,
        previous: &Memory,
        summary: &str,
        tags: &[String],
        body: &str,
    ) -> Memory {
        Memory {
            id: previous.id.clone(),
            title: previous.title.clone(),
            summary: summary.to_string(),
            tags: tags.to_vec(),
            created_at: previous.created_at.clone(),
            updated_at: format_timestamp(next_update_time((self.clock)(), &previous.updated_at)),
            body: body.to_string(),
            kind: previous.kind.clone(),
            project: previous.project.clone(),
            relative_path: previous.relative_path.clone(),
        }
    }

    fn commit_memory(
        &self,
        memory: &Memory,
        previous: Option<&Memory>,
        snapshot: &MemorySnapshot,
        operation: &str,
    ) -> Result<MutationReceipt, MemoroError> {
        let new_bytes = serialize_memory(memory)
            .map_err(MemoroError::MemoryValidation)?
            .into_bytes();
        let commit = self.commit_target(
            &memory.relative_path,
            Some(new_bytes),
            snapshot,
            operation,
            memory,
        )?;
        Ok(MutationReceipt {
            memory: memory.clone(),
            operation: operation.to_string(),
            changed: true,
            previous_memory: previous.cloned(),
            previous_commit: snapshot.commit.clone(),
            commit,
        })
    }

    fn commit_target(
        &self,
        relative_path: &str,
        new_bytes: Option<Vec<u8>>,
        snapshot: &MemorySnapshot,
        operation: &str,
        memory: &Memory,
    ) -> Result<String, MemoroError> {
        let target = self.repository.worktree_path(relative_path)?;
        if is_filesystem_link(&target) {
            return Err(MemoroError::Repository(format!(
                "Memory target {} is a filesystem link. Replace it with a regular file before \
                 retrying.",
                target.display()
            )));
        }
        let old_bytes = read_old_bytes(&target)?;
        let mut committed = false;
        let mut attempt = || -> Result<String, MemoroError> {
            if let Some(bytes) = &new_bytes {
                atomic_replace(&target, bytes).map_err(|_| target_write_failure(&target))?;
            } else if old_bytes.is_none() {
                return Err(MemoroError::Repository(format!(
                    "Committed memory target {} is missing from the worktree. Restore the clean \
                     Git checkout before retrying.",
                    target.display()
                )));
            } else {
                fs::remove_file(&target).map_err(|_| target_write_failure(&target))?;
            }
            self.repository.stage(relative_path)?;
            let message = commit_message(operation, memory)?;
            let commit = match &self.commit_hook {
                Some(hook) => hook(&message, relative_path)?,
                None => self.repository.commit(&message, relative_path)?,
            };
            committed = true;
            Ok(commit)
        };
        let outcome = match attempt() {
            Ok(commit) => Ok(commit),
            Err(error) => {
                let current_head = self.repository.head()?;
                if current_head != snapshot.commit {
                    Err(MemoroError::Repository(
                        "The Git commit command failed after HEAD changed. Memoro did not alter \
                         the new commit; inspect the memory repository before retrying."
                            .to_string(),
                    ))
                } else {
                    let restored = restore_target(&target, old_bytes.as_deref()).is_ok()
                        && self
                            .repository
                            .unstage(relative_path, snapshot.commit.as_deref())
                            .is_ok();
                    if restored {
                        Err(error)
                    } else {
                        Err(MemoroError::Repository(format!(
                            "Memory {operation} failed and automatic rollback could not restore \
                             {}. Inspect the memory repository before continuing.",
                            target.display()
                        )))
                    }
                }
            }
        };
        if (new_bytes.is_none() && committed) || (old_bytes.is_none() && !committed) {
            if let Some(parent) = target.parent() {
                remove_empty_memory_directories(parent, self.repository.path());
            }
        }
        outcome
    }
}

fn validate_integrity(memories: &[Memory]) -> Result<(), MemoroError> {
    let mut ids: HashMap<&str, &str> = HashMap::new();
    let mut titles: HashMap<(&str, Option<&str>, String), &str> = HashMap::new();
    for memory in memories {
        if let Some(first_path) = ids.get(memory.id.as_str()) {
            return Err(MemoroError::MemoryIntegrity(format!(
                "Memory ID {} appears in both {} and {}. Repair and commit one file before \
                 retrying.",
                memory.id,
                py_repr(first_path),
                py_repr(&memory.relative_path)
            )));
        }
        ids.insert(memory.id.as_str(), memory.relative_path.as_str());
        let key = (
            memory.kind.as_str(),
            memory.project.as_deref(),
            title_key_or_error(&memory.title)?,
        );
        if let Some(first_path) = titles.get(&key) {
            let label = location_label(&memory.kind, memory.project.as_deref())
                .map_err(MemoroError::MemoryValidation)?;
            return Err(MemoroError::MemoryIntegrity(format!(
                "Kind {} has duplicate normalized titles in {} and {}. Repair and commit one \
                 file before retrying.",
                py_repr(&label),
                py_repr(first_path),
                py_repr(&memory.relative_path)
            )));
        }
        titles.insert(key, memory.relative_path.as_str());
    }
    Ok(())
}

fn memory_by_id<'a>(
    snapshot: &'a MemorySnapshot,
    memory_id: &str,
) -> Result<&'a Memory, MemoroError> {
    snapshot.by_id().get(memory_id).copied().ok_or_else(|| {
        MemoroError::MemoryNotFound(format!(
            "Memory {memory_id} was not found in the committed snapshot. List or search \
                 memories again, then retry with a current memory ID."
        ))
    })
}

fn require_revision(memory: &Memory, base_revision: &str) -> Result<(), MemoroError> {
    if memory_revision(memory).map_err(MemoroError::MemoryValidation)? != base_revision {
        return Err(MemoroError::MemoryConflict(format!(
            "Memory {} changed after it was read. Get the current memory and retry with its \
             revision; no file was changed.",
            memory.id
        )));
    }
    Ok(())
}

fn unchanged_receipt(
    operation: &str,
    memory: &Memory,
    snapshot: &MemorySnapshot,
) -> Result<MutationReceipt, MemoroError> {
    let Some(commit) = snapshot.commit.clone() else {
        return Err(MemoroError::MemoryIntegrity(
            "A committed memory exists without a readable Git HEAD.".to_string(),
        ));
    };
    Ok(MutationReceipt {
        memory: memory.clone(),
        operation: operation.to_string(),
        changed: false,
        previous_memory: Some(memory.clone()),
        previous_commit: snapshot.commit.clone(),
        commit,
    })
}

fn validated_edits(edits: &[PatchEdit]) -> Result<Vec<PatchEdit>, MemoroError> {
    if edits.is_empty() {
        return Err(MemoroError::MemoryValidation(
            "Memory patch is invalid. Provide at least one exact edit.".to_string(),
        ));
    }
    let mut normalized = Vec::with_capacity(edits.len());
    for edit in edits {
        normalized.push(PatchEdit {
            old_text: patch_text(&edit.old_text, false)?,
            new_text: patch_text(&edit.new_text, true)?,
        });
    }
    Ok(normalized)
}

fn patch_text(value: &str, allow_empty: bool) -> Result<String, MemoroError> {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    if !allow_empty && normalized.is_empty() {
        return Err(MemoroError::MemoryValidation(
            "Memory patch old_text is empty. Use an exact non-empty anchor.".to_string(),
        ));
    }
    // Python also rejects surrogates (category Cs), which cannot occur in a
    // Rust `str`; `is_control` matches the Cc category.
    if normalized
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(MemoroError::MemoryValidation(
            "Memory patch text contains an unsupported control character. Use plain text."
                .to_string(),
        ));
    }
    Ok(normalized)
}

fn apply_edits(body: &str, edits: &[PatchEdit]) -> Result<String, MemoroError> {
    let mut located: Vec<(usize, usize, &str)> = Vec::with_capacity(edits.len());
    for edit in edits {
        let Some(start) = body.find(&edit.old_text) else {
            return Err(MemoroError::MemoryConflict(
                "Memory patch could not find one of its exact old_text anchors. Get the current \
                 memory and retry; no edit was applied."
                    .to_string(),
            ));
        };
        let mut search_from = start + 1;
        while !body.is_char_boundary(search_from) {
            search_from += 1;
        }
        if body[search_from..].contains(&edit.old_text) {
            return Err(MemoroError::MemoryConflict(
                "Memory patch found an old_text anchor more than once. Include more surrounding \
                 text so every edit is unique; no edit was applied."
                    .to_string(),
            ));
        }
        located.push((start, start + edit.old_text.len(), edit.new_text.as_str()));
    }

    located.sort_by_key(|item| item.0);
    for window in located.windows(2) {
        if window[1].0 < window[0].1 {
            return Err(MemoroError::MemoryConflict(
                "Memory patch edits overlap in the base body. Submit non-overlapping edits; no \
                 edit was applied."
                    .to_string(),
            ));
        }
    }

    let mut result = body.to_string();
    for (start, end, replacement) in located.iter().rev() {
        result.replace_range(start..end, replacement);
    }
    let normalized = validated("body", normalize_body, &result)?;
    if normalized != result {
        return Err(MemoroError::MemoryValidation(
            "Memory patch result is not canonical. Keep content within the existing body \
             boundaries instead of adding leading or trailing newlines."
                .to_string(),
        ));
    }
    Ok(normalized)
}

fn validated_location(
    kind: &str,
    project: Option<&str>,
) -> Result<(String, Option<String>), MemoroError> {
    validate_location(kind, project).map_err(|exc| {
        MemoroError::MemoryValidation(format!(
            "Memory location is invalid: {exc}. Use kind persona, project, or playbook, and \
             provide a project slug exactly when kind is project."
        ))
    })
}

fn validated_tags(tags: Vec<String>) -> Result<Vec<String>, MemoroError> {
    normalize_tags(tags).map_err(|exc| {
        MemoroError::MemoryValidation(format!(
            "Memory tags are invalid: {exc}. Use at most {MAX_TAGS} non-empty plain-text tags of \
             at most 32 characters each."
        ))
    })
}

fn validated(
    name: &str,
    normalizer: impl Fn(&str) -> Result<String, String>,
    value: &str,
) -> Result<String, MemoroError> {
    normalizer(value).map_err(|_| {
        let guidance = match name {
            "title" => "Use a non-empty title of at most 120 characters.",
            "summary" => "Use one non-empty plain-text line of at most 300 characters.",
            "body" => "Use a non-empty body of at most 20,000 characters.",
            "revision" => "Get the memory again and use its current revision.",
            _ => "Retry the operation.",
        };
        MemoroError::MemoryValidation(format!("Memory {name} is invalid. {guidance}"))
    })
}

fn title_key_or_error(value: &str) -> Result<String, MemoroError> {
    title_key(value).map_err(MemoroError::MemoryValidation)
}

fn is_filesystem_link(target: &Path) -> bool {
    fs::symlink_metadata(target)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn read_old_bytes(target: &Path) -> Result<Option<Vec<u8>>, MemoroError> {
    if !target.exists() {
        return Ok(None);
    }
    fs::read(target)
        .map(Some)
        .map_err(|_| target_read_failure(target))
}

fn restore_target(target: &Path, old_bytes: Option<&[u8]>) -> io::Result<()> {
    match old_bytes {
        None => match fs::remove_file(target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
        Some(bytes) => atomic_replace(target, bytes),
    }
}

fn remove_empty_memory_directories(directory: &Path, repository: &Path) {
    let mut current = directory.to_path_buf();
    while current != repository {
        if fs::remove_dir(&current).is_err() {
            break;
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
}

fn commit_message(operation: &str, memory: &Memory) -> Result<String, MemoroError> {
    let label = location_label(&memory.kind, memory.project.as_deref())
        .map_err(MemoroError::MemoryValidation)?;
    let safe_title = memory.title.replace('"', "'");
    Ok(format!("memory({label}): {operation} \"{safe_title}\""))
}

fn target_read_failure(target: &Path) -> MemoroError {
    MemoroError::Repository(format!(
        "Memory target {} could not be read. Inspect the memory repository before retrying.",
        target.display()
    ))
}

fn target_write_failure(target: &Path) -> MemoroError {
    MemoroError::Repository(format!(
        "Memory target {} could not be written. Inspect the memory repository before retrying.",
        target.display()
    ))
}

/// Render a string the way Python's `repr()` renders a `str`.
fn py_repr(value: &str) -> String {
    if value.contains('\'') && !value.contains('"') {
        format!("\"{value}\"")
    } else {
        format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}
