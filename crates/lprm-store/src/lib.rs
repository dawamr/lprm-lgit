//! SQLite persistence adapter for LPRM.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use lprm_core::{AdmissionGates, LprmError, PullRequestStatus, RevisionFingerprint};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use serde_json::{Value, json};
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

/// An exclusive OS advisory lock for one repository coordinator.
pub struct RepositoryLock {
    file: File,
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
    pub audit_id: String,
    pub actor: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionEvidence {
    pub audit_id: String,
    pub actor: String,
    pub reason: String,
    pub revision_id: String,
    pub source_sha: String,
    pub target_sha: String,
    pub policy_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunStatus {
    Passed,
    Failed,
    TimedOut,
    MissingExecutable,
}

impl CheckRunStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::MissingExecutable => "missing_executable",
        }
    }

    fn parse(value: &str) -> Result<Self, LprmError> {
        match value {
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            "timed_out" => Ok(Self::TimedOut),
            "missing_executable" => Ok(Self::MissingExecutable),
            _ => Err(LprmError::internal(format!(
                "Unknown persisted check status {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckRunEvidence {
    pub id: String,
    pub name: String,
    pub status: CheckRunStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordCheckSuite {
    pub audit_id: String,
    pub actor: String,
    pub reason: String,
    pub revision_id: String,
    pub source_sha: String,
    pub target_sha: String,
    pub policy_hash: String,
    pub check_config_hash: String,
    pub checks: Vec<CheckRunEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckRunRecord {
    pub id: String,
    pub revision_id: String,
    pub name: String,
    pub status: CheckRunStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnqueuePullRequest {
    pub queue_id: String,
    pub audit_id: String,
    pub actor: String,
    pub reason: String,
    pub priority: i64,
    pub current_fingerprint: RevisionFingerprint,
    pub gates: AdmissionGates,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartMergeAttempt {
    pub attempt_id: String,
    pub audit_id: String,
    pub actor: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateMergeEvidence {
    pub attempt_id: String,
    pub candidate_commit_sha: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteMergeAttempt {
    pub attempt_id: String,
    pub audit_id: String,
    pub actor: String,
    pub target_before_sha: String,
    pub source_sha: String,
    pub candidate_commit_sha: String,
    pub target_after_sha: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequeueMergeAttempt {
    pub attempt_id: String,
    pub audit_id: String,
    pub actor: String,
    pub target_before_sha: String,
    pub source_sha: String,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AuditEventRecord {
    pub id: String,
    pub pr_id: Option<String>,
    pub event_type: String,
    pub actor: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MergeQueueRecord {
    pub id: String,
    pub pr_id: String,
    pub revision_id: String,
    pub priority: i64,
    pub status: String,
    pub enqueued_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MergeAttemptRecord {
    pub id: String,
    pub repository_id: String,
    pub queue_id: String,
    pub pr_id: String,
    pub revision_id: String,
    pub target_before_sha: String,
    pub source_sha: String,
    pub candidate_commit_sha: Option<String>,
    pub target_after_sha: Option<String>,
    pub state: String,
    pub error_code: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Serialize)]
struct LockHolder<'a> {
    pid: u32,
    operation: &'a str,
    acquired_unix_ms: u128,
}

struct QueueAdmissionRow {
    status: String,
    revision_id: String,
    source_sha: String,
    target_sha: String,
    policy_hash: String,
    check_config_hash: String,
}

struct QueuedMergeRow {
    queue_id: String,
    pull_request_id: String,
    revision_id: String,
    status: String,
    target_sha: String,
    source_sha: String,
}

struct MergeCompletionRow {
    state: String,
    target_before_sha: String,
    source_sha: String,
    candidate_commit_sha: Option<String>,
    queue_id: String,
    pull_request_id: String,
    pull_request_status: String,
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

impl RepositoryLock {
    /// Acquires the repository coordinator lock within a bounded timeout.
    ///
    /// The lock file contents are diagnostic metadata only. Ownership is always determined by the
    /// operating system lock, so callers must never delete this file based on metadata age.
    ///
    /// # Errors
    ///
    /// Returns `BUSY` when another process retains the lock past `timeout`, or an internal error
    /// when the lock file or holder metadata cannot be prepared.
    pub fn acquire(path: &Path, timeout: Duration, operation: &str) -> Result<Self, LprmError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| persistence_error("create lock directory", error))?;
        }
        let mut file = open_lock_file(path)?;
        let started = Instant::now();
        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => break,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if started.elapsed() >= timeout {
                        return Err(LprmError::busy(format!(
                            "Repository coordinator lock is busy: {}",
                            path.display()
                        )));
                    }
                    thread::sleep(
                        timeout
                            .saturating_sub(started.elapsed())
                            .min(Duration::from_millis(10)),
                    );
                }
                Err(error) => return Err(persistence_error("acquire repository lock", error)),
            }
        }
        write_lock_holder(&mut file, operation)?;
        Ok(Self { file })
    }
}

impl Drop for RepositoryLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
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
        let audit_payload = json!({
            "reason": "pull request created",
            "from": null,
            "to": PullRequestStatus::Draft.as_str(),
            "revision_id": request.revision_id,
            "source_sha": request.source_sha,
            "target_sha": request.target_sha,
            "policy_hash": request.policy_hash,
            "check_config_hash": request.check_config_hash,
        });
        transaction
            .execute(
                "INSERT INTO audit_events(\
                    id, repository_id, pr_id, event_type, actor, payload_json, created_at\
                 ) VALUES (\
                    ?1, ?2, ?3, 'pr_created', ?4, ?5, \
                    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
                 )",
                (
                    &request.audit_id,
                    &request.repository_id,
                    &request.id,
                    &request.actor,
                    audit_payload.to_string(),
                ),
            )
            .map_err(|error| persistence_error("record pull request creation audit", error))?;
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
        evidence: &TransitionEvidence,
    ) -> Result<PullRequestRecord, LprmError> {
        self.transition_pull_request(
            repository_id,
            pull_request_id,
            PullRequestStatus::Closed,
            evidence,
        )
    }

    /// Changes status and records exact revision evidence in the same transaction.
    ///
    /// # Errors
    ///
    /// Returns a stale-revision error when evidence differs from the latest immutable revision, a
    /// validation error for an illegal state change, or an internal error on persistence failure.
    pub fn transition_pull_request(
        &mut self,
        repository_id: &str,
        pull_request_id: &str,
        next: PullRequestStatus,
        evidence: &TransitionEvidence,
    ) -> Result<PullRequestRecord, LprmError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start pull request transition", error))?;
        let current_status = transaction
            .query_row(
                "SELECT status FROM pull_requests WHERE repository_id = ?1 AND id = ?2",
                (repository_id, pull_request_id),
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| persistence_error("read pull request transition state", error))?
            .ok_or_else(|| LprmError::invalid_argument("Unknown pull request ID"))?;
        let current = parse_pull_request_status(&current_status)?;
        current.transition_to(next)?;
        let revision = transaction
            .query_row(
                "SELECT id, source_sha, target_sha, policy_hash \
                 FROM pr_revisions WHERE pr_id = ?1 ORDER BY revision_number DESC LIMIT 1",
                [pull_request_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| persistence_error("read pull request transition evidence", error))?
            .ok_or_else(|| LprmError::internal("Pull request has no revision history"))?;
        if revision
            != (
                evidence.revision_id.clone(),
                evidence.source_sha.clone(),
                evidence.target_sha.clone(),
                evidence.policy_hash.clone(),
            )
        {
            return Err(LprmError::stale_revision(
                "Pull request transition evidence is stale",
            ));
        }
        let changed = transaction
            .execute(
                "UPDATE pull_requests \
                 SET status = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE repository_id = ?2 AND id = ?3 AND status = ?4",
                (
                    next.as_str(),
                    repository_id,
                    pull_request_id,
                    current.as_str(),
                ),
            )
            .map_err(|error| persistence_error("transition pull request", error))?;
        if changed != 1 {
            return Err(LprmError::internal(
                "Pull request status changed concurrently",
            ));
        }
        let payload = json!({
            "reason": evidence.reason,
            "from": current.as_str(),
            "to": next.as_str(),
            "revision_id": evidence.revision_id,
            "source_sha": evidence.source_sha,
            "target_sha": evidence.target_sha,
            "policy_hash": evidence.policy_hash,
        });
        transaction
            .execute(
                "INSERT INTO audit_events(\
                    id, repository_id, pr_id, event_type, actor, payload_json, created_at\
                 ) VALUES (\
                    ?1, ?2, ?3, 'pr_status_transition', ?4, ?5, \
                    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
                 )",
                (
                    &evidence.audit_id,
                    repository_id,
                    pull_request_id,
                    &evidence.actor,
                    payload.to_string(),
                ),
            )
            .map_err(|error| persistence_error("record pull request transition audit", error))?;
        transaction
            .commit()
            .map_err(|error| persistence_error("commit pull request transition", error))?;
        self.pull_request(repository_id, pull_request_id)?
            .ok_or_else(|| LprmError::internal("Transitioned pull request could not be read"))
    }

    /// Persists a required-check suite and its resulting PR transition atomically.
    ///
    /// # Errors
    ///
    /// Returns a stale-revision error when the immutable evidence tuple differs, a validation
    /// error unless the PR is checking, or an internal error when persistence fails.
    pub fn record_check_suite(
        &mut self,
        repository_id: &str,
        pull_request_id: &str,
        request: &RecordCheckSuite,
    ) -> Result<PullRequestRecord, LprmError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start check suite transaction", error))?;
        let evidence = transaction
            .query_row(
                "SELECT p.status, r.id, r.source_sha, r.target_sha, r.policy_hash, \
                        r.check_config_hash \
                 FROM pull_requests p \
                 JOIN pr_revisions r ON r.pr_id = p.id \
                 WHERE p.repository_id = ?1 AND p.id = ?2 \
                 ORDER BY r.revision_number DESC LIMIT 1",
                (repository_id, pull_request_id),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| persistence_error("read check suite evidence", error))?
            .ok_or_else(|| LprmError::invalid_argument("Unknown pull request ID"))?;
        if evidence.0 != PullRequestStatus::Checking.as_str() {
            return Err(LprmError::validation_failed(
                "Pull request is not awaiting check results",
            ));
        }
        if evidence.1 != request.revision_id
            || evidence.2 != request.source_sha
            || evidence.3 != request.target_sha
            || evidence.4 != request.policy_hash
            || evidence.5 != request.check_config_hash
        {
            return Err(LprmError::stale_revision("Check suite evidence is stale"));
        }
        insert_check_runs(&transaction, request)?;
        let next = if request
            .checks
            .iter()
            .all(|check| check.status == CheckRunStatus::Passed)
        {
            PullRequestStatus::Approved
        } else {
            PullRequestStatus::Failed
        };
        PullRequestStatus::Checking.transition_to(next)?;
        let changed = transaction
            .execute(
                "UPDATE pull_requests \
                 SET status = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE repository_id = ?2 AND id = ?3 AND status = 'checking'",
                (next.as_str(), repository_id, pull_request_id),
            )
            .map_err(|error| persistence_error("finish pull request checks", error))?;
        if changed != 1 {
            return Err(LprmError::internal(
                "Pull request changed while recording checks",
            ));
        }
        record_check_suite_audit(&transaction, repository_id, pull_request_id, next, request)?;
        transaction
            .commit()
            .map_err(|error| persistence_error("commit check suite", error))?;
        self.pull_request(repository_id, pull_request_id)?
            .ok_or_else(|| LprmError::internal("Checked pull request could not be read"))
    }

    /// Lists persisted check evidence for one immutable PR revision.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot query or decode check records.
    pub fn list_check_runs(&self, revision_id: &str) -> Result<Vec<CheckRunRecord>, LprmError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, revision_id, name, status, exit_code, duration_ms, stdout_path, \
                        stderr_path, started_at, finished_at \
                 FROM check_runs WHERE revision_id = ?1 ORDER BY started_at, id",
            )
            .map_err(|error| persistence_error("prepare check run query", error))?;
        let rows = statement
            .query_map([revision_id], |row| {
                let status = row.get::<_, String>(3)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    status,
                    row.get::<_, Option<i32>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            })
            .map_err(|error| persistence_error("query check runs", error))?;
        rows.map(|row| {
            let (
                id,
                revision_id,
                name,
                status,
                exit_code,
                duration_ms,
                stdout_path,
                stderr_path,
                started_at,
                finished_at,
            ) = row.map_err(|error| persistence_error("decode check run", error))?;
            let duration_ms = u64::try_from(duration_ms)
                .map_err(|_| LprmError::internal("Persisted check duration is invalid"))?;
            Ok(CheckRunRecord {
                id,
                revision_id,
                name,
                status: CheckRunStatus::parse(&status)?,
                exit_code,
                duration_ms,
                stdout_path: PathBuf::from(stdout_path),
                stderr_path: PathBuf::from(stderr_path),
                started_at,
                finished_at,
            })
        })
        .collect()
    }

    /// Lists audit events in durable chronological order.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot query or decode an event payload.
    pub fn audit_events(
        &self,
        repository_id: &str,
        pull_request_id: Option<&str>,
    ) -> Result<Vec<AuditEventRecord>, LprmError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, pr_id, event_type, actor, payload_json, created_at \
                 FROM audit_events \
                 WHERE repository_id = ?1 AND (?2 IS NULL OR pr_id = ?2) \
                 ORDER BY created_at, id",
            )
            .map_err(|error| persistence_error("prepare audit event query", error))?;
        let rows = statement
            .query_map((repository_id, pull_request_id), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|error| persistence_error("query audit events", error))?;
        rows.map(|row| {
            let (id, pr_id, event_type, actor, payload, created_at) =
                row.map_err(|error| persistence_error("decode audit event", error))?;
            let payload = serde_json::from_str(&payload)
                .map_err(|error| persistence_error("decode audit event payload", error))?;
            Ok(AuditEventRecord {
                id,
                pr_id,
                event_type,
                actor,
                payload,
                created_at,
            })
        })
        .collect()
    }

    /// Admits an approved PR to the durable queue when all current evidence gates pass.
    ///
    /// # Errors
    ///
    /// Returns a validation error for unmet gates or an ineligible state, a stale-revision error
    /// for changed evidence, or an internal error when the atomic queue transaction fails.
    pub fn enqueue_pull_request(
        &mut self,
        repository_id: &str,
        pull_request_id: &str,
        request: &EnqueuePullRequest,
    ) -> Result<MergeQueueRecord, LprmError> {
        if !request.gates.all_satisfied() {
            return Err(LprmError::validation_failed(
                "Pull request does not satisfy every queue admission gate",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start queue admission transaction", error))?;
        let row = queue_admission_row(&transaction, repository_id, pull_request_id)?;
        let QueueAdmissionRow {
            status,
            revision_id,
            source_sha,
            target_sha,
            policy_hash,
            check_config_hash,
        } = row;
        let status = parse_pull_request_status(&status)?;
        status.transition_to(PullRequestStatus::Queued)?;
        let recorded = RevisionFingerprint {
            source_sha: source_sha.clone(),
            target_sha: target_sha.clone(),
            policy_hash: policy_hash.clone(),
            check_config_hash: check_config_hash.clone(),
        };
        if !recorded
            .assess(&request.current_fingerprint, status)
            .queue_evidence_current
        {
            return Err(LprmError::stale_revision(
                "Pull request evidence changed before queue admission",
            ));
        }
        ensure_no_active_queue_entry(&transaction, pull_request_id)?;
        transaction
            .execute(
                "INSERT INTO merge_queue(\
                    id, pr_id, revision_id, priority, status, enqueued_at\
                 ) VALUES (\
                    ?1, ?2, ?3, ?4, 'queued', strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
                 )",
                (
                    &request.queue_id,
                    pull_request_id,
                    &revision_id,
                    request.priority,
                ),
            )
            .map_err(|error| persistence_error("enqueue pull request", error))?;
        let changed = transaction
            .execute(
                "UPDATE pull_requests \
                 SET status = 'queued', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE repository_id = ?1 AND id = ?2 AND status = 'approved'",
                (repository_id, pull_request_id),
            )
            .map_err(|error| persistence_error("mark pull request queued", error))?;
        if changed != 1 {
            return Err(LprmError::internal(
                "Pull request changed during queue admission",
            ));
        }
        let payload = json!({
            "reason": request.reason,
            "from": "approved",
            "to": "queued",
            "revision_id": revision_id,
            "source_sha": source_sha,
            "target_sha": target_sha,
            "policy_hash": policy_hash,
            "check_config_hash": check_config_hash,
        });
        transaction
            .execute(
                "INSERT INTO audit_events(\
                    id, repository_id, pr_id, event_type, actor, payload_json, created_at\
                 ) VALUES (\
                    ?1, ?2, ?3, 'pr_status_transition', ?4, ?5, \
                    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
                 )",
                (
                    &request.audit_id,
                    repository_id,
                    pull_request_id,
                    &request.actor,
                    payload.to_string(),
                ),
            )
            .map_err(|error| persistence_error("record queue admission audit", error))?;
        transaction
            .commit()
            .map_err(|error| persistence_error("commit queue admission", error))?;

        self.merge_queue_record(repository_id, &request.queue_id)?
            .ok_or_else(|| LprmError::internal("Queued pull request could not be read"))
    }

    /// Lists durable queue entries for a repository in admission order.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot query or decode queue records.
    pub fn list_merge_queue(
        &self,
        repository_id: &str,
    ) -> Result<Vec<MergeQueueRecord>, LprmError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT q.id, q.pr_id, q.revision_id, q.priority, q.status, q.enqueued_at, \
                        q.started_at, q.finished_at \
                 FROM merge_queue q \
                 JOIN pull_requests p ON p.id = q.pr_id \
                 WHERE p.repository_id = ?1 \
                 ORDER BY q.enqueued_at, q.id",
            )
            .map_err(|error| persistence_error("prepare merge queue query", error))?;
        let rows = statement
            .query_map([repository_id], decode_merge_queue_record)
            .map_err(|error| persistence_error("query merge queue", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| persistence_error("decode merge queue", error))
    }

    fn merge_queue_record(
        &self,
        repository_id: &str,
        queue_id: &str,
    ) -> Result<Option<MergeQueueRecord>, LprmError> {
        self.connection
            .query_row(
                "SELECT q.id, q.pr_id, q.revision_id, q.priority, q.status, q.enqueued_at, \
                        q.started_at, q.finished_at \
                 FROM merge_queue q \
                 JOIN pull_requests p ON p.id = q.pr_id \
                 WHERE p.repository_id = ?1 AND q.id = ?2",
                (repository_id, queue_id),
                decode_merge_queue_record,
            )
            .optional()
            .map_err(|error| persistence_error("read merge queue entry", error))
    }

    /// Atomically claims the next queued PR and records its merge attempt.
    ///
    /// The caller must retain the matching [`RepositoryLock`] for the complete coordinator
    /// operation. The database's active-attempt constraint is an additional safety boundary.
    ///
    /// # Errors
    ///
    /// Returns `BUSY` if the repository already has an active attempt, a validation error if the
    /// selected PR is no longer queued, or an internal error when the transaction fails.
    pub fn start_next_merge_attempt(
        &mut self,
        repository_id: &str,
        request: &StartMergeAttempt,
    ) -> Result<Option<MergeAttemptRecord>, LprmError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start merge attempt transaction", error))?;
        ensure_no_active_merge_attempt(&transaction, repository_id)?;
        let Some(queued) = next_queued_merge(&transaction, repository_id)? else {
            transaction
                .commit()
                .map_err(|error| persistence_error("finish empty queue transaction", error))?;
            return Ok(None);
        };
        let status = parse_pull_request_status(&queued.status)?;
        status.transition_to(PullRequestStatus::Merging)?;
        insert_merge_attempt(&transaction, repository_id, request, &queued)?;
        update_claimed_queue(&transaction, &queued.queue_id)?;
        update_merging_pull_request(&transaction, repository_id, &queued.pull_request_id)?;
        record_merge_attempt_audit(&transaction, repository_id, request, &queued)?;
        transaction
            .commit()
            .map_err(|error| persistence_error("commit merge attempt", error))?;

        self.merge_attempt_record(repository_id, &request.attempt_id)?
            .ok_or_else(|| LprmError::internal("Merge attempt could not be read"))
            .map(Some)
    }

    /// Persists the exact candidate commit after integrated checks pass.
    ///
    /// # Errors
    ///
    /// Returns a validation error for malformed evidence or an attempt outside `preparing`, and an
    /// internal error when the atomic update cannot be committed.
    pub fn record_candidate_ready(
        &mut self,
        repository_id: &str,
        evidence: &CandidateMergeEvidence,
    ) -> Result<MergeAttemptRecord, LprmError> {
        ensure_object_id(&evidence.candidate_commit_sha, "candidate commit")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start candidate evidence transaction", error))?;
        let changed = transaction
            .execute(
                "UPDATE merge_attempts \
                 SET candidate_commit_sha = ?1, state = 'integrating' \
                 WHERE repository_id = ?2 AND id = ?3 AND state = 'preparing' \
                   AND candidate_commit_sha IS NULL",
                (
                    &evidence.candidate_commit_sha,
                    repository_id,
                    &evidence.attempt_id,
                ),
            )
            .map_err(|error| persistence_error("record candidate evidence", error))?;
        if changed != 1 {
            return Err(LprmError::validation_failed(
                "Merge attempt is not awaiting candidate evidence",
            ));
        }
        transaction
            .commit()
            .map_err(|error| persistence_error("commit candidate evidence", error))?;
        self.merge_attempt_record(repository_id, &evidence.attempt_id)?
            .ok_or_else(|| LprmError::internal("Updated merge attempt could not be read"))
    }

    /// Atomically completes a verified Git merge across attempt, queue, PR, and audit records.
    ///
    /// # Errors
    ///
    /// Returns a stale-revision error when any exact SHA differs from persisted evidence, a
    /// validation error for an ineligible attempt, or an internal error on transaction failure.
    pub fn complete_merge_attempt(
        &mut self,
        repository_id: &str,
        request: &CompleteMergeAttempt,
    ) -> Result<MergeAttemptRecord, LprmError> {
        ensure_completion_object_ids(request)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start merge completion transaction", error))?;
        let row = merge_completion_row(&transaction, repository_id, &request.attempt_id)?;
        validate_merge_completion(&row, request)?;
        update_completed_attempt(&transaction, repository_id, request)?;
        update_completed_queue(&transaction, &row.queue_id)?;
        update_merged_pull_request(&transaction, repository_id, &row.pull_request_id)?;
        record_merge_completion_audit(&transaction, repository_id, &row, request)?;
        transaction
            .commit()
            .map_err(|error| persistence_error("commit merge completion", error))?;
        self.merge_attempt_record(repository_id, &request.attempt_id)?
            .ok_or_else(|| LprmError::internal("Completed merge attempt could not be read"))
    }

    /// Atomically closes an unintegrated attempt and restores its queue entry for retry.
    ///
    /// # Errors
    ///
    /// Returns a stale-revision error when exact SHA evidence differs, a validation error when the
    /// attempt is no longer active, or an internal error when the transaction cannot commit.
    pub fn requeue_interrupted_merge(
        &mut self,
        repository_id: &str,
        request: &RequeueMergeAttempt,
    ) -> Result<MergeAttemptRecord, LprmError> {
        ensure_object_id(&request.target_before_sha, "target before")?;
        ensure_object_id(&request.source_sha, "source")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start merge recovery transaction", error))?;
        let row = merge_completion_row(&transaction, repository_id, &request.attempt_id)?;
        if !matches!(
            row.state.as_str(),
            "preparing" | "testing" | "integrating" | "reconciling"
        ) {
            return Err(LprmError::validation_failed(
                "Merge attempt is not active for recovery",
            ));
        }
        if row.target_before_sha != request.target_before_sha
            || row.source_sha != request.source_sha
        {
            return Err(LprmError::stale_revision(
                "Recovery evidence does not match the active attempt",
            ));
        }
        if row.pull_request_status != PullRequestStatus::Merging.as_str() {
            return Err(LprmError::validation_failed(
                "Pull request is not merging during recovery",
            ));
        }
        update_requeued_attempt(&transaction, repository_id, request)?;
        update_requeued_queue(&transaction, &row.queue_id)?;
        update_requeued_pull_request(&transaction, repository_id, &row.pull_request_id)?;
        record_merge_requeue_audit(&transaction, repository_id, &row, request)?;
        transaction
            .commit()
            .map_err(|error| persistence_error("commit merge recovery", error))?;
        self.merge_attempt_record(repository_id, &request.attempt_id)?
            .ok_or_else(|| LprmError::internal("Recovered merge attempt could not be read"))
    }

    /// Marks an active attempt as requiring Git/database reconciliation.
    ///
    /// # Errors
    ///
    /// Returns a validation error if the attempt is not active, or an internal error when the
    /// state update cannot be persisted.
    pub fn mark_merge_attempt_reconciling(
        &mut self,
        repository_id: &str,
        attempt_id: &str,
        error_code: &str,
    ) -> Result<MergeAttemptRecord, LprmError> {
        if error_code.is_empty() {
            return Err(LprmError::invalid_argument(
                "Merge reconciliation error code must not be empty",
            ));
        }
        let changed = self
            .connection
            .execute(
                "UPDATE merge_attempts \
                 SET state = 'reconciling', error_code = ?1 \
                 WHERE repository_id = ?2 AND id = ?3 \
                   AND state IN ('preparing', 'testing', 'integrating', 'reconciling')",
                (error_code, repository_id, attempt_id),
            )
            .map_err(|error| persistence_error("mark merge attempt reconciling", error))?;
        if changed != 1 {
            return Err(LprmError::validation_failed(
                "Merge attempt is not active for reconciliation",
            ));
        }
        self.merge_attempt_record(repository_id, attempt_id)?
            .ok_or_else(|| LprmError::internal("Reconciling merge attempt could not be read"))
    }

    /// Lists merge attempts for a repository in start order.
    ///
    /// # Errors
    ///
    /// Returns an internal error when SQLite cannot query or decode attempt records.
    pub fn list_merge_attempts(
        &self,
        repository_id: &str,
    ) -> Result<Vec<MergeAttemptRecord>, LprmError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT a.id, a.repository_id, a.queue_id, q.pr_id, q.revision_id, \
                        a.target_before_sha, a.source_sha, a.candidate_commit_sha, \
                        a.target_after_sha, a.state, a.error_code, a.started_at, a.finished_at \
                 FROM merge_attempts a \
                 JOIN merge_queue q ON q.id = a.queue_id \
                 WHERE a.repository_id = ?1 \
                 ORDER BY a.started_at, a.id",
            )
            .map_err(|error| persistence_error("prepare merge attempt query", error))?;
        let rows = statement
            .query_map([repository_id], decode_merge_attempt_record)
            .map_err(|error| persistence_error("query merge attempts", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| persistence_error("decode merge attempts", error))
    }

    fn merge_attempt_record(
        &self,
        repository_id: &str,
        attempt_id: &str,
    ) -> Result<Option<MergeAttemptRecord>, LprmError> {
        self.connection
            .query_row(
                "SELECT a.id, a.repository_id, a.queue_id, q.pr_id, q.revision_id, \
                        a.target_before_sha, a.source_sha, a.candidate_commit_sha, \
                        a.target_after_sha, a.state, a.error_code, a.started_at, a.finished_at \
                 FROM merge_attempts a \
                 JOIN merge_queue q ON q.id = a.queue_id \
                 WHERE a.repository_id = ?1 AND a.id = ?2",
                (repository_id, attempt_id),
                decode_merge_attempt_record,
            )
            .optional()
            .map_err(|error| persistence_error("read merge attempt", error))
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

fn insert_check_runs(
    transaction: &Transaction<'_>,
    request: &RecordCheckSuite,
) -> Result<(), LprmError> {
    let mut names = std::collections::BTreeSet::new();
    for check in &request.checks {
        if check.name.is_empty() || !names.insert(&check.name) {
            return Err(LprmError::invalid_argument(
                "Check suite names must be non-empty and unique",
            ));
        }
        let duration_ms = i64::try_from(check.duration_ms)
            .map_err(|_| LprmError::invalid_argument("Check duration is too large"))?;
        transaction
            .execute(
                "INSERT INTO check_runs(\
                    id, revision_id, name, status, exit_code, duration_ms, stdout_path, \
                    stderr_path, started_at, finished_at\
                 ) VALUES (\
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, \
                    strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
                 )",
                (
                    &check.id,
                    &request.revision_id,
                    &check.name,
                    check.status.as_str(),
                    check.exit_code,
                    duration_ms,
                    check.stdout_path.to_string_lossy().as_ref(),
                    check.stderr_path.to_string_lossy().as_ref(),
                ),
            )
            .map_err(|error| persistence_error("record check run", error))?;
    }
    Ok(())
}

