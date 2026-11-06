//! Dependency directories are symlinked from the source checkout into new worktrees.

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
    let fixture = Fixture::new("/usr/bin/true");
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
    let fixture = Fixture::new("/usr/bin/true");
    seed(&fixture.repository, "vendors", "pkg");
    fs::write(fixture.repository.join("vendors/tracked.txt"), "src\n").unwrap();
    git(&fixture.repository, &["add", "-f", "vendors/tracked.txt"]);
    git(&fixture.repository, &["commit", "-m", "track"]);

    let created = fixture.json_success(&["--json", "worktree", "create", "--task", "keep"]);

    let path = Path::new(created["data"]["path"].as_str().unwrap());
    assert!(!path.join("vendors/tracked.txt").is_symlink());
    assert!(path.join("vendors/pkg").is_symlink());
}
