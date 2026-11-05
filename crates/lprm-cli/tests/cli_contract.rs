use std::path::{Path, PathBuf};
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

    let listed = lprm_in_with_state(&repository, &state, &["--json", "worktree", "list"]);
    let listed_document: Value =
        serde_json::from_slice(&listed.stdout).expect("stdout should be JSON");
    assert!(listed.status.success());
    assert_eq!(
        listed_document["data"]
            .as_array()
            .expect("list data should be an array")
            .len(),
        2
    );
    assert_eq!(listed_document["data"][0]["present"], true);
    assert_eq!(listed_document["data"][0]["dirty"], false);

    let first_id = first_document["data"]["id"]
        .as_str()
        .expect("worktree id should be a string");
    let status = lprm_in_with_state(
        &repository,
        &state,
        &["--json", "worktree", "status", first_id],
    );
    let status_document: Value =
        serde_json::from_slice(&status.stdout).expect("stdout should be JSON");
    assert!(status.status.success());
    assert_eq!(status_document["data"]["id"], first_id);
    assert_eq!(status_document["data"]["head_sha"], target_sha);

    assert_dirty_and_missing_observations(
        &repository,
        &state,
        first_path,
        second_path,
        &first_document["data"]["id"],
        &second_document["data"]["id"],
    );
}

#[test]
fn worktree_cleanup_is_dry_run_by_default_and_refuses_data_loss() {
    let fixture = tempdir().expect("temporary directory should be created");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    std::fs::create_dir(&repository).expect("repository directory should be created");
    initialize_repository(&repository);
    assert!(
        lprm_in_with_state(&repository, &state, &["--json", "init", "--target", "dev"])
            .status
            .success()
    );

    let created = lprm_in_with_state(
        &repository,
        &state,
        &["--json", "worktree", "create", "--task", "Cleanup fixture"],
    );
    let created_document: Value =
        serde_json::from_slice(&created.stdout).expect("stdout should be JSON");
    let id = created_document["data"]["id"]
        .as_str()
        .expect("worktree id should be a string");
    let path = Path::new(
        created_document["data"]["path"]
            .as_str()
            .expect("worktree path should be a string"),
    );

    let preview = lprm_in_with_state(&repository, &state, &["--json", "worktree", "cleanup", id]);
    let preview_document: Value =
        serde_json::from_slice(&preview.stdout).expect("stdout should be JSON");
    assert!(preview.status.success());
    assert_eq!(preview_document["data"]["dry_run"], true);
    assert_eq!(preview_document["data"]["removed"], false);
    assert!(path.exists());

    std::fs::write(path.join("dirty.txt"), "dirty\n").expect("dirty fixture should be written");
    let dirty = lprm_in_with_state(
        &repository,
        &state,
        &["--json", "--yes", "worktree", "cleanup", id],
    );
    let dirty_document: Value =
        serde_json::from_slice(&dirty.stdout).expect("stdout should be JSON");
    assert_eq!(dirty.status.code(), Some(5));
    assert_eq!(dirty_document["error"]["code"], "VALIDATION_FAILED");
    assert!(path.exists());
    std::fs::remove_file(path.join("dirty.txt")).expect("dirty fixture should be removed");

    std::fs::write(path.join("unique.txt"), "unique\n").expect("unique fixture should be written");
    run_git(path, &["add", "unique.txt"]);
    run_git(path, &["commit", "--quiet", "-m", "unique work"]);
    let unique = lprm_in_with_state(
        &repository,
        &state,
        &["--json", "--yes", "worktree", "cleanup", id],
    );
    assert_eq!(unique.status.code(), Some(5));
    assert!(path.exists());

    let clean_created = lprm_in_with_state(
        &repository,
        &state,
        &[
            "--json",
            "worktree",
            "create",
            "--task",
            "Disposable fixture",
        ],
    );
    let clean_document: Value =
        serde_json::from_slice(&clean_created.stdout).expect("stdout should be JSON");
    let clean_id = clean_document["data"]["id"]
        .as_str()
        .expect("worktree id should be a string");
    let clean_path = Path::new(
        clean_document["data"]["path"]
            .as_str()
            .expect("worktree path should be a string"),
    );
    let removed = lprm_in_with_state(
        &repository,
        &state,
        &["--json", "--yes", "worktree", "cleanup", clean_id],
    );
    let removed_document: Value =
        serde_json::from_slice(&removed.stdout).expect("stdout should be JSON");
    assert!(removed.status.success());
    assert_eq!(removed_document["data"]["removed"], true);
    assert!(!clean_path.exists());
}

