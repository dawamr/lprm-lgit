use std::fs;
use std::time::{Duration, Instant};

use lprm_runner::{CheckRunner, CheckStatus};
use tempfile::tempdir;

fn command(parts: &[&str]) -> Vec<String> {
    parts.iter().map(ToString::to_string).collect()
}

#[test]
fn captures_success_and_failure_with_stable_statuses() {
    let fixture = tempdir().expect("temporary directory should be created");
    let runner = CheckRunner::new(1024).expect("runner should configure");

    let passed = runner
        .run(
            "passed",
            &command(&["/bin/sh", "-c", "printf success; printf warning >&2"]),
            fixture.path(),
            &fixture.path().join("logs"),
            Duration::from_secs(2),
        )
        .expect("passing check should run");
    let failed = runner
        .run(
            "failed",
            &command(&["/bin/sh", "-c", "exit 7"]),
            fixture.path(),
            &fixture.path().join("logs"),
            Duration::from_secs(2),
        )
        .expect("failing check should run");

    assert_eq!(passed.status, CheckStatus::Passed);
    assert_eq!(passed.exit_code, Some(0));
    assert_eq!(fs::read_to_string(passed.stdout_path).unwrap(), "success");
    assert_eq!(fs::read_to_string(passed.stderr_path).unwrap(), "warning");
    assert_eq!(failed.status, CheckStatus::Failed);
    assert_eq!(failed.exit_code, Some(7));
}

#[test]
fn times_out_and_reports_a_missing_executable() {
    let fixture = tempdir().expect("temporary directory should be created");
    let runner = CheckRunner::new(1024).expect("runner should configure");

    let timed_out = runner
        .run(
            "timeout",
            &command(&["/bin/sleep", "2"]),
            fixture.path(),
            &fixture.path().join("logs"),
            Duration::from_millis(25),
        )
        .expect("timed out check should return a result");
    let missing = runner
        .run(
            "missing",
            &command(&["/definitely/missing/lprm-check"]),
            fixture.path(),
            &fixture.path().join("logs"),
            Duration::from_secs(1),
        )
        .expect("missing executable should return a result");

    assert_eq!(timed_out.status, CheckStatus::TimedOut);
    assert_eq!(missing.status, CheckStatus::MissingExecutable);
    assert_eq!(missing.exit_code, None);
}

#[test]
fn timeout_terminates_descendants_that_keep_output_pipes_open() {
    let fixture = tempdir().expect("temporary directory should be created");
    let runner = CheckRunner::new(1024).expect("runner should configure");
    let started = Instant::now();

    let result = runner
        .run(
            "descendant-timeout",
            &command(&["/bin/sh", "-c", "sleep 2 & wait"]),
            fixture.path(),
            &fixture.path().join("logs"),
            Duration::from_millis(25),
        )
        .expect("timed out process tree should return a result");

    assert_eq!(result.status, CheckStatus::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn bounds_logs_and_passes_hostile_arguments_without_shell_evaluation() {
    let fixture = tempdir().expect("temporary directory should be created");
    let runner = CheckRunner::new(16).expect("runner should configure");
    let sentinel = fixture.path().join("must-not-exist");
    let hostile = format!("$(touch {})", sentinel.display());
    let oversized = "x".repeat(128);

    let literal = runner
        .run(
            "hostile/../name",
            &["/usr/bin/printf".to_owned(), "%s".to_owned(), hostile],
            fixture.path(),
            &fixture.path().join("logs"),
            Duration::from_secs(1),
        )
        .expect("literal argument check should run");
    let bounded = runner
        .run(
            "bounded",
            &["/usr/bin/printf".to_owned(), "%s".to_owned(), oversized],
            fixture.path(),
            &fixture.path().join("logs"),
            Duration::from_secs(1),
        )
        .expect("bounded check should run");

    assert_eq!(literal.status, CheckStatus::Passed);
    assert!(!sentinel.exists());
    assert!(literal.stdout_path.starts_with(fixture.path().join("logs")));
    assert!(bounded.stdout_truncated);
    assert_eq!(fs::metadata(bounded.stdout_path).unwrap().len(), 16);
}
