use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::tempdir;

fn lprm(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lprm"))
        .args(args)
        .output()
        .expect("lprm should start")
}

fn lprm_in(directory: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lprm"))
        .current_dir(directory)
        .args(args)
        .output()
        .expect("lprm should start")
}

fn lprm_in_with_state(directory: &Path, state: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lprm"))
        .current_dir(directory)
        .env("LPRM_STATE_HOME", state)
        .args(args)
        .output()
        .expect("lprm should start")
}

fn run_git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(directory)
        .args(args)
        .status()
        .expect("git should start");
    assert!(status.success(), "git command failed: {args:?}");
}

fn initialize_repository(directory: &Path) {
    run_git(directory, &["init", "--quiet", "--initial-branch=dev"]);
    run_git(directory, &["config", "user.name", "LPRM Test"]);
    run_git(directory, &["config", "user.email", "lprm@example.invalid"]);
    std::fs::write(directory.join("README.md"), "fixture\n").expect("fixture should be written");
    run_git(directory, &["add", "README.md"]);
    run_git(directory, &["commit", "--quiet", "-m", "fixture"]);
}

#[test]
fn json_mode_returns_a_versioned_envelope_for_valid_commands() {
    let repository = tempdir().expect("temporary directory should be created");
    let git = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repository.path())
        .status()
        .expect("git should start");
    assert!(git.success());

    let output = lprm_in(repository.path(), &["--json", "doctor"]);
    let document: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert!(output.status.success());
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["ok"], true);
    assert_eq!(document["data"]["repository"]["has_submodules"], false);
    assert_eq!(document["error"], Value::Null);
}

#[test]
fn doctor_reports_a_git_precondition_outside_a_repository() {
    let directory = tempdir().expect("temporary directory should be created");
    let output = lprm_in(directory.path(), &["--json", "doctor"]);
    let document: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(document["error"]["code"], "GIT_PRECONDITION");
}

#[test]
fn config_show_returns_validated_defaults() {
    let repository = tempdir().expect("temporary directory should be created");
    let git = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repository.path())
        .status()
        .expect("git should start");
    assert!(git.success());

    let output = lprm_in(repository.path(), &["--json", "config", "show"]);
    let document: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert!(output.status.success());
    assert_eq!(document["data"]["schema_version"], 1);
    assert_eq!(document["data"]["repository"]["target_branch"], "dev");
    assert_eq!(document["data"]["merge"]["strategy"], "no-ff");
}

#[test]
fn config_show_rejects_an_unknown_schema() {
    let repository = tempdir().expect("temporary directory should be created");
    let git = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repository.path())
        .status()
        .expect("git should start");
    assert!(git.success());
    std::fs::write(
        repository.path().join(".lprm.toml"),
        "schema_version = 99\n",
    )
    .expect("config fixture should be written");

    let output = lprm_in(repository.path(), &["--json", "config", "show"]);
    let document: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(document["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn init_persists_external_state_and_is_idempotent_without_a_remote() {
    let fixture = tempdir().expect("temporary directory should be created");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    std::fs::create_dir(&repository).expect("repository directory should be created");
    initialize_repository(&repository);

    let first = lprm_in_with_state(&repository, &state, &["--json", "init", "--target", "dev"]);
    let first_document: Value =
        serde_json::from_slice(&first.stdout).expect("stdout should be JSON");
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(first_document["data"]["target_branch"], "dev");
    assert!(
        first_document["data"]["target_sha"]
            .as_str()
            .is_some_and(|sha| sha.len() >= 40)
    );

    let database_path = Path::new(
        first_document["data"]["database_path"]
            .as_str()
            .expect("database path should be a string"),
    );
    assert!(database_path.starts_with(&state));
    assert!(database_path.exists());
    assert!(!repository.join(".lprm.toml").exists());

    let second = lprm_in_with_state(&repository, &state, &["--json", "init", "--target", "dev"]);
    let second_document: Value =
        serde_json::from_slice(&second.stdout).expect("stdout should be JSON");
    assert!(second.status.success());
    assert_eq!(
        first_document["data"]["repository_id"],
        second_document["data"]["repository_id"]
    );
}

#[test]
fn init_rejects_a_missing_target_branch() {
    let fixture = tempdir().expect("temporary directory should be created");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    std::fs::create_dir(&repository).expect("repository directory should be created");
    initialize_repository(&repository);

    let output = lprm_in_with_state(
        &repository,
        &state,
        &["--json", "init", "--target", "missing"],
    );
    let document: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(document["error"]["code"], "GIT_PRECONDITION");
    assert!(!state.exists());
}

#[test]
fn worktree_create_builds_distinct_checkouts_from_the_exact_target() {
    let fixture = tempdir().expect("temporary directory should be created");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    std::fs::create_dir(&repository).expect("repository directory should be created");
    initialize_repository(&repository);
    let target_sha = git_stdout(&repository, &["rev-parse", "dev"]);

    let initialized =
        lprm_in_with_state(&repository, &state, &["--json", "init", "--target", "dev"]);
    assert!(initialized.status.success());

    let first = lprm_in_with_state(
        &repository,
        &state,
        &[
            "--json",
            "worktree",
            "create",
            "--task",
            "Add User Roles",
            "--agent",
            "codex",
        ],
    );
    let first_document: Value =
        serde_json::from_slice(&first.stdout).expect("stdout should be JSON");
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let second = lprm_in_with_state(
        &repository,
        &state,
        &[
            "--json",
            "worktree",
            "create",
            "--task",
            "Add Audit Search",
            "--agent",
            "codex",
        ],
    );
    let second_document: Value =
        serde_json::from_slice(&second.stdout).expect("stdout should be JSON");
    assert!(second.status.success());

    let first_path = Path::new(
        first_document["data"]["path"]
            .as_str()
            .expect("worktree path should be a string"),
    );
    let second_path = Path::new(
        second_document["data"]["path"]
            .as_str()
            .expect("worktree path should be a string"),
    );
    assert_ne!(first_path, second_path);
    assert_ne!(
        first_document["data"]["branch"],
        second_document["data"]["branch"]
    );
    assert_eq!(git_stdout(first_path, &["rev-parse", "HEAD"]), target_sha);
    assert_eq!(git_stdout(second_path, &["rev-parse", "HEAD"]), target_sha);
    assert_eq!(git_stdout(&repository, &["rev-parse", "dev"]), target_sha);
    assert_eq!(git_stdout(&repository, &["status", "--porcelain"]), "");
}

fn git_stdout(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .expect("git should start");
    assert!(output.status.success(), "git command failed: {args:?}");
    String::from_utf8(output.stdout)
        .expect("git output should be UTF-8")
        .trim()
        .to_owned()
}

#[test]
fn json_mode_maps_argument_errors_to_exit_code_two() {
    let output = lprm(&["--json", "worktree", "status"]);
    let document: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(document["ok"], false);
    assert_eq!(document["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(document["error"]["retryable"], false);
}

#[test]
fn human_mode_keeps_machine_stdout_empty_for_errors() {
    let directory = tempdir().expect("temporary directory should be created");
    let output = lprm_in(directory.path(), &["doctor"]);

    assert!(!output.status.success());
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert_ne!(output.stderr, Vec::<u8>::new());
}
