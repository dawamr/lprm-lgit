//! SQLite persistence adapter for LPRM.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lprm_core::{LprmError, PullRequestStatus};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeRecord {
    pub id: String,
    pub branch: String,
    pub absolute_path: PathBuf,
    pub owner_agent: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewPullRequest {
    pub id: String,
    pub repository_id: String,
    pub worktree_id: Option<String>,
    pub title: String,
    pub body: String,
    pub source_branch: String,
    pub target_branch: String,
    pub revision_id: String,
    pub source_sha: String,
    pub target_sha: String,
    pub policy_hash: String,
    pub check_config_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PullRequestRevisionRecord {
    pub id: String,
    pub revision_number: u32,
    pub source_sha: String,
    pub target_sha: String,
    pub policy_hash: String,
    pub check_config_hash: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PullRequestRecord {
    #[serde(rename = "pr_id")]
    pub id: String,
    pub worktree_id: Option<String>,
    pub title: String,
    pub body: String,
    pub source_branch: String,
    pub target_branch: String,
    pub status: PullRequestStatus,
    pub created_at: String,
    pub updated_at: String,
    pub merged_at: Option<String>,
    pub revisions: Vec<PullRequestRevisionRecord>,
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

    /// Lists active worktrees registered for a repository in creation order.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot query or decode worktree rows.
    pub fn list_worktrees(&self, repository_id: &str) -> Result<Vec<WorktreeRecord>, LprmError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, branch, absolute_path, owner_agent, created_at \
                 FROM worktrees \
                 WHERE repository_id = ?1 AND cleaned_at IS NULL \
                 ORDER BY created_at, id",
            )
            .map_err(|error| persistence_error("prepare managed worktree query", error))?;
        let rows = statement
            .query_map([repository_id], |row| {
                Ok(WorktreeRecord {
                    id: row.get(0)?,
                    branch: row.get(1)?,
                    absolute_path: PathBuf::from(row.get::<_, String>(2)?),
                    owner_agent: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|error| persistence_error("query managed worktrees", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| persistence_error("decode managed worktrees", error))
    }

    /// Looks up one active worktree by repository and immutable worktree ID.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot query or decode the worktree row.
    pub fn worktree(
        &self,
        repository_id: &str,
        worktree_id: &str,
    ) -> Result<Option<WorktreeRecord>, LprmError> {
        self.connection
            .query_row(
                "SELECT id, branch, absolute_path, owner_agent, created_at \
                 FROM worktrees \
                 WHERE repository_id = ?1 AND id = ?2 AND cleaned_at IS NULL",
                (repository_id, worktree_id),
                |row| {
                    Ok(WorktreeRecord {
                        id: row.get(0)?,
                        branch: row.get(1)?,
                        absolute_path: PathBuf::from(row.get::<_, String>(2)?),
                        owner_agent: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|error| persistence_error("read managed worktree", error))
    }

    /// Marks a successfully removed worktree as cleaned while preserving its audit record.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot update exactly one active record.
    pub fn mark_worktree_cleaned(
        &self,
        repository_id: &str,
        worktree_id: &str,
    ) -> Result<(), LprmError> {
        let changed = self
            .connection
            .execute(
                "UPDATE worktrees \
                 SET cleaned_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE repository_id = ?1 AND id = ?2 AND cleaned_at IS NULL",
                (repository_id, worktree_id),
            )
            .map_err(|error| persistence_error("mark managed worktree cleaned", error))?;
        if changed != 1 {
            return Err(LprmError::internal(
                "Managed worktree cleanup record was not active",
            ));
        }
        Ok(())
    }

    /// Creates a PR and its immutable first revision in one transaction.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite rejects either record or cannot commit atomically.
    pub fn create_pull_request(
        &mut self,
        request: &NewPullRequest,
    ) -> Result<PullRequestRecord, LprmError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start pull request transaction", error))?;
        transaction
            .execute(
                "INSERT INTO pull_requests(\
                    id, repository_id, worktree_id, title, body, source_branch, target_branch, \
                    status, created_at, updated_at\
                 ) VALUES (\
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'draft', \
                    strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
                 )",
                (
                    &request.id,
                    &request.repository_id,
                    &request.worktree_id,
                    &request.title,
                    &request.body,
                    &request.source_branch,
                    &request.target_branch,
                ),
            )
            .map_err(|error| persistence_error("create pull request", error))?;
        transaction
            .execute(
                "INSERT INTO pr_revisions(\
                    id, pr_id, revision_number, source_sha, target_sha, policy_hash, \
                    check_config_hash, created_at\
                 ) VALUES (\
                    ?1, ?2, 1, ?3, ?4, ?5, ?6, \
                    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
                 )",
                (
                    &request.revision_id,
                    &request.id,
                    &request.source_sha,
                    &request.target_sha,
                    &request.policy_hash,
                    &request.check_config_hash,
                ),
            )
            .map_err(|error| persistence_error("create pull request revision", error))?;
        transaction
            .commit()
            .map_err(|error| persistence_error("commit pull request transaction", error))?;

        self.pull_request(&request.repository_id, &request.id)?
            .ok_or_else(|| LprmError::internal("Created pull request could not be read"))
    }

    /// Lists repository PRs, optionally filtered by exact status, in creation order.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot query or decode PR records.
    pub fn list_pull_requests(
        &self,
        repository_id: &str,
        status: Option<PullRequestStatus>,
    ) -> Result<Vec<PullRequestRecord>, LprmError> {
        let ids = if let Some(status) = status {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT id FROM pull_requests \
                     WHERE repository_id = ?1 AND status = ?2 ORDER BY created_at, id",
                )
                .map_err(|error| persistence_error("prepare filtered pull request query", error))?;
            let rows = statement
                .query_map((repository_id, status.as_str()), |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|error| persistence_error("query filtered pull requests", error))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| persistence_error("decode filtered pull requests", error))?
        } else {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT id FROM pull_requests \
                     WHERE repository_id = ?1 ORDER BY created_at, id",
                )
                .map_err(|error| persistence_error("prepare pull request query", error))?;
            let rows = statement
                .query_map([repository_id], |row| row.get::<_, String>(0))
                .map_err(|error| persistence_error("query pull requests", error))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| persistence_error("decode pull requests", error))?
        };

        ids.into_iter()
            .map(|id| {
                self.pull_request(repository_id, &id)?
                    .ok_or_else(|| LprmError::internal("Listed pull request could not be read"))
            })
            .collect()
    }

    /// Reads one PR and its immutable revision history.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot query or decode the record.
    pub fn pull_request(
        &self,
        repository_id: &str,
        pull_request_id: &str,
    ) -> Result<Option<PullRequestRecord>, LprmError> {
        let record = self
            .connection
            .query_row(
                "SELECT id, worktree_id, title, body, source_branch, target_branch, status, \
                        created_at, updated_at, merged_at \
                 FROM pull_requests WHERE repository_id = ?1 AND id = ?2",
                (repository_id, pull_request_id),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, Option<String>>(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| persistence_error("read pull request", error))?;
        let Some((
            id,
            worktree_id,
            title,
            body,
            source_branch,
            target_branch,
            status,
            created_at,
            updated_at,
            merged_at,
        )) = record
        else {
            return Ok(None);
        };
        let status = parse_pull_request_status(&status)?;
        let revisions = self.pull_request_revisions(&id)?;
        Ok(Some(PullRequestRecord {
            id,
            worktree_id,
            title,
            body,
            source_branch,
            target_branch,
            status,
            created_at,
            updated_at,
            merged_at,
            revisions,
        }))
    }

    /// Closes an eligible PR using the core transition guard.
    ///
    /// # Errors
    ///
    /// Returns a validation error for an illegal transition, or an internal error on persistence
    /// failure or a concurrent status change.
    pub fn close_pull_request(
        &mut self,
        repository_id: &str,
        pull_request_id: &str,
    ) -> Result<PullRequestRecord, LprmError> {
        let current = self
            .pull_request(repository_id, pull_request_id)?
            .ok_or_else(|| LprmError::invalid_argument("Unknown pull request ID"))?;
        let next = current.status.transition_to(PullRequestStatus::Closed)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start pull request close transaction", error))?;
        let changed = transaction
            .execute(
                "UPDATE pull_requests \
                 SET status = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE repository_id = ?2 AND id = ?3 AND status = ?4",
                (
                    next.as_str(),
                    repository_id,
                    pull_request_id,
                    current.status.as_str(),
                ),
            )
            .map_err(|error| persistence_error("close pull request", error))?;
        if changed != 1 {
            return Err(LprmError::internal(
                "Pull request status changed concurrently",
            ));
        }
        transaction
            .commit()
            .map_err(|error| persistence_error("commit pull request close", error))?;
        self.pull_request(repository_id, pull_request_id)?
            .ok_or_else(|| LprmError::internal("Closed pull request could not be read"))
    }

    fn pull_request_revisions(
        &self,
        pull_request_id: &str,
    ) -> Result<Vec<PullRequestRevisionRecord>, LprmError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, revision_number, source_sha, target_sha, policy_hash, \
                        check_config_hash, created_at \
                 FROM pr_revisions WHERE pr_id = ?1 ORDER BY revision_number",
            )
            .map_err(|error| persistence_error("prepare pull request revision query", error))?;
        let rows = statement
            .query_map([pull_request_id], |row| {
                Ok(PullRequestRevisionRecord {
                    id: row.get(0)?,
                    revision_number: row.get(1)?,
                    source_sha: row.get(2)?,
                    target_sha: row.get(3)?,
                    policy_hash: row.get(4)?,
                    check_config_hash: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .map_err(|error| persistence_error("query pull request revisions", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| persistence_error("decode pull request revisions", error))
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

fn parse_pull_request_status(value: &str) -> Result<PullRequestStatus, LprmError> {
    PullRequestStatus::ALL
        .into_iter()
        .find(|status| status.as_str() == value)
        .ok_or_else(|| LprmError::internal(format!("Unknown persisted PR status {value:?}")))
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