fn record_check_suite_audit(
    transaction: &Transaction<'_>,
    repository_id: &str,
    pull_request_id: &str,
    next: PullRequestStatus,
    request: &RecordCheckSuite,
) -> Result<(), LprmError> {
    let payload = json!({
        "reason": request.reason,
        "from": "checking",
        "to": next.as_str(),
        "revision_id": request.revision_id,
        "source_sha": request.source_sha,
        "target_sha": request.target_sha,
        "policy_hash": request.policy_hash,
        "check_config_hash": request.check_config_hash,
        "check_count": request.checks.len(),
    });
    transaction
        .execute(
            "INSERT INTO audit_events(\
                id, repository_id, pr_id, event_type, actor, payload_json, created_at\
             ) VALUES (\
                ?1, ?2, ?3, 'pr_status_transition', ?4, ?5, \
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
             )",
            (
                &request.audit_id,
                repository_id,
                pull_request_id,
                &request.actor,
                payload.to_string(),
            ),
        )
        .map_err(|error| persistence_error("record check suite audit", error))?;
    Ok(())
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

fn decode_merge_queue_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<MergeQueueRecord> {
    Ok(MergeQueueRecord {
        id: row.get(0)?,
        pr_id: row.get(1)?,
        revision_id: row.get(2)?,
        priority: row.get(3)?,
        status: row.get(4)?,
        enqueued_at: row.get(5)?,
        started_at: row.get(6)?,
        finished_at: row.get(7)?,
    })
}

