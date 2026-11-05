use std::path::{Path, PathBuf};

use lprm_core::{ErrorCode, PullRequestStatus};
use lprm_store::{
    CheckRunEvidence, CheckRunStatus, NewPullRequest, NewRevision, RecordCheckSuite, Store,
    TransitionEvidence,
};

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
        audit_id: "audit-1".to_owned(),
        actor: "codex".to_owned(),
    }
}

fn checking_store() -> Store {
    let mut store = initialized_store();
    store.create_pull_request(&new_pull_request()).unwrap();
    for (index, status) in [PullRequestStatus::Ready, PullRequestStatus::Checking]
        .into_iter()
        .enumerate()
    {
        store
            .transition_pull_request(
                "repo-1",
                "pr-1",
                status,
                &TransitionEvidence {
                    audit_id: format!("audit-check-transition-{index}"),
                    actor: "codex".to_owned(),
                    reason: "run checks".to_owned(),
                    revision_id: "revision-1".to_owned(),
                    source_sha: "1111111111111111111111111111111111111111".to_owned(),
                    target_sha: "0000000000000000000000000000000000000000".to_owned(),
                    policy_hash: "policy".to_owned(),
                },
            )
            .unwrap();
    }
    store
}

fn check_suite(status: CheckRunStatus) -> RecordCheckSuite {
    RecordCheckSuite {
        audit_id: "audit-check-suite".to_owned(),
        actor: "codex".to_owned(),
        reason: "required checks completed".to_owned(),
        revision_id: "revision-1".to_owned(),
        source_sha: "1111111111111111111111111111111111111111".to_owned(),
        target_sha: "0000000000000000000000000000000000000000".to_owned(),
        policy_hash: "policy".to_owned(),
        check_config_hash: "checks".to_owned(),
        checks: vec![CheckRunEvidence {
            id: "check-1".to_owned(),
            name: "test".to_owned(),
            status,
            exit_code: Some(i32::from(status != CheckRunStatus::Passed)),
            duration_ms: 42,
            stdout_path: PathBuf::from("/tmp/test.stdout.log"),
            stderr_path: PathBuf::from("/tmp/test.stderr.log"),
        }],
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

    let evidence = TransitionEvidence {
        audit_id: "audit-2".to_owned(),
        actor: "operator".to_owned(),
        reason: "close".to_owned(),
        revision_id: "revision-1".to_owned(),
        source_sha: "1111111111111111111111111111111111111111".to_owned(),
        target_sha: "0000000000000000000000000000000000000000".to_owned(),
        policy_hash: "policy".to_owned(),
    };
    let closed = store
        .close_pull_request("repo-1", "pr-1", &evidence)
        .expect("draft pull request should close");

    assert_eq!(closed.status, PullRequestStatus::Closed);
    assert_eq!(closed.revisions.len(), 1);
    assert!(
        store
            .close_pull_request("repo-1", "pr-1", &evidence)
            .is_err()
    );
}

#[test]
fn transition_audit_is_ordered_and_rolls_back_on_stale_evidence() {
    let mut store = initialized_store();
    store
        .create_pull_request(&new_pull_request())
        .expect("pull request should be created");
    let stale = TransitionEvidence {
        audit_id: "audit-2".to_owned(),
        actor: "operator".to_owned(),
        reason: "close stale request".to_owned(),
        revision_id: "revision-1".to_owned(),
        source_sha: "2222222222222222222222222222222222222222".to_owned(),
        target_sha: "0000000000000000000000000000000000000000".to_owned(),
        policy_hash: "policy".to_owned(),
    };

    let stale_error = store
        .transition_pull_request("repo-1", "pr-1", PullRequestStatus::Closed, &stale)
        .expect_err("stale evidence must fail");
    assert_eq!(stale_error.code, ErrorCode::StaleRevision);
    assert_eq!(
        store
            .pull_request("repo-1", "pr-1")
            .expect("PR should query")
            .expect("PR should exist")
            .status,
        PullRequestStatus::Draft
    );
    assert_eq!(
        store
            .audit_events("repo-1", Some("pr-1"))
            .expect("audit should query")
            .len(),
        1
    );

    let current = TransitionEvidence {
        source_sha: "1111111111111111111111111111111111111111".to_owned(),
        ..stale
    };
    store
        .transition_pull_request("repo-1", "pr-1", PullRequestStatus::Closed, &current)
        .expect("current evidence should transition");
    let events = store
        .audit_events("repo-1", Some("pr-1"))
        .expect("audit should query");

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "pr_created");
    assert_eq!(events[1].event_type, "pr_status_transition");
    assert_eq!(events[1].actor, "operator");
    assert_eq!(events[1].payload["reason"], "close stale request");
    assert_eq!(events[1].payload["from"], "draft");
    assert_eq!(events[1].payload["to"], "closed");
    assert_eq!(events[1].payload["revision_id"], "revision-1");
    assert_eq!(
        events[1].payload["source_sha"],
        "1111111111111111111111111111111111111111"
    );
    assert_eq!(
        events[1].payload["target_sha"],
        "0000000000000000000000000000000000000000"
    );
    assert_eq!(events[1].payload["policy_hash"], "policy");
    assert_ne!(events[1].created_at, "");
}

