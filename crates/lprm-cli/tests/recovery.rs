use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lprm_cli::coordinator::{MergeRecoveryOptions, recover_merges};
use lprm_core::{AdmissionGates, PullRequestStatus, RevisionFingerprint};
use lprm_git::{Git, GuardedMerge};
use lprm_store::{
    CandidateMergeEvidence, EnqueuePullRequest, NewPullRequest, StartMergeAttempt, StateLayout,
    Store, TransitionEvidence,
};
use tempfile::{TempDir, tempdir};

struct RecoveryFixture {
    _temporary: TempDir,
    repository: PathBuf,
    layout: StateLayout,
    target_sha: String,
    source_sha: String,
}

#[test]
fn recovery_before_merge_requeues_once_without_changing_git() {
    let fixture = recovery_fixture();

    let report = recover(&fixture);

    assert_eq!(report.len(), 1);
    assert_eq!(report[0].classification, "not_integrated");
    assert_eq!(report[0].action, "requeued");
    assert_eq!(recover(&fixture), Vec::new());
    assert_eq!(
        git_stdout(&fixture.repository, &["rev-parse", "dev"]),
        fixture.target_sha
    );
    let store = Store::open(fixture.layout.database_path()).unwrap();
    assert_eq!(
        store
            .list_merge_attempts(fixture.layout.repository_id())
            .unwrap()[0]
            .state,
        "failed"
    );
    assert_eq!(
        store
            .list_merge_queue(fixture.layout.repository_id())
            .unwrap()[0]
            .status,
        "queued"
    );
}

#[test]
fn recovery_during_candidate_work_requeues_once() {
    let fixture = recovery_fixture();
    let (candidate_commit, _) = persist_candidate(&fixture);

    let report = recover(&fixture);

    assert_eq!(report[0].action, "requeued");
    assert_eq!(recover(&fixture), Vec::new());
    let attempt = &Store::open(fixture.layout.database_path())
        .unwrap()
        .list_merge_attempts(fixture.layout.repository_id())
        .unwrap()[0];
    assert_eq!(attempt.state, "failed");
    assert_eq!(
        attempt.candidate_commit_sha.as_deref(),
        Some(candidate_commit.as_str())
    );
}

#[test]
fn recovery_after_git_success_completes_once_without_duplicate_merge() {
    let fixture = recovery_fixture();
    let (_, candidate_tree) = persist_candidate(&fixture);
    let GuardedMerge::Verified(merged) = Git::default()
        .guarded_merge(
            &fixture.repository,
            "dev",
            &fixture.target_sha,
            &fixture.source_sha,
            &candidate_tree,
        )
        .unwrap()
    else {
        panic!("fixture merge should verify");
    };

    let report = recover(&fixture);

    assert_eq!(report[0].classification, "verified");
    assert_eq!(report[0].action, "completed");
    assert_eq!(report[0].observed_target_sha, merged.target_after_sha);
    assert_eq!(recover(&fixture), Vec::new());
    assert_eq!(
        git_stdout(
            &fixture.repository,
            &["rev-list", "--merges", "--count", "dev"]
        ),
        "1"
    );
    let store = Store::open(fixture.layout.database_path()).unwrap();
    assert_eq!(
        store
            .list_merge_attempts(fixture.layout.repository_id())
            .unwrap()[0]
            .state,
        "succeeded"
    );
    assert_eq!(
        store
            .pull_request(fixture.layout.repository_id(), "pr-recovery")
            .unwrap()
            .unwrap()
            .status,
        PullRequestStatus::Merged
    );
}

#[test]
fn recovery_marks_unmatched_target_movement_for_operator_review() {
    let fixture = recovery_fixture();
    persist_candidate(&fixture);
    fs::write(fixture.repository.join("external.txt"), "external\n").unwrap();
    git(&fixture.repository, &["add", "external.txt"]);
    git(
        &fixture.repository,
        &["commit", "-m", "external target movement"],
    );
    let observed = git_stdout(&fixture.repository, &["rev-parse", "dev"]);

    let first = recover(&fixture);
    let second = recover(&fixture);

    assert_eq!(first[0].classification, "needs_operator_review");
    assert_eq!(first[0].action, "marked_for_operator_review");
    assert_eq!(first[0].observed_target_sha, observed);
    assert_eq!(second[0].classification, "needs_operator_review");
    let attempt = &Store::open(fixture.layout.database_path())
        .unwrap()
        .list_merge_attempts(fixture.layout.repository_id())
        .unwrap()[0];
    assert_eq!(attempt.state, "reconciling");
    assert_eq!(attempt.error_code.as_deref(), Some("NEEDS_OPERATOR_REVIEW"));
}

fn recover(fixture: &RecoveryFixture) -> Vec<lprm_cli::coordinator::MergeRecoveryReport> {
    recover_merges(&MergeRecoveryOptions {
        repository: &fixture.repository,
        layout: &fixture.layout,
        dry_run: false,
        lock_timeout: Duration::from_secs(1),
        actor: "test-recovery",
    })
    .expect("recovery should classify active attempts")
}