fn decode_merge_attempt_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<MergeAttemptRecord> {
    Ok(MergeAttemptRecord {
        id: row.get(0)?,
        repository_id: row.get(1)?,
        queue_id: row.get(2)?,
        pr_id: row.get(3)?,
        revision_id: row.get(4)?,
        target_before_sha: row.get(5)?,
        source_sha: row.get(6)?,
        candidate_commit_sha: row.get(7)?,
        target_after_sha: row.get(8)?,
        state: row.get(9)?,
        error_code: row.get(10)?,
        started_at: row.get(11)?,
        finished_at: row.get(12)?,
    })
}

fn ensure_no_active_merge_attempt(
    transaction: &Transaction<'_>,
    repository_id: &str,
) -> Result<(), LprmError> {
    let active = transaction
        .query_row(
            "SELECT EXISTS(\
                SELECT 1 FROM merge_attempts \
                WHERE repository_id = ?1 \
                  AND state IN ('preparing', 'testing', 'integrating', 'reconciling')\
             )",
            [repository_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| persistence_error("check active merge attempt", error))?;
    if active {
        Err(LprmError::busy(
            "Repository already has an active merge attempt",
        ))
    } else {
        Ok(())
    }
}

fn next_queued_merge(
    transaction: &Transaction<'_>,
    repository_id: &str,
) -> Result<Option<QueuedMergeRow>, LprmError> {
    transaction
        .query_row(
            "SELECT q.id, q.pr_id, q.revision_id, p.status, r.target_sha, r.source_sha \
             FROM merge_queue q \
             JOIN pull_requests p ON p.id = q.pr_id \
             JOIN pr_revisions r ON r.id = q.revision_id AND r.pr_id = p.id \
             WHERE p.repository_id = ?1 AND q.status = 'queued' \
             ORDER BY q.priority DESC, q.enqueued_at, q.id \
             LIMIT 1",
            [repository_id],
            |row| {
                Ok(QueuedMergeRow {
                    queue_id: row.get(0)?,
                    pull_request_id: row.get(1)?,
                    revision_id: row.get(2)?,
                    status: row.get(3)?,
                    target_sha: row.get(4)?,
                    source_sha: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|error| persistence_error("select next queued pull request", error))
}

fn insert_merge_attempt(
    transaction: &Transaction<'_>,
    repository_id: &str,
    request: &StartMergeAttempt,
    queued: &QueuedMergeRow,
) -> Result<(), LprmError> {
    transaction
        .execute(
            "INSERT INTO merge_attempts(\
                id, repository_id, queue_id, target_before_sha, source_sha, state, started_at\
             ) VALUES (\
                ?1, ?2, ?3, ?4, ?5, 'preparing', strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
             )",
            (
                &request.attempt_id,
                repository_id,
                &queued.queue_id,
                &queued.target_sha,
                &queued.source_sha,
            ),
        )
        .map_err(|error| persistence_error("insert merge attempt", error))?;
    Ok(())
}

fn update_claimed_queue(transaction: &Transaction<'_>, queue_id: &str) -> Result<(), LprmError> {
    let changed = transaction
        .execute(
            "UPDATE merge_queue \
             SET status = 'started', started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?1 AND status = 'queued'",
            [queue_id],
        )
        .map_err(|error| persistence_error("mark queue entry started", error))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(LprmError::internal(
            "Queue entry changed while starting merge attempt",
        ))
    }
}

fn update_merging_pull_request(
    transaction: &Transaction<'_>,
    repository_id: &str,
    pull_request_id: &str,
) -> Result<(), LprmError> {
    let changed = transaction
        .execute(
            "UPDATE pull_requests \
             SET status = 'merging', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE repository_id = ?1 AND id = ?2 AND status = 'queued'",
            (repository_id, pull_request_id),
        )
        .map_err(|error| persistence_error("mark pull request merging", error))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(LprmError::internal(
            "Pull request changed while starting merge attempt",
        ))
    }
}

fn record_merge_attempt_audit(
    transaction: &Transaction<'_>,
    repository_id: &str,
    request: &StartMergeAttempt,
    queued: &QueuedMergeRow,
) -> Result<(), LprmError> {
    let payload = json!({
        "from": "queued",
        "to": "merging",
        "queue_id": queued.queue_id,
        "attempt_id": request.attempt_id,
        "revision_id": queued.revision_id,
        "source_sha": queued.source_sha,
        "target_sha": queued.target_sha,
    });
    transaction
        .execute(
            "INSERT INTO audit_events(\
                id, repository_id, pr_id, event_type, actor, payload_json, created_at\
             ) VALUES (\
                ?1, ?2, ?3, 'merge_attempt_started', ?4, ?5, \
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
             )",
            (
                &request.audit_id,
                repository_id,
                &queued.pull_request_id,
                &request.actor,
                payload.to_string(),
            ),
        )
        .map_err(|error| persistence_error("record merge attempt audit", error))?;
    Ok(())
}

