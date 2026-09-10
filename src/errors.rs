//! Error types for expected, user-actionable Memoro failures.
//!
//! Mirrors `python/src/memoro/errors.py`: concrete messages are built at the
//! throw site, so every variant only carries a `String` message.

/// Base class for expected, user-actionable Memoro failures.
///
/// Each variant corresponds to one Python error class:
/// - [`MemoroError::Base`] ← `MemoroError`
/// - [`MemoroError::Configuration`] ← `ConfigurationError`
/// - [`MemoroError::Repository`] ← `RepositoryError`
/// - [`MemoroError::RepositoryDirty`] ← `RepositoryDirtyError`
/// - [`MemoroError::MemoryValidation`] ← `MemoryValidationError`
/// - [`MemoroError::MemoryIntegrity`] ← `MemoryIntegrityError`
/// - [`MemoroError::MemoryConflict`] ← `MemoryConflictError`
/// - [`MemoroError::MemoryNotFound`] ← `MemoryNotFoundError`
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MemoroError {
    /// Expected, user-actionable Memoro failure without a narrower category.
    #[error("expected, user-actionable Memoro failure: {0}")]
    Base(String),

    /// Runtime configuration is missing or invalid.
    #[error("runtime configuration is missing or invalid: {0}")]
    Configuration(String),

    /// The memory Git repository cannot be used safely.
    #[error("the memory Git repository cannot be used safely: {0}")]
    Repository(String),

    /// A write was refused because the memory repository has local changes.
    #[error("a write was refused because the memory repository has local changes: {0}")]
    RepositoryDirty(String),

    /// A memory input or committed Markdown document is invalid.
    #[error("a memory input or committed Markdown document is invalid: {0}")]
    MemoryValidation(String),

    /// Committed memory documents conflict with each other.
    #[error("committed memory documents conflict with each other: {0}")]
    MemoryIntegrity(String),

    /// A mutation was refused because the target state changed or conflicts.
    #[error("a mutation was refused because the target state changed or conflicts: {0}")]
    MemoryConflict(String),

    /// The requested memory does not exist in the committed snapshot.
    #[error("the requested memory does not exist in the committed snapshot: {0}")]
    MemoryNotFound(String),
}
