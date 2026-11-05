use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use lprm_core::ChecksConfig;
use lprm_git::Git;
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