fn merge_completion_row(
    transaction: &Transaction<'_>,
    repository_id: &str,
    attempt_id: &str,
) -> Result<MergeCompletionRow, LprmError> {
    transaction
        .query_row(
            "SELECT a.state, a.target_before_sha, a.source_sha, a.candidate_commit_sha, \
                    q.id, q.pr_id, p.status \
             FROM merge_attempts a \
             JOIN merge_queue q ON q.id = a.queue_id \
             JOIN pull_requests p ON p.id = q.pr_id \
             WHERE a.repository_id = ?1 AND a.id = ?2 AND p.repository_id = ?1",
            (repository_id, attempt_id),
            |row| {
                Ok(MergeCompletionRow {
                    state: row.get(0)?,
                    target_before_sha: row.get(1)?,
                    source_sha: row.get(2)?,
                    candidate_commit_sha: row.get(3)?,
                    queue_id: row.get(4)?,
                    pull_request_id: row.get(5)?,
                    pull_request_status: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(|error| persistence_error("read merge completion evidence", error))?
        .ok_or_else(|| LprmError::invalid_argument("Unknown merge attempt ID"))
}

fn validate_merge_completion(
    row: &MergeCompletionRow,
    request: &CompleteMergeAttempt,
) -> Result<(), LprmError> {
    if !matches!(row.state.as_str(), "integrating" | "reconciling") {
        return Err(LprmError::validation_failed(
            "Merge attempt is not ready for completion",
        ));
    }
    let status = parse_pull_request_status(&row.pull_request_status)?;
    status.transition_to(PullRequestStatus::Merged)?;
    if row.target_before_sha != request.target_before_sha
        || row.source_sha != request.source_sha
        || row.candidate_commit_sha.as_deref() != Some(request.candidate_commit_sha.as_str())
    {
        return Err(LprmError::stale_revision(
            "Verified merge evidence does not match the active attempt",
        ));
    }
    Ok(())
}

fn update_completed_attempt(
    transaction: &Transaction<'_>,
    repository_id: &str,
    request: &CompleteMergeAttempt,
) -> Result<(), LprmError> {
    let changed = transaction
        .execute(
            "UPDATE merge_attempts \
             SET target_after_sha = ?1, state = 'succeeded', error_code = NULL, \
                 finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE repository_id = ?2 AND id = ?3 AND state IN ('integrating', 'reconciling') \
               AND target_before_sha = ?4 AND source_sha = ?5 AND candidate_commit_sha = ?6",
            (
                &request.target_after_sha,
                repository_id,
                &request.attempt_id,
                &request.target_before_sha,
                &request.source_sha,
                &request.candidate_commit_sha,
            ),
        )
        .map_err(|error| persistence_error("complete merge attempt", error))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(LprmError::stale_revision(
            "Merge attempt changed before completion",
        ))
    }
}

fn update_completed_queue(transaction: &Transaction<'_>, queue_id: &str) -> Result<(), LprmError> {
    let changed = transaction
        .execute(
            "UPDATE merge_queue \
             SET status = 'completed', finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?1 AND status = 'started'",
            [queue_id],
        )
        .map_err(|error| persistence_error("complete merge queue entry", error))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(LprmError::internal(
            "Merge queue entry changed before completion",
        ))
    }
}

fn update_merged_pull_request(
    transaction: &Transaction<'_>,
    repository_id: &str,
    pull_request_id: &str,
) -> Result<(), LprmError> {
    let changed = transaction
        .execute(
            "UPDATE pull_requests \
             SET status = 'merged', merged_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE repository_id = ?1 AND id = ?2 AND status = 'merging'",
            (repository_id, pull_request_id),
        )
        .map_err(|error| persistence_error("mark pull request merged", error))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(LprmError::internal(
            "Pull request changed before merge completion",
        ))
    }
}

fn record_merge_completion_audit(
    transaction: &Transaction<'_>,
    repository_id: &str,
    row: &MergeCompletionRow,
    request: &CompleteMergeAttempt,
) -> Result<(), LprmError> {
    let payload = json!({
        "from": "merging",
        "to": "merged",
        "attempt_id": request.attempt_id,
        "queue_id": row.queue_id,
        "target_before_sha": request.target_before_sha,
        "source_sha": request.source_sha,
        "candidate_commit_sha": request.candidate_commit_sha,
        "target_after_sha": request.target_after_sha,
    });
    transaction
        .execute(
            "INSERT INTO audit_events(\
                id, repository_id, pr_id, event_type, actor, payload_json, created_at\
             ) VALUES (\
                ?1, ?2, ?3, 'merge_completed', ?4, ?5, \
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
             )",
            (
                &request.audit_id,
                repository_id,
                &row.pull_request_id,
                &request.actor,
                payload.to_string(),
            ),
        )
        .map_err(|error| persistence_error("record merge completion audit", error))?;
    Ok(())
}

