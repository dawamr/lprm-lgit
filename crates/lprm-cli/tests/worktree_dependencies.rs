//! Worktree dependency preparation: opt-in symlinks, copy-on-write clones, and setup commands.

mod common;

use std::fs;
use std::path::Path;

use common::{Fixture, git};

fn seed(root: &Path, directory: &str, entry: &str) {
    fs::create_dir_all(root.join(directory).join(entry)).unwrap();
    fs::write(root.join(directory).join(entry).join("index.js"), "x\n").unwrap();
}

#[test]
fn present_dependency_directories_are_linked_and_missing_ones_ignored() {
    let fixture = Fixture::with_options(
        "[\"/usr/bin/true\"]",
        "[worktree]\nlink_dependencies = [\"vendors\", \"node_modules\"]\n",
    );
    fs::write(
        fixture.repository.join(".gitignore"),
        "node_modules/\nvendors/\n.lprm.toml\n",
    )
    .unwrap();
    git(&fixture.repository, &["add", ".gitignore"]);
    git(&fixture.repository, &["commit", "-m", "ignore deps"]);
    seed(&fixture.repository, "node_modules", "left-pad");
    // `vendors` is intentionally absent.

    let created = fixture.json_success(&["--json", "worktree", "create", "--task", "deps"]);

    let path = Path::new(created["data"]["path"].as_str().unwrap());
    let link = path.join("node_modules/left-pad");
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert!(link.join("index.js").is_file());
    assert!(!path.join("vendors").exists());
    assert_eq!(
        created["data"]["linked_dependencies"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let status = common::git_stdout(path, &["status", "--porcelain"]);
    assert_eq!(
        status, "",
        "linked dependencies must not dirty the worktree"
    );
}

#[test]
fn tracked_files_are_never_replaced() {
    let fixture = Fixture::with_options(
        "[\"/usr/bin/true\"]",
        "[worktree]\nlink_dependencies = [\"vendors\", \"node_modules\"]\n",
    );
    seed(&fixture.repository, "vendors", "pkg");
    fs::write(fixture.repository.join("vendors/tracked.txt"), "src\n").unwrap();
    git(&fixture.repository, &["add", "-f", "vendors/tracked.txt"]);
    git(&fixture.repository, &["commit", "-m", "track"]);

    let created = fixture.json_success(&["--json", "worktree", "create", "--task", "keep"]);

    let path = Path::new(created["data"]["path"].as_str().unwrap());
    assert!(!path.join("vendors/tracked.txt").is_symlink());
    assert!(path.join("vendors/pkg").is_symlink());
}

#[test]
fn nothing_is_linked_cloned_or_run_by_default() {
    let fixture = Fixture::new("/usr/bin/true");
    seed(&fixture.repository, "node_modules", "left-pad");

    let created = fixture.json_success(&["--json", "worktree", "create", "--task", "plain"]);

    let path = Path::new(created["data"]["path"].as_str().unwrap());
    assert!(!path.join("node_modules").exists());
    assert_eq!(
        created["data"]["linked_dependencies"],
        serde_json::json!([])
    );
    assert_eq!(
        created["data"]["cloned_dependencies"],
        serde_json::json!([])
    );
    assert_eq!(created["data"]["setup"], serde_json::json!([]));
    let directory = path.file_name().unwrap().to_str().unwrap();
    assert!(directory.starts_with("wt-"), "{directory}");
}

#[test]
fn cloned_dependencies_are_isolated_copies_and_support_nested_paths() {
    let fixture = Fixture::with_options(
        "[\"/usr/bin/true\"]",
        "[worktree]\nclone_dependencies = [\"app/vendor\", \"missing\"]\n",
    );
    fs::create_dir_all(fixture.repository.join("app")).unwrap();
    fs::write(fixture.repository.join("app/composer.json"), "{}\n").unwrap();
    fs::write(
        fixture.repository.join(".gitignore"),
        "vendor/\n.lprm.toml\n",
    )
    .unwrap();
    git(&fixture.repository, &["add", "."]);
    git(&fixture.repository, &["commit", "-m", "app"]);
    seed(&fixture.repository, "app/vendor", "pkg");

    let created = fixture.json_success(&["--json", "worktree", "create", "--task", "clone"]);

    let path = Path::new(created["data"]["path"].as_str().unwrap());
    let copy = path.join("app/vendor/pkg/index.js");
    assert!(copy.is_file());
    assert!(!path.join("app/vendor").is_symlink());
    assert!(!path.join("app/vendor/pkg").is_symlink());
    assert_eq!(
        created["data"]["cloned_dependencies"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fs::write(&copy, "changed\n").unwrap();
    assert_eq!(
        fs::read_to_string(fixture.repository.join("app/vendor/pkg/index.js")).unwrap(),
        "x\n",
        "writing the clone must not change the source checkout"
    );
}

#[test]
fn setup_commands_run_in_the_worktree_and_are_reported() {
    let fixture = Fixture::with_options(
        "[\"/usr/bin/true\"]",
        "[worktree]\nsetup_commands = [[\"/usr/bin/touch\", \"prepared.marker\"]]\n",
    );

    let created = fixture.json_success(&["--json", "worktree", "create", "--task", "setup"]);

    let path = Path::new(created["data"]["path"].as_str().unwrap());
    assert!(path.join("prepared.marker").is_file());
    let setup = created["data"]["setup"].as_array().unwrap();
    assert_eq!(setup.len(), 1);
    assert_eq!(setup[0]["result"]["status"], "passed");
}

#[test]
fn failing_setup_command_errors_but_keeps_the_worktree() {
    let fixture = Fixture::with_options(
        "[\"/usr/bin/true\"]",
        "[worktree]\nsetup_commands = [[\"/usr/bin/false\"], [\"/usr/bin/touch\", \"never.marker\"]]\n",
    );

    let output = fixture.lprm(&["--json", "worktree", "create", "--task", "broken"]);

    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Worktree setup command 1"), "{text}");
    assert!(text.contains("kept at"), "{text}");
    let listed = common::git_stdout(&fixture.repository, &["worktree", "list"]);
    assert!(listed.contains("wt-"), "worktree must be kept: {listed}");
}

#[test]
fn clone_dependencies_must_be_plain_relative_paths() {
    for bad in ["../escape", "/abs", ""] {
        let config = format!("schema_version = 1\n[worktree]\nclone_dependencies = [\"{bad}\"]\n");
        assert!(lprm_core::Config::from_toml(&config).is_err(), "{bad}");
    }
    assert!(
        lprm_core::Config::from_toml(
            "schema_version = 1\n[worktree]\nclone_dependencies = [\"app/vendor\"]\n"
        )
        .is_ok()
    );
}
