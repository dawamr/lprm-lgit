use std::process::Command;

use serde_json::Value;

fn lprm(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lprm"))
        .args(args)
        .output()
        .expect("lprm should start")
}

#[test]
fn json_mode_returns_a_versioned_envelope_for_valid_commands() {
    let output = lprm(&["--json", "doctor"]);
    let document: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert_eq!(document["schema_version"], 1);
    assert!(document["ok"].is_boolean());
    assert!(document.get("data").is_some());
    assert!(document.get("error").is_some());
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
    let output = lprm(&["doctor"]);

    assert!(!output.status.success());
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert_ne!(output.stderr, Vec::<u8>::new());
}