#[test]
fn passing_check_suite_is_persisted_before_approval() {
    let mut store = checking_store();

    let pull_request = store
        .record_check_suite("repo-1", "pr-1", &check_suite(CheckRunStatus::Passed))
        .expect("passing suite should persist");

    assert_eq!(pull_request.status, PullRequestStatus::Approved);
    let checks = store.list_check_runs("revision-1").unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name, "test");
    assert_eq!(checks[0].status, CheckRunStatus::Passed);
    assert!(
        store
            .audit_events("repo-1", Some("pr-1"))
            .unwrap()
            .iter()
            .any(|event| event.payload["to"] == "approved")
    );
}

#[test]
fn failing_check_suite_is_persisted_without_approval() {
    let mut store = checking_store();

    let pull_request = store
        .record_check_suite("repo-1", "pr-1", &check_suite(CheckRunStatus::Failed))
        .expect("failing suite should persist");

    assert_eq!(pull_request.status, PullRequestStatus::Failed);
    assert_eq!(
        store.list_check_runs("revision-1").unwrap()[0].status,
        CheckRunStatus::Failed
    );
}

fn next_revision(target: &str) -> NewRevision {
    NewRevision {
        revision_id: "revision-2".to_owned(),
        source_sha: "1111111111111111111111111111111111111111".to_owned(),
        target_sha: target.to_owned(),
        policy_hash: "policy".to_owned(),
        check_config_hash: "checks".to_owned(),
        audit_id: "audit-revision-2".to_owned(),
        actor: "codex".to_owned(),
    }
}

#[test]
fn new_revision_is_appended_immutably_and_audited() {
    let mut store = initialized_store();
    store.create_pull_request(&new_pull_request()).unwrap();

    let unchanged = store
        .add_pull_request_revision(
            "repo-1",
            "pr-1",
            &next_revision("0000000000000000000000000000000000000000"),
        )
        .unwrap_err();
    assert_eq!(unchanged.code, ErrorCode::ValidationFailed);

    let record = store
        .add_pull_request_revision(
            "repo-1",
            "pr-1",
            &next_revision("2222222222222222222222222222222222222222"),
        )
        .unwrap();

    assert_eq!(record.status, PullRequestStatus::Draft);
    assert_eq!(record.revisions.len(), 2);
    assert_eq!(
        record.revisions[0].target_sha,
        "0000000000000000000000000000000000000000"
    );
    let events = store.audit_events("repo-1", Some("pr-1")).unwrap();
    assert_eq!(events.last().unwrap().event_type, "pr_revision_created");
}

#[test]
fn revision_is_rejected_while_checking() {
    let mut store = checking_store();

    let error = store
        .add_pull_request_revision(
            "repo-1",
            "pr-1",
            &next_revision("2222222222222222222222222222222222222222"),
        )
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::ValidationFailed);
}