fn update_requeued_attempt(
    transaction: &Transaction<'_>,
    repository_id: &str,
    request: &RequeueMergeAttempt,
) -> Result<(), LprmError> {
    let changed = transaction
        .execute(
            "UPDATE merge_attempts \
             SET state = 'failed', error_code = 'INTERRUPTED_RETRYABLE', \
                 finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE repository_id = ?1 AND id = ?2 \
               AND state IN ('preparing', 'testing', 'integrating', 'reconciling') \
               AND target_before_sha = ?3 AND source_sha = ?4",
            (
                repository_id,
                &request.attempt_id,
                &request.target_before_sha,
                &request.source_sha,
            ),
        )
        .map_err(|error| persistence_error("close interrupted merge attempt", error))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(LprmError::stale_revision(
            "Merge attempt changed before recovery",
        ))
    }
}

fn update_requeued_queue(transaction: &Transaction<'_>, queue_id: &str) -> Result<(), LprmError> {
    let changed = transaction
        .execute(
            "UPDATE merge_queue SET status = 'queued', started_at = NULL, finished_at = NULL \
             WHERE id = ?1 AND status = 'started'",
            [queue_id],
        )
        .map_err(|error| persistence_error("requeue interrupted merge", error))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(LprmError::internal(
            "Merge queue entry changed before recovery",
        ))
    }
}

