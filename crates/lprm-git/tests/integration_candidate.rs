use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use lprm_core::{ChecksConfig, ErrorCode};
use lprm_git::{Git, GuardedMerge};
use lprm_runner::{CheckRunner, CheckStatus};
use tempfile::{TempDir, tempdir};

struct Fixture {
    temporary: TempDir,
    repository: PathBuf,
    target_sha: String,
    source_sha: String,
    checks: ChecksConfig,
}

#[test]
fn candidate_merge_uses_exact_commits_without_mutating_target() {
    let fixture = integration_fixture();
    let candidate_root = fixture.temporary.path().join("candidates");
    fs::create_dir(&candidate_root).unwrap();
    let candidate = Git::default()
        .create_integration_candidate(
            &fixture.repository,
            &candidate_root,
            &fixture.target_sha,
            &fixture.source_sha,
        )
        .expect("exact commits should produce a candidate");

    let parents = git_stdout(
        candidate.path(),
        &["rev-list", "--parents", "-n", "1", candidate.commit_sha()],
    );
    assert_eq!(
        parents.split_whitespace().collect::<Vec<_>>(),
        [
            candidate.commit_sha(),
            fixture.target_sha.as_str(),
            fixture.source_sha.as_str(),
        ]
    );
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(candidate.path())
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        git_stdout(&fixture.repository, &["rev-parse", "refs/heads/dev"]),
        fixture.target_sha
    );
}

#[test]
fn combined_required_check_failure_is_observed_and_candidate_is_cleaned() {
    let fixture = integration_fixture();
    let runner = CheckRunner::new(4096).unwrap();
    let source_suite = runner
        .run_required(
            &fixture.checks,
            &fixture.repository,
            &fixture.temporary.path().join("logs/source"),
        )
        .expect("source check should run before switching branches");
    assert!(source_suite.passed);

    git(&fixture.repository, &["checkout", "dev"]);
    let target_suite = runner
        .run_required(
            &fixture.checks,
            &fixture.repository,
            &fixture.temporary.path().join("logs/target"),
        )
        .expect("target check should run");
    assert!(target_suite.passed);
    let candidate_root = fixture.temporary.path().join("candidates");
    fs::create_dir(&candidate_root).unwrap();

    let candidate_path;
    {
        let candidate = Git::default()
            .create_integration_candidate(
                &fixture.repository,
                &candidate_root,
                &fixture.target_sha,
                &fixture.source_sha,
            )
            .expect("exact commits should produce a candidate");
        candidate_path = candidate.path().to_owned();
        let combined_suite = runner
            .run_required(
                &fixture.checks,
                candidate.path(),
                &fixture.temporary.path().join("logs/combined"),
            )
            .expect("combined check should return a verdict");

        assert!(!combined_suite.passed);
        assert_eq!(combined_suite.checks.len(), 1);
        assert_eq!(combined_suite.checks[0].result.status, CheckStatus::Failed);
    }

    assert!(!candidate_path.exists());
    assert!(
        Git::default()
            .list_worktrees(&fixture.repository)
            .unwrap()
            .iter()
            .all(|worktree| worktree.path != candidate_path)
    );
    assert_eq!(
        git_stdout(&fixture.repository, &["rev-parse", "refs/heads/dev"]),
        fixture.target_sha
    );
}

#[test]
fn guarded_merge_matches_candidate_tree_and_exact_parents() {
    let fixture = integration_fixture();
    let candidate = create_candidate(&fixture);
    git(&fixture.repository, &["checkout", "dev"]);

    let outcome = Git::default()
        .guarded_merge(
            &fixture.repository,
            "dev",
            &fixture.target_sha,
            &fixture.source_sha,
            candidate.tree_sha(),
        )
        .expect("guarded merge should run");
    let GuardedMerge::Verified(merged) = outcome else {
        panic!("matching merge should verify");
    };

    assert_eq!(
        git_stdout(&fixture.repository, &["rev-parse", "refs/heads/dev"]),
        merged.target_after_sha
    );
    assert_eq!(merged.tree_sha, candidate.tree_sha());
    assert_eq!(
        git_stdout(
            &fixture.repository,
            &["rev-list", "--parents", "-n", "1", &merged.target_after_sha]
        )
        .split_whitespace()
        .collect::<Vec<_>>(),
        [
            merged.target_after_sha.as_str(),
            fixture.target_sha.as_str(),
            fixture.source_sha.as_str(),
        ]
    );
}

#[test]
fn guarded_merge_rejects_target_race_before_source_integration() {
    let fixture = integration_fixture();
    let candidate = create_candidate(&fixture);
    git(&fixture.repository, &["checkout", "dev"]);
    fs::write(fixture.repository.join("race.flag"), "race\n").unwrap();
    git(&fixture.repository, &["add", "race.flag"]);
    git(&fixture.repository, &["commit", "-m", "external race"]);
    let raced_sha = git_stdout(&fixture.repository, &["rev-parse", "HEAD"]);

    let error = Git::default()
        .guarded_merge(
            &fixture.repository,
            "dev",
            &fixture.target_sha,
            &fixture.source_sha,
            candidate.tree_sha(),
        )
        .expect_err("changed target must be stale");

    assert_eq!(error.code, ErrorCode::StaleRevision);
    assert_eq!(
        git_stdout(&fixture.repository, &["rev-parse", "refs/heads/dev"]),
        raced_sha
    );
    assert!(!git_status(
        &fixture.repository,
        &["merge-base", "--is-ancestor", &fixture.source_sha, "HEAD"]
    ));
}

