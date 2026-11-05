//! SQLite persistence adapter for LPRM.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lprm_core::LprmError;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: i64 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const MIGRATION_1: &str = r"
CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
) STRICT;

CREATE TABLE repositories (
    id TEXT PRIMARY KEY,
    common_git_dir TEXT NOT NULL UNIQUE,
    target_branch TEXT NOT NULL,
    created_at TEXT NOT NULL,
    config_hash TEXT NOT NULL
) STRICT;

CREATE TABLE worktrees (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE RESTRICT,
    branch TEXT NOT NULL,
    absolute_path TEXT NOT NULL,
    owner_agent TEXT,
    created_at TEXT NOT NULL,
    cleaned_at TEXT
) STRICT;
CREATE UNIQUE INDEX worktrees_active_branch
    ON worktrees(repository_id, branch) WHERE cleaned_at IS NULL;
CREATE UNIQUE INDEX worktrees_active_path
    ON worktrees(repository_id, absolute_path) WHERE cleaned_at IS NULL;

CREATE TABLE pull_requests (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE RESTRICT,
    worktree_id TEXT REFERENCES worktrees(id) ON DELETE RESTRICT,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    source_branch TEXT NOT NULL,
    target_branch TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    merged_at TEXT
) STRICT;

CREATE TABLE pr_revisions (
    id TEXT PRIMARY KEY,
    pr_id TEXT NOT NULL REFERENCES pull_requests(id) ON DELETE RESTRICT,
    revision_number INTEGER NOT NULL CHECK (revision_number > 0),
    source_sha TEXT NOT NULL,
    target_sha TEXT NOT NULL,
    policy_hash TEXT NOT NULL,
    check_config_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(pr_id, revision_number)
) STRICT;

CREATE TABLE check_runs (
    id TEXT PRIMARY KEY,
    revision_id TEXT NOT NULL REFERENCES pr_revisions(id) ON DELETE RESTRICT,
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    exit_code INTEGER,
    duration_ms INTEGER,
    stdout_path TEXT,
    stderr_path TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT
) STRICT;