#[test]
fn recover_reports_database_only_and_git_only_worktrees_without_mutation() {
    let fixture = tempdir().expect("temporary directory should be created");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    std::fs::create_dir(&repository).expect("repository directory should be created");
    initialize_repository(&repository);
    assert!(
        lprm_in_with_state(&repository, &state, &["--json", "init", "--target", "dev"])
            .status
            .success()
    );
    let created = lprm_in_with_state(
        &repository,
        &state,
        &[
            "--json",
            "worktree",
            "create",
            "--task",
            "Missing registration fixture",
        ],
    );
    let created_document: Value =
        serde_json::from_slice(&created.stdout).expect("stdout should be JSON");
    let missing_path = created_document["data"]["path"]
        .as_str()
        .expect("worktree path should be a string");
    run_git(
        &repository,
        &["worktree", "remove", "--force", missing_path],
    );

    let git_only_path = fixture.path().join(".lprm-worktrees/manual-orphan");
    run_git(
        &repository,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "manual/orphan",
            git_only_path
                .to_str()
                .expect("fixture path should be UTF-8"),
            "dev",
        ],
    );

    let recovered = lprm_in_with_state(&repository, &state, &["--json", "recover", "--dry-run"]);
    let recovered_document: Value =
        serde_json::from_slice(&recovered.stdout).expect("stdout should be JSON");
    assert!(recovered.status.success());
    let orphans = recovered_document["data"]["worktree_orphans"]
        .as_array()
        .expect("orphans should be an array");
    assert!(
        orphans
            .iter()
            .any(|orphan| orphan["kind"] == "database_only")
    );
    assert!(orphans.iter().any(|orphan| orphan["kind"] == "git_only"));
    assert!(git_only_path.exists());
}

#[test]
fn pull_request_create_rejects_empty_dirty_and_missing_sources() {
    let fixture = tempdir().expect("temporary directory should be created");
    let (repository, state, source_path, source_branch) = setup_pr_fixture(fixture.path());

    let empty = lprm_in_with_state(
        &repository,
        &state,
        &[
            "--json",
            "pr",
            "create",
            "--from",
            &source_branch,
            "--title",
            "Empty",
        ],
    );
    assert_eq!(empty.status.code(), Some(5));

    std::fs::write(source_path.join("feature.txt"), "uncommitted\n")
        .expect("dirty fixture should be written");
    let dirty = lprm_in_with_state(
        &repository,
        &state,
        &[
            "--json",
            "pr",
            "create",
            "--from",
            &source_branch,
            "--title",
            "Dirty",
        ],
    );
    assert_eq!(dirty.status.code(), Some(5));

    let missing = lprm_in_with_state(
        &repository,
        &state,
        &[
            "--json",
            "pr",
            "create",
            "--from",
            "missing/branch",
            "--title",
            "Missing",
        ],
    );
    assert_eq!(missing.status.code(), Some(3));
}

#[test]
fn pull_request_lifecycle_persists_and_uses_the_exact_revision() {
    let fixture = tempdir().expect("temporary directory should be created");
    let (repository, state, source_path, source_branch) = setup_pr_fixture(fixture.path());
    std::fs::write(source_path.join("feature.txt"), "committed\n")
        .expect("feature fixture should be written");
    run_git(&source_path, &["add", "--", "feature.txt"]);
    run_git(&source_path, &["commit", "--quiet", "-m", "feature"]);
    let source_sha = git_stdout(&source_path, &["rev-parse", "HEAD"]);
    let target_sha = git_stdout(&repository, &["rev-parse", "dev"]);
    let body_path = fixture.path().join("body.md");
    std::fs::write(&body_path, "Lifecycle body\n").expect("body should be written");

    let created = lprm_in_with_state(
        &repository,
        &state,
        &[
            "--json",
            "pr",
            "create",
            "--from",
            &source_branch,
            "--title",
            "Add feature",
            "--body-file",
            body_path.to_str().expect("body path should be UTF-8"),
        ],
    );
    let created_document: Value =
        serde_json::from_slice(&created.stdout).expect("stdout should be JSON");
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let pr_id = created_document["data"]["pr_id"]
        .as_str()
        .expect("PR ID should be a string");
    assert_eq!(created_document["data"]["status"], "draft");
    assert_eq!(created_document["data"]["body"], "Lifecycle body\n");
    assert_eq!(
        created_document["data"]["revisions"][0]["source_sha"],
        source_sha
    );
    assert_eq!(
        created_document["data"]["revisions"][0]["target_sha"],
        target_sha
    );

    let listed = lprm_in_with_state(
        &repository,
        &state,
        &["--json", "pr", "list", "--status", "draft"],
    );
    let listed_document: Value =
        serde_json::from_slice(&listed.stdout).expect("stdout should be JSON");
    assert_eq!(listed_document["data"].as_array().map(Vec::len), Some(1));
    assert_eq!(listed_document["data"][0]["pr_id"], pr_id);

    let shown = lprm_in_with_state(&repository, &state, &["--json", "pr", "show", pr_id]);
    let shown_document: Value =
        serde_json::from_slice(&shown.stdout).expect("stdout should be JSON");
    assert_eq!(shown_document["data"]["pr_id"], pr_id);

    let diff = lprm_in_with_state(&repository, &state, &["--json", "pr", "diff", pr_id]);
    let diff_document: Value = serde_json::from_slice(&diff.stdout).expect("stdout should be JSON");
    assert_eq!(diff_document["data"]["source_sha"], source_sha);
    assert_eq!(diff_document["data"]["target_sha"], target_sha);
    assert_eq!(diff_document["data"]["files"][0]["path"], "feature.txt");
    assert!(diff_document["data"]["patch"].as_str().is_some());

    let stat = lprm_in_with_state(
        &repository,
        &state,
        &["--json", "pr", "diff", pr_id, "--stat"],
    );
    let stat_document: Value = serde_json::from_slice(&stat.stdout).expect("stdout should be JSON");
    assert_eq!(stat_document["data"]["files"][0]["path"], "feature.txt");
    assert_eq!(stat_document["data"]["patch"], Value::Null);

    assert_close_and_audit(&repository, &state, pr_id, &source_sha);
}

