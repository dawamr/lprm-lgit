use lprm_store::Store;
use tempfile::tempdir;

#[test]
fn opening_a_new_database_applies_schema_one_idempotently() {
    let directory = tempdir().expect("temporary directory should be created");
    let path = directory.path().join("state/lprm.db");

    let first = Store::open(&path).expect("first open should migrate");
    assert_eq!(first.schema_version().expect("version should read"), 1);
    drop(first);

    let second = Store::open(&path).expect("second open should be idempotent");
    assert_eq!(second.schema_version().expect("version should read"), 1);
    assert_eq!(
        second
            .connection()
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .expect("foreign key setting should read"),
        1
    );
}

#[test]
fn schema_contains_every_normative_entity() {
    let store = Store::open_in_memory().expect("memory database should migrate");
    let mut statement = store
        .connection()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("schema query should prepare");
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("schema query should run")
        .collect::<Result<Vec<_>, _>>()
        .expect("schema rows should decode");

    for required in [
        "approvals",
        "audit_events",
        "check_runs",
        "merge_attempts",
        "merge_queue",
        "pr_revisions",
        "pull_requests",
        "repositories",
        "review_runs",
        "worktrees",
    ] {
        assert!(
            names.iter().any(|name| name == required),
            "missing {required}"
        );
    }
}

#[test]
fn transactions_roll_back_and_foreign_keys_reject_orphans() {
    let mut store = Store::open_in_memory().expect("memory database should migrate");
    let transaction = store
        .connection_mut()
        .transaction()
        .expect("transaction should start");
    transaction
        .execute(
            "INSERT INTO repositories(id, common_git_dir, target_branch, created_at, config_hash) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ["repo-1", "/tmp/repo", "dev", "2026-11-05T00:00:00Z", "hash"],
        )
        .expect("repository should insert inside transaction");
    transaction
        .rollback()
        .expect("transaction should roll back");

    let repository_count: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM repositories", [], |row| row.get(0))
        .expect("repository count should read");
    assert_eq!(repository_count, 0);

    let orphan_result = store.connection().execute(
        "INSERT INTO worktrees(id, repository_id, branch, absolute_path, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        [
            "worktree-1",
            "missing-repository",
            "feature/a",
            "/tmp/worktree",
            "2026-11-05T00:00:00Z",
        ],
    );
    assert!(orphan_result.is_err());
}

#[test]
fn file_databases_use_wal_mode() {
    let directory = tempdir().expect("temporary directory should be created");
    let store = Store::open(&directory.path().join("lprm.db")).expect("database should open");
    let journal_mode: String = store
        .connection()
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("journal mode should read");

    assert_eq!(journal_mode, "wal");
}