CREATE TABLE review_runs (
    id TEXT PRIMARY KEY,
    revision_id TEXT NOT NULL REFERENCES pr_revisions(id) ON DELETE RESTRICT,
    reviewer_kind TEXT NOT NULL,
    verdict TEXT NOT NULL,
    findings_json TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE approvals (
    id TEXT PRIMARY KEY,
    revision_id TEXT NOT NULL REFERENCES pr_revisions(id) ON DELETE RESTRICT,
    actor TEXT NOT NULL,
    kind TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE merge_queue (
    id TEXT PRIMARY KEY,
    pr_id TEXT NOT NULL REFERENCES pull_requests(id) ON DELETE RESTRICT,
    revision_id TEXT NOT NULL REFERENCES pr_revisions(id) ON DELETE RESTRICT,
    priority INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    enqueued_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT
) STRICT;

CREATE TABLE merge_attempts (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE RESTRICT,
    queue_id TEXT NOT NULL REFERENCES merge_queue(id) ON DELETE RESTRICT,
    target_before_sha TEXT NOT NULL,
    source_sha TEXT NOT NULL,
    candidate_commit_sha TEXT,
    target_after_sha TEXT,
    state TEXT NOT NULL,
    error_code TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT
) STRICT;
CREATE UNIQUE INDEX merge_attempts_one_active_repository
    ON merge_attempts(repository_id)
    WHERE state IN ('preparing', 'testing', 'integrating', 'reconciling');

CREATE TABLE audit_events (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE RESTRICT,
    pr_id TEXT REFERENCES pull_requests(id) ON DELETE RESTRICT,
    event_type TEXT NOT NULL,
    actor TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;
CREATE INDEX audit_events_repository_time ON audit_events(repository_id, created_at, id);
CREATE INDEX audit_events_pr_time ON audit_events(pr_id, created_at, id);

INSERT INTO schema_migrations(version, applied_at)
VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
";

pub struct Store {
    connection: Connection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateLayout {
    repository_id: String,
    repository_dir: PathBuf,
    database_path: PathBuf,
    logs_dir: PathBuf,
    lock_path: PathBuf,
}

impl StateLayout {
    /// Resolves a repository-scoped layout beneath the configured platform state root.
    ///
    /// # Errors
    ///
    /// Returns an internal error when no usable state or home directory is configured.
    pub fn for_repository(common_git_dir: &Path) -> Result<Self, LprmError> {
        let root = default_state_root()?;
        Ok(Self::under(&root, common_git_dir))
    }

    pub fn under(root: &Path, common_git_dir: &Path) -> Self {
        let digest = hex::encode(Sha256::digest(common_git_dir.to_string_lossy().as_bytes()));
        let repository_id = format!("repo_{digest}");
        let repository_dir = root.join("repositories").join(&repository_id);
        Self {
            database_path: repository_dir.join("lprm.db"),
            logs_dir: repository_dir.join("logs"),
            lock_path: repository_dir.join("merge.lock"),
            repository_id,
            repository_dir,
        }
    }

    /// Creates private repository and log directories without touching the Git worktree.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the directories or permissions cannot be prepared.
    pub fn prepare(&self) -> Result<(), LprmError> {
        fs::create_dir_all(&self.logs_dir)
            .map_err(|error| persistence_error("create repository state directories", error))?;
        set_private_permissions(&self.repository_dir)?;
        set_private_permissions(&self.logs_dir)
    }

    pub fn repository_id(&self) -> &str {
        &self.repository_id
    }

    pub fn repository_dir(&self) -> &Path {
        self.repository_dir.as_path()
    }

    pub fn database_path(&self) -> &Path {
        self.database_path.as_path()
    }

    pub fn logs_dir(&self) -> &Path {
        self.logs_dir.as_path()
    }

    pub fn lock_path(&self) -> &Path {
        self.lock_path.as_path()
    }
}

impl Store {
    /// Opens a database file, creates its parent directory, and applies pending migrations.
    ///
    /// # Errors
    ///
    /// Returns an internal error when directories, SQLite setup, or a migration fails.
    pub fn open(path: &Path) -> Result<Self, LprmError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| persistence_error("create state directory", error))?;
        }
        let connection = Connection::open(path)
            .map_err(|error| persistence_error("open SQLite database", error))?;
        Self::initialize(connection, true)
    }

    /// Opens a migrated in-memory database for isolated tests and transient operations.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite setup or a migration fails.
    pub fn open_in_memory() -> Result<Self, LprmError> {
        let connection = Connection::open_in_memory()
            .map_err(|error| persistence_error("open in-memory SQLite database", error))?;
        Self::initialize(connection, false)
    }

    /// Returns the currently applied schema version.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot read the version pragma.
    pub fn schema_version(&self) -> Result<i64, LprmError> {
        self.connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| persistence_error("read SQLite schema version", error))
    }

    pub const fn connection(&self) -> &Connection {
        &self.connection
    }

    pub const fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// Registers or refreshes the repository record for an initialized state database.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the canonical path is not UTF-8 or SQLite rejects the row.
    pub fn register_repository(
        &self,
        repository_id: &str,
        common_git_dir: &Path,
        target_branch: &str,
        config_hash: &str,
    ) -> Result<(), LprmError> {
        let common_git_dir = common_git_dir.to_str().ok_or_else(|| {
            LprmError::internal("The canonical common Git directory is not valid UTF-8")
        })?;
        self.connection
            .execute(
                "INSERT INTO repositories(\
                    id, common_git_dir, target_branch, created_at, config_hash\
                 ) VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?4) \
                 ON CONFLICT(id) DO UPDATE SET \
                    target_branch = excluded.target_branch, \
                    config_hash = excluded.config_hash",
                (repository_id, common_git_dir, target_branch, config_hash),
            )
            .map_err(|error| persistence_error("register repository state", error))?;
        Ok(())
    }

    /// Returns the target branch registered during `lprm init`.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot query repository state.
    pub fn repository_target(&self, repository_id: &str) -> Result<Option<String>, LprmError> {
        self.connection
            .query_row(
                "SELECT target_branch FROM repositories WHERE id = ?1",
                [repository_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| persistence_error("read repository state", error))
    }

    /// Registers a linked worktree after Git creation succeeds.
    ///
    /// # Errors
    ///
    /// Returns an internal error for non-UTF-8 paths or rejected SQLite constraints.
    pub fn register_worktree(
        &self,
        id: &str,
        repository_id: &str,
        branch: &str,
        absolute_path: &Path,
        owner_agent: Option<&str>,
    ) -> Result<(), LprmError> {
        let absolute_path = absolute_path
            .to_str()
            .ok_or_else(|| LprmError::internal("The worktree path is not valid UTF-8"))?;
        self.connection
            .execute(
                "INSERT INTO worktrees(\
                    id, repository_id, branch, absolute_path, owner_agent, created_at\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                (id, repository_id, branch, absolute_path, owner_agent),
            )
            .map_err(|error| persistence_error("register managed worktree", error))?;
        Ok(())
    }

    fn initialize(mut connection: Connection, file_backed: bool) -> Result<Self, LprmError> {
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| persistence_error("enable SQLite foreign keys", error))?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(|error| persistence_error("set SQLite busy timeout", error))?;
        if file_backed {
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(|error| persistence_error("enable SQLite WAL mode", error))?;
        }

        let current: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| persistence_error("read SQLite schema version", error))?;
        match current {
            0 => migrate_to_one(&mut connection)?,
            SCHEMA_VERSION => {}
            version => {
                return Err(LprmError::internal(format!(
                    "Unsupported SQLite schema version {version}; expected {SCHEMA_VERSION}"
                )));
            }
        }

        Ok(Self { connection })
    }
}

fn migrate_to_one(connection: &mut Connection) -> Result<(), LprmError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| persistence_error("start SQLite migration", error))?;
    transaction
        .execute_batch(MIGRATION_1)
        .map_err(|error| persistence_error("apply SQLite schema version 1", error))?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| persistence_error("record SQLite schema version", error))?;
    transaction
        .commit()
        .map_err(|error| persistence_error("commit SQLite migration", error))
}

fn persistence_error(operation: &str, error: impl std::fmt::Display) -> LprmError {
    LprmError::internal(format!("Unable to {operation}: {error}"))
}

fn default_state_root() -> Result<PathBuf, LprmError> {
    if let Some(override_path) = env::var_os("LPRM_STATE_HOME") {
        if override_path.is_empty() {
            return Err(LprmError::internal("LPRM_STATE_HOME must not be empty"));
        }
        return Ok(PathBuf::from(override_path));
    }

    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| LprmError::internal("HOME is unavailable for LPRM state discovery"))?;

    #[cfg(target_os = "macos")]
    {
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("lprm"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(env::var_os("XDG_STATE_HOME")
            .filter(|value| !value.is_empty())
            .map_or_else(
                || home.join(".local/state/lprm"),
                |value| PathBuf::from(value).join("lprm"),
            ))
    }
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), LprmError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| persistence_error("set private state permissions", error))
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), LprmError> {
    Ok(())
}
