//! Adversarial and stress coverage: corruption, busy database, hostile names, path swaps.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::{Fixture, assert_success, checkout_snapshot, git, git_stdout};
use serde_json::Value;

fn error_code(output: &std::process::Output) -> String {
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    document["error"]["code"].as_str().unwrap().to_owned()
}

fn state_database(fixture: &Fixture) -> PathBuf {
    let mut databases = Vec::new();
    let mut pending = vec![fixture.state.clone()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "db") {
                databases.push(path);
            }
        }
    }
    assert_eq!(databases.len(), 1, "expected one state database");
    databases.remove(0)
}

#[test]
fn corrupt_database_fails_closed_without_touching_the_repository() {
    let fixture = Fixture::new("/usr/bin/true");
    let database = state_database(&fixture);
    let before = checkout_snapshot(&fixture.repository);
    fs::write(&database, vec![0x5a; 4096]).unwrap();
    for sidecar in ["-wal", "-shm"] {
        let mut name = database.clone().into_os_string();
        name.push(sidecar);
        let _ = fs::remove_file(PathBuf::from(name));
    }

    let output = fixture.lprm(&["--json", "queue", "list"]);

    assert_eq!(output.status.code(), Some(8));
    assert_eq!(error_code(&output), "INTERNAL");
    assert_eq!(checkout_snapshot(&fixture.repository), before);
}

#[test]
fn busy_database_reports_retryable_busy_instead_of_internal() {
    let fixture = Fixture::new("/usr/bin/true");
    let holder = rusqlite::Connection::open(state_database(&fixture)).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();

    let output = fixture.lprm(&["--json", "worktree", "create", "--task", "blocked"]);

    holder.execute_batch("ROLLBACK").unwrap();
    assert_eq!(output.status.code(), Some(7));
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["error"]["code"], "BUSY");
    assert_eq!(document["error"]["retryable"], true);
}

#[test]
fn hostile_task_names_never_escape_or_corrupt_state() {
    let fixture = Fixture::new("/usr/bin/true");
    let long = "x".repeat(300);
    let tasks = [
        "../../escape",
        "--help",
        "-rf /",
        "line\nbreak",
        "tab\tname",
        "refs/heads/dev",
        "dev",
        "ünï/cødé",
        "semi;colon && $(touch pwned)",
        "..",
        ".lock",
        &long,
    ];
    let mut root: Option<PathBuf> = None;
    for task in tasks {
        let output = fixture.lprm(&["--json", "worktree", "create", "--task", task]);
        let code = output.status.code();
        assert!(
            matches!(code, Some(0 | 2)),
            "task {task:?} exited {code:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        if code == Some(0) {
            let document: Value = serde_json::from_slice(&output.stdout).unwrap();
            let path = document["data"]["path"].as_str().unwrap();
            let branch = document["data"]["branch"].as_str().unwrap();
            let parent = Path::new(path).parent().unwrap().to_path_buf();
            assert_eq!(
                *root.get_or_insert_with(|| parent.clone()),
                parent,
                "worktree {path:?} must be a direct child of the managed root"
            );
            assert!(!path.contains(".."));
            assert!(branch.starts_with("lprm/"));
            git(
                &fixture.repository,
                &["check-ref-format", "--branch", branch],
            );
        }
    }
    assert!(!fixture.repository.join("pwned").exists());
    assert_eq!(
        git_stdout(&fixture.repository, &["status", "--porcelain"]),
        ""
    );
    let listed = fixture.lprm(&["--json", "worktree", "list"]);
    assert_success(&listed);
}

#[test]
fn cleanup_refuses_a_worktree_path_swapped_for_a_symlink() {
    let fixture = Fixture::new("/usr/bin/true");
    let created = fixture.json_success(&["--json", "worktree", "create", "--task", "swap"]);
    let id = created["data"]["id"].as_str().unwrap();
    let path = PathBuf::from(created["data"]["path"].as_str().unwrap());
    let victim = fixture.repository.parent().unwrap().join("victim");
    fs::create_dir(&victim).unwrap();
    fs::write(victim.join("precious.txt"), "keep\n").unwrap();
    fs::rename(&path, fixture.repository.parent().unwrap().join("moved")).unwrap();
    std::os::unix::fs::symlink(&victim, &path).unwrap();

    let output = fixture.lprm(&["--json", "--yes", "worktree", "cleanup", id]);

    assert!(!output.status.success(), "symlinked path must be refused");
    assert_eq!(
        fs::read_to_string(victim.join("precious.txt")).unwrap(),
        "keep\n"
    );
}

#[test]
fn repeated_concurrent_queue_runs_never_duplicate_a_merge() {
    for round in 0..5 {
        let fixture = Fixture::new("/usr/bin/true");
        let first = fixture.submit("first.txt");
        let second = fixture.submit("second.txt");
        for id in [&first.pr_id, &second.pr_id] {
            assert_success(&fixture.lprm(&["--json", "pr", "ready", id]));
            assert_success(&fixture.lprm(&["--json", "pr", "check", id]));
        }
        assert_success(&fixture.lprm(&["--json", "pr", "enqueue", &first.pr_id]));
        let base = git_stdout(&fixture.repository, &["rev-parse", "dev"]);

        let children = (0..4)
            .map(|_| {
                Command::new(env!("CARGO_BIN_EXE_lprm"))
                    .current_dir(&fixture.repository)
                    .env("LPRM_STATE_HOME", &fixture.state)
                    .args(["--json", "queue", "run", "--all"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for child in children {
            let output = child.wait_with_output().unwrap();
            assert!(
                matches!(output.status.code(), Some(0 | 7)),
                "round {round}: {:?}",
                output.status.code()
            );
        }

        let merges = git_stdout(
            &fixture.repository,
            &["rev-list", "--merges", "--count", &format!("{base}..dev")],
        );
        assert_eq!(merges, "1", "round {round}");
        assert_eq!(
            git_stdout(&fixture.repository, &["status", "--porcelain"]),
            ""
        );
    }
}
