use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use lprm_core::{AdmissionGates, ErrorCode, PullRequestStatus, RevisionFingerprint};
use lprm_store::{
    CandidateMergeEvidence, CompleteMergeAttempt, EnqueuePullRequest, MergeQueueRecord,
    NewPullRequest, RepositoryLock, StartMergeAttempt, Store, TransitionEvidence,
};
use serde_json::Value;
use tempfile::tempdir;

fn approved_store() -> Store {
    seed_approved_store(Store::open_in_memory().expect("memory database should migrate"))
}

fn seed_approved_store(mut store: Store) -> Store {
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

fn started_store() -> Store {
    let mut store = approved_store();
    store
        .enqueue_pull_request("repo-1", "pr-1", &admission())
        .expect("approved PR should enqueue");
    store
        .start_next_merge_attempt(
            "repo-1",
            &StartMergeAttempt {
                attempt_id: "attempt-1".to_owned(),
                audit_id: "audit-attempt".to_owned(),
                actor: "coordinator".to_owned(),
            },
        )
        .expect("queued PR should start")
        .expect("queue should contain one PR");
    store
}

fn completion() -> CompleteMergeAttempt {
    CompleteMergeAttempt {
        attempt_id: "attempt-1".to_owned(),
        audit_id: "audit-complete".to_owned(),
        actor: "coordinator".to_owned(),
        target_before_sha: "0000000000000000000000000000000000000000".to_owned(),
        source_sha: "1111111111111111111111111111111111111111".to_owned(),
        candidate_commit_sha: "2222222222222222222222222222222222222222".to_owned(),
        target_after_sha: "3333333333333333333333333333333333333333".to_owned(),
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

#[test]
fn candidate_evidence_advances_attempt_to_integrating() {
    let mut store = started_store();

    let attempt = store
        .record_candidate_ready(
            "repo-1",
            &CandidateMergeEvidence {
                attempt_id: "attempt-1".to_owned(),
                candidate_commit_sha: "2222222222222222222222222222222222222222".to_owned(),
            },
        )
        .expect("candidate evidence should persist");

    assert_eq!(attempt.state, "integrating");
    assert_eq!(
        attempt.candidate_commit_sha.as_deref(),
        Some("2222222222222222222222222222222222222222")
    );
    assert_eq!(attempt.target_after_sha, None);
}

#[test]
fn completion_atomically_finishes_attempt_queue_pr_and_audit() {
    let mut store = started_store();
    store
        .record_candidate_ready(
            "repo-1",
            &CandidateMergeEvidence {
                attempt_id: "attempt-1".to_owned(),
                candidate_commit_sha: "2222222222222222222222222222222222222222".to_owned(),
            },
        )
        .unwrap();

    let completed = store
        .complete_merge_attempt("repo-1", &completion())
        .expect("verified merge should complete");

    assert_eq!(completed.state, "succeeded");
    assert_eq!(
        completed.target_after_sha.as_deref(),
        Some("3333333333333333333333333333333333333333")
    );
    assert!(completed.finished_at.is_some());
    assert_eq!(
        store.list_merge_queue("repo-1").unwrap()[0].status,
        "completed"
    );
    assert_eq!(
        store
            .pull_request("repo-1", "pr-1")
            .unwrap()
            .unwrap()
            .status,
        PullRequestStatus::Merged
    );
    let audit = store.audit_events("repo-1", Some("pr-1")).unwrap();
    assert!(
        audit
            .iter()
            .any(|event| event.event_type == "merge_completed")
    );
}

#[test]
fn completion_rejects_mismatched_sha_evidence_without_mutation() {
    let mismatches = [
        (
            "target_before_sha",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        ("source_sha", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        (
            "candidate_commit_sha",
            "cccccccccccccccccccccccccccccccccccccccc",
        ),
    ];
    for (field, wrong_sha) in mismatches {
        let mut store = started_store();
        store
            .record_candidate_ready(
                "repo-1",
                &CandidateMergeEvidence {
                    attempt_id: "attempt-1".to_owned(),
                    candidate_commit_sha: "2222222222222222222222222222222222222222".to_owned(),
                },
            )
            .unwrap();
        let mut request = completion();
        match field {
            "target_before_sha" => request.target_before_sha = wrong_sha.to_owned(),
            "source_sha" => request.source_sha = wrong_sha.to_owned(),
            "candidate_commit_sha" => request.candidate_commit_sha = wrong_sha.to_owned(),
            _ => unreachable!(),
        }

        let error = store
            .complete_merge_attempt("repo-1", &request)
            .expect_err("mismatched evidence must fail");

        assert_eq!(error.code, ErrorCode::StaleRevision);
        assert_eq!(
            store.list_merge_attempts("repo-1").unwrap()[0].state,
            "integrating"
        );
        assert_eq!(
            store.list_merge_queue("repo-1").unwrap()[0].status,
            "started"
        );
        assert_eq!(
            store
                .pull_request("repo-1", "pr-1")
                .unwrap()
                .unwrap()
                .status,
            PullRequestStatus::Merging
        );
    }
}

#[test]
fn concurrent_coordinators_produce_one_lock_holder_and_one_merge_attempt() {
    let temporary = tempdir().expect("temporary state should create");
    let database_path = temporary.path().join("lprm.db");
    let lock_path = temporary.path().join("merge.lock");
    let marker_path = temporary.path().join("lock-held");
    let mut store = seed_approved_store(Store::open(&database_path).expect("database should open"));
    store
        .enqueue_pull_request("repo-1", "pr-1", &admission())
        .expect("approved PR should enqueue");
    drop(store);

    let mut first = coordinator_process(&database_path, &lock_path, &marker_path, 1_000, 400)
        .spawn()
        .expect("first coordinator should spawn");
    wait_for_file(&marker_path, Duration::from_secs(2));

    let holder: Value = serde_json::from_slice(
        &fs::read(&lock_path).expect("lock holder metadata should be readable"),
    )
    .expect("lock holder metadata should be JSON");
    assert_eq!(holder["pid"], u64::from(first.id()));

    let second = coordinator_process(&database_path, &lock_path, &marker_path, 50, 0)
        .status()
        .expect("second coordinator should run");
    let first_status = first.wait().expect("first coordinator should finish");

    assert!(first_status.success());
    assert_eq!(second.code(), Some(ErrorCode::Busy.exit_code()));
    let store = Store::open(&database_path).expect("database should reopen");
    let attempts = store
        .list_merge_attempts("repo-1")
        .expect("attempts should query");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].state, "preparing");
    assert_eq!(
        store
            .pull_request("repo-1", "pr-1")
            .expect("PR should query")
            .expect("PR should exist")
            .status,
        PullRequestStatus::Merging
    );
}

#[test]
#[ignore = "helper process launched by the concurrent coordinator test"]
fn repository_lock_process_helper() {
    let Some(database_path) = env::var_os("LPRM_TEST_COORDINATOR_DATABASE") else {
        return;
    };
    let lock_path = required_path_env("LPRM_TEST_COORDINATOR_LOCK");
    let marker_path = required_path_env("LPRM_TEST_COORDINATOR_MARKER");
    let timeout = required_u64_env("LPRM_TEST_COORDINATOR_TIMEOUT_MS");
    let hold = required_u64_env("LPRM_TEST_COORDINATOR_HOLD_MS");
    let process_id = std::process::id();
    let _lock =
        match RepositoryLock::acquire(&lock_path, Duration::from_millis(timeout), "queue-run") {
            Ok(lock) => lock,
            Err(error) if error.code == ErrorCode::Busy => {
                std::process::exit(error.exit_code());
            }
            Err(error) => panic!("lock acquisition failed: {error}"),
        };
    fs::write(&marker_path, process_id.to_string()).expect("lock marker should write");
    let mut store = Store::open(Path::new(&database_path)).expect("database should open");
    store
        .start_next_merge_attempt(
            "repo-1",
            &StartMergeAttempt {
                attempt_id: format!("attempt-{process_id}"),
                audit_id: format!("audit-attempt-{process_id}"),
                actor: format!("coordinator-{process_id}"),
            },
        )
        .expect("merge attempt should start");
    thread::sleep(Duration::from_millis(hold));
}

fn coordinator_process(
    database_path: &Path,
    lock_path: &Path,
    marker_path: &Path,
    timeout_ms: u64,
    hold_ms: u64,
) -> Command {
    let mut command = Command::new(env::current_exe().expect("test executable should resolve"));
    command
        .arg("--ignored")
        .arg("--exact")
        .arg("repository_lock_process_helper")
        .arg("--nocapture")
        .env("LPRM_TEST_COORDINATOR_DATABASE", database_path)
        .env("LPRM_TEST_COORDINATOR_LOCK", lock_path)
        .env("LPRM_TEST_COORDINATOR_MARKER", marker_path)
        .env("LPRM_TEST_COORDINATOR_TIMEOUT_MS", timeout_ms.to_string())
        .env("LPRM_TEST_COORDINATOR_HOLD_MS", hold_ms.to_string());
    command
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < timeout,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn required_path_env(name: &str) -> PathBuf {
    env::var_os(name).map_or_else(|| panic!("{name} should be set"), PathBuf::from)
}

fn required_u64_env(name: &str) -> u64 {
    env::var(name)
        .unwrap_or_else(|_| panic!("{name} should be set"))
        .parse()
        .unwrap_or_else(|_| panic!("{name} should be an integer"))
}