fn update_requeued_pull_request(
    transaction: &Transaction<'_>,
    repository_id: &str,
    pull_request_id: &str,
) -> Result<(), LprmError> {
    let changed = transaction
        .execute(
            "UPDATE pull_requests \
             SET status = 'queued', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE repository_id = ?1 AND id = ?2 AND status = 'merging'",
            (repository_id, pull_request_id),
        )
        .map_err(|error| persistence_error("restore interrupted pull request", error))?;
    if changed == 1 {
        Ok(())
    } else {
        Err(LprmError::internal(
            "Pull request changed before merge recovery",
        ))
    }
}

fn record_merge_requeue_audit(
    transaction: &Transaction<'_>,
    repository_id: &str,
    row: &MergeCompletionRow,
    request: &RequeueMergeAttempt,
) -> Result<(), LprmError> {
    let payload = json!({
        "from": "merging",
        "to": "queued",
        "attempt_id": request.attempt_id,
        "queue_id": row.queue_id,
        "target_before_sha": request.target_before_sha,
        "source_sha": request.source_sha,
        "reason": "interrupted_before_integration",
    });
    transaction
        .execute(
            "INSERT INTO audit_events(\
                id, repository_id, pr_id, event_type, actor, payload_json, created_at\
             ) VALUES (\
                ?1, ?2, ?3, 'merge_requeued_after_recovery', ?4, ?5, \
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')\
             )",
            (
                &request.audit_id,
                repository_id,
                &row.pull_request_id,
                &request.actor,
                payload.to_string(),
            ),
        )
        .map_err(|error| persistence_error("record merge requeue audit", error))?;
    Ok(())
}

