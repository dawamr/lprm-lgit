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
