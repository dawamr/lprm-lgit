use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use lprm_git::Git;
use lprm_store::{RepositoryLock, StateLayout};
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct PreparedPr {
    _temporary: TempDir,
    repository: PathBuf,
    state: PathBuf,
    pr_id: String,
}

#[test]
fn golden_machine_envelopes_are_stable_and_single_line() {
    let fixture = tempdir().unwrap();
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).unwrap();
    initialize_repository(&repository);
    assert_success(&lprm(
        &repository,
        &state,
        &["--json", "init", "--target", "dev"],
    ));

    let success = lprm(&repository, &state, &["--json", "queue", "list"]);
    assert_success(&success);
    assert_eq!(
        success.stdout,
        b"{\"schema_version\":1,\"ok\":true,\"data\":[],\"error\":null}\n"
    );

    let failure = lprm(&repository, &state, &["--json", "pr", "show", "missing"]);
    assert_eq!(failure.status.code(), Some(2));
    assert_eq!(
        failure.stdout,
        b"{\"schema_version\":1,\"ok\":false,\"data\":null,\"error\":{\"code\":\"INVALID_ARGUMENT\",\"message\":\"Unknown pull request ID \\\"missing\\\"\",\"details\":{},\"retryable\":false}}\n"
    );
}

#[test]
fn conflict_exit_four_preserves_a_conflict_state() {
    let prepared = prepared_pull_request(true);
    assert_success(&lprm(
        &prepared.repository,
        &prepared.state,
        &["--json", "pr", "ready", &prepared.pr_id],
    ));

    let conflict = lprm(
        &prepared.repository,
        &prepared.state,
        &["--json", "pr", "check", &prepared.pr_id],
    );

    assert_error(&conflict, 4, "CONFLICT", false);
    let shown = json(&lprm(
        &prepared.repository,
        &prepared.state,
        &["--json", "pr", "show", &prepared.pr_id],
    ));
    assert_eq!(shown["data"]["status"], "conflict");
}

#[test]
fn validation_and_stale_revision_have_distinct_exit_codes() {
    let prepared = prepared_pull_request(false);
    assert_success(&lprm(
        &prepared.repository,
        &prepared.state,
        &["--json", "pr", "ready", &prepared.pr_id],
    ));
    let invalid_transition = lprm(
        &prepared.repository,
        &prepared.state,
        &["--json", "pr", "ready", &prepared.pr_id],
    );
    assert_error(&invalid_transition, 5, "VALIDATION_FAILED", false);

    fs::write(prepared.repository.join("target.txt"), "target moved\n").unwrap();
    git(&prepared.repository, &["add", "target.txt"]);
    git(&prepared.repository, &["commit", "-m", "move target"]);
    let stale = lprm(
        &prepared.repository,
        &prepared.state,
        &["--json", "pr", "check", &prepared.pr_id],
    );
    assert_error(&stale, 6, "STALE_REVISION", true);
}

#[test]
fn busy_and_internal_failures_keep_their_public_exit_codes() {
    let fixture = tempdir().unwrap();
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).unwrap();
    initialize_repository(&repository);
    assert_success(&lprm(
        &repository,
        &state,
        &["--json", "init", "--target", "dev"],
    ));
    let discovered = Git::default().discover(&repository).unwrap();
    let layout = StateLayout::under(&state, &discovered.common_git_dir);
    let _lock = RepositoryLock::acquire(
        layout.lock_path(),
        Duration::from_millis(50),
        "machine-contract-test",
    )
    .unwrap();

    let busy = lprm(&repository, &state, &["--json", "queue", "run", "--once"]);
    assert_error(&busy, 7, "BUSY", true);

    let blocked_state = fixture.path().join("blocked-state");
    fs::write(&blocked_state, "not a directory").unwrap();
    let internal = lprm(
        &repository,
        &blocked_state,
        &["--json", "init", "--target", "dev"],
    );
    assert_error(&internal, 8, "INTERNAL", false);
}

#[test]
fn argument_and_repository_errors_keep_exit_two_and_three() {
    let invalid = Command::new(env!("CARGO_BIN_EXE_lprm"))
        .args(["--json", "pr", "show"])
        .output()
        .unwrap();
    assert_error(&invalid, 2, "INVALID_ARGUMENT", false);
    let outside = tempdir().unwrap();
    let git_precondition = Command::new(env!("CARGO_BIN_EXE_lprm"))
        .current_dir(outside.path())
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert_error(&git_precondition, 3, "GIT_PRECONDITION", false);
}

fn prepared_pull_request(conflict: bool) -> PreparedPr {
    let temporary = tempdir().unwrap();
    let repository = temporary.path().join("repository");
    let state = temporary.path().join("state");
    fs::create_dir(&repository).unwrap();
    initialize_repository(&repository);
    write_check_config(&repository);
    assert_success(&lprm(
        &repository,
        &state,
        &["--json", "init", "--target", "dev"],
    ));
    let worktree = json(&lprm(
        &repository,
        &state,
        &["--json", "worktree", "create", "--task", "contract"],
    ));
    let source_path = PathBuf::from(worktree["data"]["path"].as_str().unwrap());
    let source_branch = worktree["data"]["branch"].as_str().unwrap();
    fs::write(source_path.join("shared.txt"), "source\n").unwrap();
    git(&source_path, &["add", "shared.txt"]);
    git(&source_path, &["commit", "-m", "source"]);
    if conflict {
        fs::write(repository.join("shared.txt"), "target\n").unwrap();
        git(&repository, &["add", "shared.txt"]);
        git(&repository, &["commit", "-m", "conflicting target"]);
    }
    let created = json(&lprm(
        &repository,
        &state,
        &[
            "--json",
            "pr",
            "create",
            "--from",
            source_branch,
            "--title",
            "Contract PR",
        ],
    ));
    PreparedPr {
        _temporary: temporary,
        repository,
        state,
        pr_id: created["data"]["pr_id"].as_str().unwrap().to_owned(),
    }
}

fn write_check_config(repository: &Path) {
    fs::write(
        repository.join(".lprm.toml"),
        "schema_version = 1\n[checks]\ntimeout_seconds = 5\nrequired = [\"pass\"]\n[checks.commands]\npass = [\"/usr/bin/true\"]\n",
    )
    .unwrap();
    git(repository, &["add", ".lprm.toml"]);
    git(repository, &["commit", "-m", "configure checks"]);
}

fn lprm(repository: &Path, state: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lprm"))
        .current_dir(repository)
        .env("LPRM_STATE_HOME", state)
        .args(arguments)
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    assert_success(output);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_error(output: &Output, exit: i32, code: &str, retryable: bool) {
    assert_eq!(output.status.code(), Some(exit));
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["ok"], false);
    assert_eq!(document["data"], Value::Null);
    assert_eq!(document["error"]["code"], code);
    assert_eq!(document["error"]["details"], serde_json::json!({}));
    assert_eq!(document["error"]["retryable"], retryable);
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