fn assert_close_and_audit(repository: &Path, state: &Path, pr_id: &str, source_sha: &str) {
    let closed = lprm_in_with_state(
        repository,
        state,
        &[
            "--json",
            "pr",
            "close",
            pr_id,
            "--reason",
            "superseded locally",
        ],
    );
    let closed_document: Value =
        serde_json::from_slice(&closed.stdout).expect("stdout should be JSON");
    assert_eq!(closed_document["data"]["status"], "closed");
    assert_eq!(
        closed_document["data"]["revisions"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let audit = lprm_in_with_state(repository, state, &["--json", "audit", pr_id]);
    let document: Value = serde_json::from_slice(&audit.stdout).expect("stdout should be JSON");
    assert_eq!(document["data"].as_array().map(Vec::len), Some(2));
    assert_eq!(document["data"][0]["event_type"], "pr_created");
    assert_eq!(document["data"][1]["event_type"], "pr_status_transition");
    assert_eq!(
        document["data"][1]["payload"]["reason"],
        "superseded locally"
    );
    assert_eq!(document["data"][1]["payload"]["source_sha"], source_sha);
}

fn setup_pr_fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf, String) {
    let repository = root.join("repository");
    let state = root.join("state");
    std::fs::create_dir(&repository).expect("repository directory should be created");
    initialize_repository(&repository);
    assert!(
        lprm_in_with_state(&repository, &state, &["--json", "init", "--target", "dev"])
            .status
            .success()
    );
    let created = lprm_in_with_state(
        &repository,
        &state,
        &["--json", "worktree", "create", "--task", "PR lifecycle"],
    );
    let document: Value = serde_json::from_slice(&created.stdout).expect("stdout should be JSON");
    let source_path = PathBuf::from(
        document["data"]["path"]
            .as_str()
            .expect("worktree path should be a string"),
    );
    let source_branch = document["data"]["branch"]
        .as_str()
        .expect("branch should be a string")
        .to_owned();
    (repository, state, source_path, source_branch)
}

fn assert_dirty_and_missing_observations(
    repository: &Path,
    state: &Path,
    dirty_path: &Path,
    missing_path: &Path,
    dirty_id: &Value,
    missing_id: &Value,
) {
    std::fs::write(dirty_path.join("untracked.txt"), "dirty\n")
        .expect("dirty fixture should be written");
    run_git(
        repository,
        &[
            "worktree",
            "remove",
            "--force",
            missing_path.to_str().expect("fixture path should be UTF-8"),
        ],
    );
    let observed = lprm_in_with_state(repository, state, &["--json", "worktree", "list"]);
    let observed_document: Value =
        serde_json::from_slice(&observed.stdout).expect("stdout should be JSON");
    let observed_items = observed_document["data"]
        .as_array()
        .expect("list data should be an array");
    let dirty = observed_items
        .iter()
        .find(|item| &item["id"] == dirty_id)
        .expect("dirty worktree should be listed");
    let missing = observed_items
        .iter()
        .find(|item| &item["id"] == missing_id)
        .expect("missing worktree should be listed");
    assert_eq!(dirty["dirty"], true);
    assert_eq!(missing["present"], false);
    assert_eq!(missing["dirty"], Value::Null);
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
