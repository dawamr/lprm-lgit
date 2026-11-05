use std::path::Path;

use lprm_core::PullRequestStatus;
use lprm_store::{NewPullRequest, Store};

fn initialized_store() -> Store {
    let store = Store::open_in_memory().expect("memory database should migrate");
    store
        .register_repository("repo-1", Path::new("/tmp/repo/.git"), "dev", "config")
        .expect("repository should register");
    store
        .register_worktree(
            "worktree-1",
            "repo-1",
            "feature/a",
            Path::new("/tmp/worktree-a"),
            Some("codex"),
        )
        .expect("worktree should register");
    store
}

fn new_pull_request() -> NewPullRequest {
    NewPullRequest {
        id: "pr-1".to_owned(),
        repository_id: "repo-1".to_owned(),
        worktree_id: Some("worktree-1".to_owned()),
        title: "Add A".to_owned(),
        body: "Body".to_owned(),
        source_branch: "feature/a".to_owned(),
        target_branch: "dev".to_owned(),
        revision_id: "revision-1".to_owned(),
        source_sha: "1111111111111111111111111111111111111111".to_owned(),
        target_sha: "0000000000000000000000000000000000000000".to_owned(),
        policy_hash: "policy".to_owned(),
        check_config_hash: "checks".to_owned(),
    }
}

#[test]
fn creates_pr_and_first_revision_atomically_then_queries_history() {
    let mut store = initialized_store();

    let created = store
        .create_pull_request(&new_pull_request())
        .expect("pull request should be created");

    assert_eq!(created.id, "pr-1");
    assert_eq!(created.status, PullRequestStatus::Draft);
    assert_eq!(created.revisions.len(), 1);
    assert_eq!(created.revisions[0].revision_number, 1);
    assert_eq!(
        created.revisions[0].source_sha,
        "1111111111111111111111111111111111111111"
    );
    assert_eq!(
        store
            .list_pull_requests("repo-1", Some(PullRequestStatus::Draft))
            .expect("pull requests should list"),
        std::slice::from_ref(&created)
    );
    assert_eq!(
        store
            .pull_request("repo-1", "pr-1")
            .expect("pull request should query"),
        Some(created)
    );
}

#[test]
fn duplicate_id_rolls_back_without_a_second_revision() {
    let mut store = initialized_store();
    let request = new_pull_request();
    store
        .create_pull_request(&request)
        .expect("first pull request should be created");

    assert!(store.create_pull_request(&request).is_err());

    let revision_count: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM pr_revisions", [], |row| row.get(0))
        .expect("revision count should query");
    assert_eq!(revision_count, 1);
}

#[test]
fn close_uses_state_guard_and_preserves_revision_history() {
    let mut store = initialized_store();
    store
        .create_pull_request(&new_pull_request())
        .expect("pull request should be created");

    let closed = store
        .close_pull_request("repo-1", "pr-1")
        .expect("draft pull request should close");

    assert_eq!(closed.status, PullRequestStatus::Closed);
    assert_eq!(closed.revisions.len(), 1);
    assert!(store.close_pull_request("repo-1", "pr-1").is_err());
}
