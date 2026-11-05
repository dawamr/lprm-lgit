use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use lprm_cli::coordinator::{CoordinatorOptions, run_once};
use lprm_core::{AdmissionGates, ChecksConfig, PullRequestStatus, RevisionFingerprint};
use lprm_git::Git;
use lprm_store::{EnqueuePullRequest, NewPullRequest, StateLayout, Store, TransitionEvidence};
use tempfile::tempdir;

#[test]
fn sequential_pull_requests_merge_once_with_exact_parent_chains() {
    let temporary = tempdir().expect("temporary directory should create");
    let repository = temporary.path().join("repository");
    fs::create_dir(&repository).unwrap();
    initialize_repository(&repository);
    let git = Git::default();
    let discovered = git.discover(&repository).unwrap();
    let layout = StateLayout::under(&temporary.path().join("state"), &discovered.common_git_dir);
    layout.prepare().unwrap();
    let mut store = Store::open(layout.database_path()).unwrap();
    store
        .register_repository(
            layout.repository_id(),
            &discovered.common_git_dir,
            "dev",
            "config",
        )
        .unwrap();

    let base_sha = git_stdout(&repository, &["rev-parse", "dev"]);
    let source_one = create_source_commit(&repository, "feature/one", "one.txt", "one");
    seed_queued_pr(
        &mut store,
        layout.repository_id(),
        "pr-1",
        "worktree-1",
        "queue-1",
        &source_one,
        &base_sha,
    );
    drop(store);

    let checks = passing_checks();
    let first = run_once(&CoordinatorOptions {
        repository: &repository,
        layout: &layout,
        checks: &checks,
        lock_timeout: Duration::from_secs(1),
        max_output_bytes: 4096,
        actor: "test-coordinator",
    })
    .expect("first PR should integrate")
    .expect("first queue item should exist");
    assert_eq!(first.attempt.state, "succeeded");
    let first_target = git_stdout(&repository, &["rev-parse", "dev"]);
    assert_parent_chain(&repository, &first_target, &base_sha, &source_one);

    let empty = run_once(&CoordinatorOptions {
        repository: &repository,
        layout: &layout,
        checks: &checks,
        lock_timeout: Duration::from_secs(1),
        max_output_bytes: 4096,
        actor: "test-coordinator",
    })
    .expect("empty queue should be safe");
    assert!(empty.is_none());
    assert_eq!(git_stdout(&repository, &["rev-parse", "dev"]), first_target);

    let source_two = create_source_commit(&repository, "feature/two", "two.txt", "two");
    let mut store = Store::open(layout.database_path()).unwrap();
    seed_queued_pr(
        &mut store,
        layout.repository_id(),
        "pr-2",
        "worktree-2",
        "queue-2",
        &source_two,
        &first_target,
    );
    drop(store);
    let second = run_once(&CoordinatorOptions {
        repository: &repository,
        layout: &layout,
        checks: &checks,
        lock_timeout: Duration::from_secs(1),
        max_output_bytes: 4096,
        actor: "test-coordinator",
    })
    .expect("second PR should integrate")
    .expect("second queue item should exist");
    assert_eq!(second.attempt.state, "succeeded");
    let second_target = git_stdout(&repository, &["rev-parse", "dev"]);
    assert_parent_chain(&repository, &second_target, &first_target, &source_two);

    assert_persisted_completion(&layout, &repository);
}

fn assert_persisted_completion(layout: &StateLayout, repository: &Path) {
    let store = Store::open(layout.database_path()).unwrap();
    let attempts = store.list_merge_attempts(layout.repository_id()).unwrap();
    assert_eq!(attempts.len(), 2);
    assert!(attempts.iter().all(|attempt| attempt.state == "succeeded"));
    assert_eq!(
        store
            .pull_request(layout.repository_id(), "pr-1")
            .unwrap()
            .unwrap()
            .status,
        PullRequestStatus::Merged
    );
    assert_eq!(
        store
            .pull_request(layout.repository_id(), "pr-2")
            .unwrap()
            .unwrap()
            .status,
        PullRequestStatus::Merged
    );
    assert_eq!(
        git_stdout(repository, &["rev-list", "--merges", "--count", "dev"]),
        "2"
    );
}

fn seed_queued_pr(
    store: &mut Store,
    repository_id: &str,
    pr_id: &str,
    worktree_id: &str,
    queue_id: &str,
    source_sha: &str,
    target_sha: &str,
) {
    let revision_id = format!("revision-{pr_id}");
    store
        .register_worktree(
            worktree_id,
            repository_id,
            &format!("feature/{pr_id}"),
            Path::new(&format!("/tmp/{worktree_id}")),
            Some("test-agent"),
        )
        .unwrap();
    store
        .create_pull_request(&NewPullRequest {
            id: pr_id.to_owned(),
            repository_id: repository_id.to_owned(),
            worktree_id: Some(worktree_id.to_owned()),
            title: format!("Merge {pr_id}"),
            body: String::new(),
            source_branch: format!("feature/{pr_id}"),
            target_branch: "dev".to_owned(),
            revision_id: revision_id.clone(),
            source_sha: source_sha.to_owned(),
            target_sha: target_sha.to_owned(),
            policy_hash: "policy".to_owned(),
            check_config_hash: "checks".to_owned(),
            audit_id: format!("audit-create-{pr_id}"),
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
                repository_id,
                pr_id,
                status,
                &TransitionEvidence {
                    audit_id: format!("audit-{pr_id}-{index}"),
                    actor: "test-agent".to_owned(),
                    reason: "test progression".to_owned(),
                    revision_id: revision_id.clone(),
                    source_sha: source_sha.to_owned(),
                    target_sha: target_sha.to_owned(),
                    policy_hash: "policy".to_owned(),
                },
            )
            .unwrap();
    }
    store
        .enqueue_pull_request(
            repository_id,
            pr_id,
            &EnqueuePullRequest {
                queue_id: queue_id.to_owned(),
                audit_id: format!("audit-queue-{pr_id}"),
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
}

fn passing_checks() -> ChecksConfig {
    ChecksConfig {
        timeout_seconds: 2,
        required: vec!["integration".to_owned()],
        commands: BTreeMap::from([("integration".to_owned(), vec!["/usr/bin/true".to_owned()])]),
    }
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

fn create_source_commit(repository: &Path, branch: &str, file: &str, contents: &str) -> String {
    git(repository, &["checkout", "-b", branch]);
    fs::write(repository.join(file), format!("{contents}\n")).unwrap();
    git(repository, &["add", file]);
    git(repository, &["commit", "-m", contents]);
    let source_sha = git_stdout(repository, &["rev-parse", "HEAD"]);
    git(repository, &["checkout", "dev"]);
    source_sha
}

fn assert_parent_chain(repository: &Path, merge: &str, target: &str, source: &str) {
    assert_eq!(
        git_stdout(repository, &["rev-list", "--parents", "-n", "1", merge])
            .split_whitespace()
            .collect::<Vec<_>>(),
        [merge, target, source]
    );
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("Git should start");
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
        .expect("Git should start");
    assert!(output.status.success(), "Git failed: {arguments:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