fn ensure_completion_object_ids(request: &CompleteMergeAttempt) -> Result<(), LprmError> {
    ensure_object_id(&request.target_before_sha, "target before")?;
    ensure_object_id(&request.source_sha, "source")?;
    ensure_object_id(&request.candidate_commit_sha, "candidate commit")?;
    ensure_object_id(&request.target_after_sha, "target after")
}

fn ensure_object_id(value: &str, role: &str) -> Result<(), LprmError> {
    if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(LprmError::validation_failed(format!(
            "Merge {role} is not a full object ID"
        )))
    }
}

fn queue_admission_row(
    transaction: &Transaction<'_>,
    repository_id: &str,
    pull_request_id: &str,
) -> Result<QueueAdmissionRow, LprmError> {
    transaction
        .query_row(
            "SELECT p.status, r.id, r.source_sha, r.target_sha, r.policy_hash, \
                    r.check_config_hash \
             FROM pull_requests p \
             JOIN pr_revisions r ON r.pr_id = p.id \
             WHERE p.repository_id = ?1 AND p.id = ?2 \
             ORDER BY r.revision_number DESC LIMIT 1",
            (repository_id, pull_request_id),
            |row| {
                Ok(QueueAdmissionRow {
                    status: row.get(0)?,
                    revision_id: row.get(1)?,
                    source_sha: row.get(2)?,
                    target_sha: row.get(3)?,
                    policy_hash: row.get(4)?,
                    check_config_hash: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|error| persistence_error("read queue admission evidence", error))?
        .ok_or_else(|| LprmError::invalid_argument("Unknown pull request ID"))
}

fn ensure_no_active_queue_entry(
    transaction: &Transaction<'_>,
    pull_request_id: &str,
) -> Result<(), LprmError> {
    let already_active = transaction
        .query_row(
            "SELECT EXISTS(\
                SELECT 1 FROM merge_queue \
                WHERE pr_id = ?1 AND status IN ('queued', 'started')\
             )",
            [pull_request_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| persistence_error("check active queue entry", error))?;
    if already_active {
        Err(LprmError::validation_failed(
            "Pull request already has an active queue entry",
        ))
    } else {
        Ok(())
    }
}

fn open_lock_file(path: &Path) -> Result<File, LprmError> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|error| persistence_error("open repository lock", error))?;
    set_private_file_permissions(path)?;
    Ok(file)
}

fn write_lock_holder(file: &mut File, operation: &str) -> Result<(), LprmError> {
    let acquired_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| persistence_error("read lock acquisition time", error))?
        .as_millis();
    let holder = LockHolder {
        pid: std::process::id(),
        operation,
        acquired_unix_ms,
    };
    file.set_len(0)
        .map_err(|error| persistence_error("truncate lock metadata", error))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| persistence_error("seek lock metadata", error))?;
    serde_json::to_writer(&mut *file, &holder)
        .map_err(|error| persistence_error("write lock metadata", error))?;
    file.write_all(b"\n")
        .map_err(|error| persistence_error("finish lock metadata", error))?;
    file.sync_data()
        .map_err(|error| persistence_error("sync lock metadata", error))
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

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), LprmError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| persistence_error("set private lock permissions", error))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), LprmError> {
    Ok(())
}