fn persist_candidate(fixture: &RecoveryFixture) -> (String, String) {
    let candidate_root = fixture.layout.repository_dir().join("recovery-candidates");
    fs::create_dir_all(&candidate_root).unwrap();
    let candidate = Git::default()
        .create_integration_candidate(
            &fixture.repository,
            &candidate_root,
            &fixture.target_sha,
            &fixture.source_sha,
        )
        .unwrap();
    let commit = candidate.commit_sha().to_owned();
    let tree = candidate.tree_sha().to_owned();
    Store::open(fixture.layout.database_path())
        .unwrap()
        .record_candidate_ready(
            fixture.layout.repository_id(),
            &CandidateMergeEvidence {
                attempt_id: "attempt-recovery".to_owned(),
                candidate_commit_sha: commit.clone(),
            },
        )
        .unwrap();
    (commit, tree)
}

fn recovery_fixture() -> RecoveryFixture {
    let temporary = tempdir().unwrap();
    let repository = temporary.path().join("repository");
    fs::create_dir(&repository).unwrap();
    initialize_repository(&repository);
    let target_sha = git_stdout(&repository, &["rev-parse", "dev"]);
    git(&repository, &["checkout", "-b", "feature/recovery"]);
    fs::write(repository.join("source.txt"), "source\n").unwrap();
    git(&repository, &["add", "source.txt"]);
    git(&repository, &["commit", "-m", "source"]);
    let source_sha = git_stdout(&repository, &["rev-parse", "HEAD"]);
    git(&repository, &["checkout", "dev"]);
    let discovered = Git::default().discover(&repository).unwrap();
    let layout = StateLayout::under(&temporary.path().join("state"), &discovered.common_git_dir);
    layout.prepare().unwrap();
    seed_started_attempt(&layout, &target_sha, &source_sha);
    RecoveryFixture {
        _temporary: temporary,
        repository,
        layout,
        target_sha,
        source_sha,
    }
}

fn seed_started_attempt(layout: &StateLayout, target_sha: &str, source_sha: &str) {
    let mut store = Store::open(layout.database_path()).unwrap();
    store
        .register_repository(
            layout.repository_id(),
            Path::new("/tmp/recovery.git"),
            "dev",
            "config",
        )
        .unwrap();
    store
        .register_worktree(
            "worktree-recovery",
            layout.repository_id(),
            "feature/recovery",
            Path::new("/tmp/recovery-worktree"),
            Some("test-agent"),
        )
        .unwrap();
    store
        .create_pull_request(&NewPullRequest {
            id: "pr-recovery".to_owned(),
            repository_id: layout.repository_id().to_owned(),
            worktree_id: Some("worktree-recovery".to_owned()),
            title: "Recovery".to_owned(),
            body: String::new(),
            source_branch: "feature/recovery".to_owned(),
            target_branch: "dev".to_owned(),
            revision_id: "revision-recovery".to_owned(),
            source_sha: source_sha.to_owned(),
            target_sha: target_sha.to_owned(),
            policy_hash: "policy".to_owned(),
            check_config_hash: "checks".to_owned(),
            audit_id: "audit-create-recovery".to_owned(),
            actor: "test-agent".to_owned(),
        })
        .unwrap();
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
                layout.repository_id(),
                "pr-recovery",
                status,
                &TransitionEvidence {
                    audit_id: format!("audit-transition-{index}"),
                    actor: "test-agent".to_owned(),
                    reason: "test progression".to_owned(),
                    revision_id: "revision-recovery".to_owned(),
                    source_sha: source_sha.to_owned(),
                    target_sha: target_sha.to_owned(),
                    policy_hash: "policy".to_owned(),
                },
            )
            .unwrap();
    }
    store
        .enqueue_pull_request(
            layout.repository_id(),
            "pr-recovery",
            &EnqueuePullRequest {
                queue_id: "queue-recovery".to_owned(),
                audit_id: "audit-queue-recovery".to_owned(),
                actor: "test-agent".to_owned(),
                reason: "test admission".to_owned(),
                priority: 0,
                current_fingerprint: RevisionFingerprint {
                    source_sha: source_sha.to_owned(),
                    target_sha: target_sha.to_owned(),
                    policy_hash: "policy".to_owned(),
                    check_config_hash: "checks".to_owned(),
                },
                gates: AdmissionGates {
                    conflict_free: true,
                    required_checks_passed: true,
                    required_reviews_passed: true,
                    required_approvals_present: true,
                },
            },
        )
        .unwrap();
    store
        .start_next_merge_attempt(
            layout.repository_id(),
            &StartMergeAttempt {
                attempt_id: "attempt-recovery".to_owned(),
                audit_id: "audit-attempt-recovery".to_owned(),
                actor: "test-agent".to_owned(),
            },
        )
        .unwrap();
}

fn initialize_repository(repository: &Path) {
    git(repository, &["init", "-b", "dev"]);
    git(repository, &["config", "user.name", "LPRM Test"]);
    git(
        repository,
        &["config", "user.email", "lprm-test@example.invalid"],
    );
    fs::write(repository.join("base.txt"), "base\n").unwrap();
    git(repository, &["add", "base.txt"]);
    git(repository, &["commit", "-m", "base"]);
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Git failed: {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success(), "Git failed: {arguments:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
