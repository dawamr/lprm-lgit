use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

#[test]
fn p0_machine_workflow_reaches_one_verified_merge_without_prompts() {
    let fixture = tempdir().unwrap();
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    fs::create_dir(&repository).unwrap();
    initialize_repository(&repository);
    fs::write(
        repository.join(".lprm.toml"),
        "schema_version = 1\n[checks]\ntimeout_seconds = 5\nrequired = [\"pass\"]\n[checks.commands]\npass = [\"/usr/bin/true\"]\n",
    )
    .unwrap();
    git(&repository, &["add", ".lprm.toml"]);
    git(&repository, &["commit", "-m", "configure checks"]);
    assert_success(&lprm(
        &repository,
        &state,
        &["--json", "init", "--target", "dev"],
    ));
    let created_worktree = json_success(&lprm(
        &repository,
        &state,
        &[
            "--json",
            "--actor",
            "codex",
            "worktree",
            "create",
            "--task",
            "machine flow",
        ],
    ));
    let source_path = created_worktree["data"]["path"].as_str().unwrap();
    let source_branch = created_worktree["data"]["branch"].as_str().unwrap();
    fs::write(Path::new(source_path).join("feature.txt"), "feature\n").unwrap();
    git(Path::new(source_path), &["add", "feature.txt"]);
    git(Path::new(source_path), &["commit", "-m", "feature"]);
    let source_sha = git_stdout(Path::new(source_path), &["rev-parse", "HEAD"]);
    let target_before = git_stdout(&repository, &["rev-parse", "dev"]);
    let created_pr = json_success(&lprm(
        &repository,
        &state,
        &[
            "--json",
            "--actor",
            "codex",
            "pr",
            "create",
            "--from",
            source_branch,
            "--title",
            "Machine flow",
        ],
    ));
    let pr_id = created_pr["data"]["pr_id"].as_str().unwrap();

    let ready = json_success(&lprm(
        &repository,
        &state,
        &["--json", "--actor", "validator", "pr", "ready", pr_id],
    ));
    assert_eq!(ready["data"]["status"], "ready");
    let checked = json_success(&lprm(
        &repository,
        &state,
        &["--json", "--actor", "validator", "pr", "check", pr_id],
    ));
    assert_eq!(checked["data"]["pull_request"]["status"], "approved");
    assert_eq!(checked["data"]["suite"]["passed"], true);
    let queued = json_success(&lprm(
        &repository,
        &state,
        &["--json", "--actor", "coordinator", "pr", "enqueue", pr_id],
    ));
    assert_eq!(queued["data"]["status"], "queued");
    let run = json_success(&lprm(
        &repository,
        &state,
        &["--json", "--actor", "merger", "queue", "run", "--once"],
    ));
    assert_eq!(run["data"]["processed"], 1);
    assert_eq!(run["data"]["attempts"][0]["attempt"]["state"], "succeeded");
    let shown = json_success(&lprm(&repository, &state, &["--json", "pr", "show", pr_id]));
    assert_eq!(shown["data"]["status"], "merged");
    let audit = json_success(&lprm(&repository, &state, &["--json", "audit", pr_id]));
    let actors = audit["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["actor"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        actors,
        [
            "codex",
            "validator",
            "validator",
            "validator",
            "coordinator",
            "merger",
            "merger"
        ]
    );

    let target_after = git_stdout(&repository, &["rev-parse", "dev"]);
    assert_eq!(
        git_stdout(
            &repository,
            &["rev-list", "--parents", "-n", "1", &target_after]
        )
        .split_whitespace()
        .collect::<Vec<_>>(),
        [
            target_after.as_str(),
            target_before.as_str(),
            source_sha.as_str()
        ]
    );
}

fn lprm(repository: &Path, state: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lprm"))
        .current_dir(repository)
        .env("LPRM_STATE_HOME", state)
        .args(arguments)
        .output()
        .unwrap()
}

fn json_success(output: &Output) -> Value {
    assert_success(output);
    serde_json::from_slice(&output.stdout).unwrap()
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