#[test]
fn guarded_merge_reports_hook_tree_difference() {
    let fixture = integration_fixture();
    let candidate = create_candidate(&fixture);
    git(&fixture.repository, &["checkout", "dev"]);
    install_hook(
        &fixture.repository,
        "post-merge",
        "#!/bin/sh\nprintf 'hook changed tree\\n' > hook.flag\ngit add hook.flag\nold=$(git rev-parse HEAD)\ntree=$(git write-tree)\nnew=$(git commit-tree \"$tree\" -p \"$old^1\" -p \"$old^2\" -m 'hook merge')\ngit update-ref HEAD \"$new\" \"$old\"\n",
    );

    let outcome = Git::default()
        .guarded_merge(
            &fixture.repository,
            "dev",
            &fixture.target_sha,
            &fixture.source_sha,
            candidate.tree_sha(),
        )
        .expect("merge should finish with divergent evidence");
    let GuardedMerge::Diverged(merged) = outcome else {
        panic!("hook-mutated merge must diverge");
    };

    assert_ne!(merged.tree_sha, candidate.tree_sha());
    assert_ne!(merged.target_after_sha, fixture.target_sha);
    assert!(fixture.repository.join("hook.flag").is_file());
}

#[test]
fn failed_guarded_merge_aborts_without_target_mutation() {
    let fixture = integration_fixture();
    let candidate = create_candidate(&fixture);
    git(&fixture.repository, &["checkout", "dev"]);
    install_hook(
        &fixture.repository,
        "pre-merge-commit",
        "#!/bin/sh\nexit 42\n",
    );

    let error = Git::default()
        .guarded_merge(
            &fixture.repository,
            "dev",
            &fixture.target_sha,
            &fixture.source_sha,
            candidate.tree_sha(),
        )
        .expect_err("rejected merge must fail");

    assert_eq!(error.code, ErrorCode::ValidationFailed);
    assert_eq!(
        git_stdout(&fixture.repository, &["rev-parse", "refs/heads/dev"]),
        fixture.target_sha
    );
    assert_eq!(
        git_stdout(&fixture.repository, &["status", "--porcelain"]),
        ""
    );
    assert!(!git_status(
        &fixture.repository,
        &["rev-parse", "--verify", "MERGE_HEAD"]
    ));
}

fn integration_fixture() -> Fixture {
    let temporary = tempdir().expect("temporary directory should create");
    let repository = temporary.path().join("repository");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-b", "dev"]);
    git(&repository, &["config", "user.name", "LPRM Test"]);
    git(
        &repository,
        &["config", "user.email", "lprm-test@example.invalid"],
    );
    let check_path = repository.join("check-combination");
    fs::write(
        &check_path,
        "#!/bin/sh\nif [ -f source.flag ] && [ -f target.flag ]; then exit 23; fi\n",
    )
    .unwrap();
    fs::set_permissions(&check_path, fs::Permissions::from_mode(0o755)).unwrap();
    git(&repository, &["add", "check-combination"]);
    git(&repository, &["commit", "-m", "base"]);

    git(&repository, &["checkout", "-b", "feature/source"]);
    fs::write(repository.join("source.flag"), "source\n").unwrap();
    git(&repository, &["add", "source.flag"]);
    git(&repository, &["commit", "-m", "source"]);
    let source_sha = git_stdout(&repository, &["rev-parse", "HEAD"]);

    git(&repository, &["checkout", "dev"]);
    fs::write(repository.join("target.flag"), "target\n").unwrap();
    git(&repository, &["add", "target.flag"]);
    git(&repository, &["commit", "-m", "target"]);
    let target_sha = git_stdout(&repository, &["rev-parse", "HEAD"]);
    git(&repository, &["checkout", "feature/source"]);

    Fixture {
        temporary,
        repository,
        target_sha,
        source_sha,
        checks: ChecksConfig {
            timeout_seconds: 2,
            required: vec!["joint".to_owned()],
            commands: BTreeMap::from([(
                "joint".to_owned(),
                vec!["./check-combination".to_owned()],
            )]),
        },
    }
}

fn create_candidate(fixture: &Fixture) -> lprm_git::IntegrationCandidate {
    let candidate_root = fixture.temporary.path().join("guarded-candidates");
    fs::create_dir_all(&candidate_root).unwrap();
    Git::default()
        .create_integration_candidate(
            &fixture.repository,
            &candidate_root,
            &fixture.target_sha,
            &fixture.source_sha,
        )
        .expect("exact commits should produce a candidate")
}

fn install_hook(repository: &Path, name: &str, contents: &str) {
    let path = repository.join(".git/hooks").join(name);
    fs::write(&path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
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

fn git_status(repository: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("Git should start")
        .status
        .success()
}
