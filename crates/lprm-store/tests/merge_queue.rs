use std::path::Path;

use lprm_core::{AdmissionGates, PullRequestStatus, RevisionFingerprint};
use lprm_store::{EnqueuePullRequest, MergeQueueRecord, NewPullRequest, Store, TransitionEvidence};

fn approved_store() -> Store {
    let mut store = Store::open_in_memory().expect("memory database should migrate");
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
        .create_pull_request(&NewPullRequest {
            id: "pr-1".to_owned(),
            repository_id: "repo-1".to_owned(),
            worktree_id: Some("worktree-1".to_owned()),
            title: "Add A".to_owned(),
            body: String::new(),
            source_branch: "feature/a".to_owned(),
            target_branch: "dev".to_owned(),
            revision_id: "revision-1".to_owned(),
            source_sha: "1111111111111111111111111111111111111111".to_owned(),
            target_sha: "0000000000000000000000000000000000000000".to_owned(),
            policy_hash: "policy".to_owned(),
            check_config_hash: "checks".to_owned(),
            audit_id: "audit-create".to_owned(),
            actor: "codex".to_owned(),
        })
        .expect("pull request should create");
    for (index, status) in [
        PullRequestStatus::Ready,
        PullRequestStatus::Checking,
        PullRequestStatus::Approved,
    ]
    .into_iter()
    .enumerate()
    {
        store
            .transition_pull_request(
                "repo-1",
                "pr-1",
                status,
                &TransitionEvidence {
                    audit_id: format!("audit-transition-{index}"),
                    actor: "codex".to_owned(),
                    reason: "test progression".to_owned(),
                    revision_id: "revision-1".to_owned(),
                    source_sha: "1111111111111111111111111111111111111111".to_owned(),
                    target_sha: "0000000000000000000000000000000000000000".to_owned(),
                    policy_hash: "policy".to_owned(),
                },
            )
            .expect("state should advance");
    }
    store
}

fn admission() -> EnqueuePullRequest {
    EnqueuePullRequest {
        queue_id: "queue-1".to_owned(),
        audit_id: "audit-queue".to_owned(),
        actor: "coordinator".to_owned(),
        reason: "all gates current".to_owned(),
        priority: 5,
        current_fingerprint: RevisionFingerprint {
            source_sha: "1111111111111111111111111111111111111111".to_owned(),
            target_sha: "0000000000000000000000000000000000000000".to_owned(),
            policy_hash: "policy".to_owned(),
            check_config_hash: "checks".to_owned(),
        },
        gates: AdmissionGates {
            conflict_free: true,
            required_checks_passed: true,
            required_reviews_passed: true,
            required_approvals_present: true,
        },
    }
}

#[test]
fn enqueue_atomically_records_queue_status_and_audit() {
    let mut store = approved_store();

    let queued = store
        .enqueue_pull_request("repo-1", "pr-1", &admission())
        .expect("current approved PR should enqueue");

    assert_eq!(queued.id, "queue-1");
    assert_eq!(queued.pr_id, "pr-1");
    assert_eq!(queued.revision_id, "revision-1");
    assert_eq!(queued.status, "queued");
    assert_eq!(
        store
            .pull_request("repo-1", "pr-1")
            .unwrap()
            .unwrap()
            .status,
        PullRequestStatus::Queued
    );
    let events = store.audit_events("repo-1", Some("pr-1")).unwrap();
    assert!(events.iter().any(|event| event.payload["to"] == "queued"));
}

#[test]
fn enqueue_rejects_each_unmet_gate_and_stale_evidence_without_mutation() {
    let mut store = approved_store();
    let gate_cases = [
        AdmissionGates {
            conflict_free: false,
            ..admission().gates
        },
        AdmissionGates {
            required_checks_passed: false,
            ..admission().gates
        },
        AdmissionGates {
            required_reviews_passed: false,
            ..admission().gates
        },
        AdmissionGates {
            required_approvals_present: false,
            ..admission().gates
        },
    ];
    for gates in gate_cases {
        let request = EnqueuePullRequest {
            gates,
            ..admission()
        };
        assert!(
            store
                .enqueue_pull_request("repo-1", "pr-1", &request)
                .is_err()
        );
    }
    let stale = EnqueuePullRequest {
        current_fingerprint: RevisionFingerprint {
            target_sha: "2222222222222222222222222222222222222222".to_owned(),
            ..admission().current_fingerprint
        },
        ..admission()
    };
    assert!(
        store
            .enqueue_pull_request("repo-1", "pr-1", &stale)
            .is_err()
    );

    assert_eq!(
        store.list_merge_queue("repo-1").unwrap(),
        Vec::<MergeQueueRecord>::new()
    );
    assert_eq!(
        store
            .pull_request("repo-1", "pr-1")
            .unwrap()
            .unwrap()
            .status,
        PullRequestStatus::Approved
    );
}
