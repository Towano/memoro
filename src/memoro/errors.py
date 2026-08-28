# Adapted from Perenna (https://github.com/scarletkc/Perenna), MIT License.


class MemoroError(Exception):
    """Base class for expected, user-actionable Memoro failures."""


class ConfigurationError(MemoroError):
    """Runtime configuration is missing or invalid."""


class RepositoryError(MemoroError):
    """The memory Git repository cannot be used safely."""


class RepositoryDirtyError(RepositoryError):
    """A write was refused because the memory repository has local changes."""


class MemoryValidationError(MemoroError):
    """A memory input or committed Markdown document is invalid."""


class MemoryIntegrityError(MemoroError):
    """Committed memory documents conflict with each other."""


class MemoryConflictError(MemoroError):
    """A mutation was refused because the target state changed or conflicts."""


class MemoryNotFoundError(MemoroError):
    """The requested memory does not exist in the committed snapshot."""
