//! Offline P0 acceptance matrix (AC-05 to AC-10) driven through the public CLI.
//!
//! Every test uses an ephemeral repository without a remote, keys, or network.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use common::{Fixture, assert_success, checkout_snapshot, git, git_stdout};
use serde_json::Value;

#[test]
fn ac06_failing_required_check_blocks_the_queue_and_keeps_logs() {
    let fixture = Fixture::new("/usr/bin/false");
    let submitted = fixture.submit("feature.txt");
    assert_success(&fixture.lprm(&["--json", "pr", "ready", &submitted.pr_id]));

    let checked = fixture.lprm(&["--json", "pr", "check", &submitted.pr_id]);
    let envelope: Value = serde_json::from_slice(&checked.stdout).unwrap();
    assert_ne!(
        envelope["data"]["pull_request"]["status"], "approved",
        "failing check must not approve: {envelope}"
    );

    let enqueue = fixture.lprm(&["--json", "pr", "enqueue", &submitted.pr_id]);
    assert!(!enqueue.status.success(), "enqueue must be refused");
    let queue = fixture.json_success(&["--json", "queue", "list"]);
    assert_eq!(queue["data"].as_array().unwrap().len(), 0);
    let target_before = git_stdout(&fixture.repository, &["rev-parse", "dev"]);
    fixture.json_success(&["--json", "queue", "run", "--once"]);
    assert_eq!(
        git_stdout(&fixture.repository, &["rev-parse", "dev"]),
        target_before
    );
}

#[test]
fn ac07_changing_the_source_invalidates_the_validation() {
    let fixture = Fixture::new("/usr/bin/true");
    let submitted = fixture.submit("feature.txt");
    assert_success(&fixture.lprm(&["--json", "pr", "ready", &submitted.pr_id]));
    assert_success(&fixture.lprm(&["--json", "pr", "check", &submitted.pr_id]));

    fs::write(submitted.path.join("extra.txt"), "late\n").unwrap();
    git(&submitted.path, &["add", "extra.txt"]);
    git(&submitted.path, &["commit", "-m", "late change"]);

    let enqueue = fixture.lprm(&["--json", "pr", "enqueue", &submitted.pr_id]);
    assert_eq!(enqueue.status.code(), Some(6), "stale source must exit 6");
    let queue = fixture.json_success(&["--json", "queue", "list"]);
    assert_eq!(queue["data"].as_array().unwrap().len(), 0);
}

#[test]
fn ac09_older_candidate_is_revalidated_against_the_new_target_head() {
    let fixture = Fixture::new("/usr/bin/true");
    let first = fixture.submit("first.txt");
    let second = fixture.submit("second.txt");
    fixture.approve_and_enqueue(&first.pr_id);
    fixture.approve_and_enqueue(&second.pr_id);

    let run = fixture.json_success(&["--json", "queue", "run", "--once"]);
    assert_eq!(run["data"]["processed"], 1);
    assert_eq!(fixture.status(&first.pr_id), "merged");
    let target_after_first = git_stdout(&fixture.repository, &["rev-parse", "dev"]);

    // The second PR was validated against the old head: it must be demoted, never merged.
    let run = fixture.json_success(&["--json", "queue", "run", "--once"]);
    assert_eq!(run["data"]["processed"], 0);
    assert_eq!(run["data"]["invalidated"][0], second.pr_id.as_str());
    assert_eq!(fixture.status(&second.pr_id), "ready");
    assert_eq!(
        git_stdout(&fixture.repository, &["rev-parse", "dev"]),
        target_after_first
    );

    // Without a new revision the stale evidence cannot be reused.
    let stale = fixture.lprm(&["--json", "pr", "check", &second.pr_id]);
    assert_eq!(stale.status.code(), Some(6));

    let synced = fixture.json_success(&["--json", "pr", "sync", &second.pr_id]);
    assert_eq!(synced["data"]["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(
        synced["data"]["revisions"][1]["target_sha"],
        target_after_first.as_str()
    );
    let repeated = fixture.lprm(&["--json", "pr", "sync", &second.pr_id]);
    assert_eq!(repeated.status.code(), Some(5), "unchanged tuple");
    assert_success(&fixture.lprm(&["--json", "pr", "check", &second.pr_id]));
    assert_success(&fixture.lprm(&["--json", "pr", "enqueue", &second.pr_id]));
    let run = fixture.json_success(&["--json", "queue", "run", "--once"]);
    assert_eq!(run["data"]["processed"], 1);
    assert_eq!(fixture.status(&second.pr_id), "merged");
    let target_after_second = git_stdout(&fixture.repository, &["rev-parse", "dev"]);
    let parents = git_stdout(
        &fixture.repository,
        &["rev-list", "--parents", "-n", "1", &target_after_second],
    );
    assert_eq!(
        parents.split_whitespace().nth(1),
        Some(target_after_first.as_str())
    );
}

#[test]
fn ac05_conflict_preflight_leaves_the_checkout_untouched() {
    let fixture = Fixture::new("/usr/bin/true");
    fs::write(fixture.repository.join("shared.txt"), "base\n").unwrap();
    git(&fixture.repository, &["add", "shared.txt"]);
    git(&fixture.repository, &["commit", "-m", "shared"]);
    let worktree = fixture.json_success(&["--json", "worktree", "create", "--task", "conflict"]);
    let path = PathBuf::from(worktree["data"]["path"].as_str().unwrap());
    let branch = worktree["data"]["branch"].as_str().unwrap();
    fs::write(path.join("shared.txt"), "source\n").unwrap();
    git(&path, &["commit", "-am", "source side"]);
    fs::write(fixture.repository.join("shared.txt"), "target\n").unwrap();
    git(&fixture.repository, &["commit", "-am", "target side"]);
    let created = fixture.json_success(&[
        "--json", "pr", "create", "--from", branch, "--title", "conflict",
    ]);
    let pr_id = created["data"]["pr_id"].as_str().unwrap();
    assert_success(&fixture.lprm(&["--json", "pr", "ready", pr_id]));
    fs::write(fixture.repository.join("untracked.txt"), "keep\n").unwrap();
    let before = checkout_snapshot(&fixture.repository);

    let checked = fixture.lprm(&["--json", "pr", "check", pr_id]);

    assert_eq!(checked.status.code(), Some(4));
    assert_eq!(fixture.status(pr_id), "conflict");
    assert_eq!(checkout_snapshot(&fixture.repository), before);
}

#[test]
fn ac08_concurrent_queue_runs_create_one_merge() {
    let fixture = Fixture::new("/usr/bin/true");
    let submitted = fixture.submit("feature.txt");
    fixture.approve_and_enqueue(&submitted.pr_id);
    let base = git_stdout(&fixture.repository, &["rev-parse", "dev"]);

    let children = (0..2)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_lgit"))
                .current_dir(&fixture.repository)
                .env("LPRM_STATE_HOME", &fixture.state)
                .args(["--json", "queue", "run", "--once"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let outputs = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect::<Vec<_>>();

    for output in &outputs {
        let code = output.status.code();
        assert!(matches!(code, Some(0 | 7)), "unexpected exit {code:?}");
    }
    let merges = git_stdout(
        &fixture.repository,
        &["rev-list", "--merges", "--count", &format!("{base}..dev")],
    );
    assert_eq!(merges, "1");
    assert_eq!(fixture.status(&submitted.pr_id), "merged");
}

#[test]
fn ac10_combined_tree_is_tested_before_integration() {
    // Each side passes alone; only the merged tree contains both files.
    let fixture = Fixture::with_check(
        "[\"/bin/sh\", \"-c\", \"test ! -e first.txt || test ! -e second.txt\"]",
    );
    let first = fixture.submit("first.txt");
    let second = fixture.submit("second.txt");
    fixture.approve_and_enqueue(&first.pr_id);
    assert_success(&fixture.lprm(&["--json", "pr", "ready", &second.pr_id]));
    assert_success(&fixture.lprm(&["--json", "pr", "check", &second.pr_id]));
    assert_success(&fixture.lprm(&["--json", "queue", "run", "--once"]));
    assert_eq!(fixture.status(&first.pr_id), "merged");
    let target_after_first = git_stdout(&fixture.repository, &["rev-parse", "dev"]);

    assert_success(&fixture.lprm(&["--json", "pr", "sync", &second.pr_id]));
    assert_success(&fixture.lprm(&["--json", "pr", "check", &second.pr_id]));
    assert_success(&fixture.lprm(&["--json", "pr", "enqueue", &second.pr_id]));
    let run = fixture.lprm(&["--json", "queue", "run", "--once"]);

    assert_eq!(run.status.code(), Some(5), "combined checks must fail");
    assert_eq!(
        git_stdout(&fixture.repository, &["rev-parse", "dev"]),
        target_after_first
    );
    assert_ne!(fixture.status(&second.pr_id), "merged");
    assert_eq!(fixture.status(&second.pr_id), "failed");
    // The queue is not blocked: a failed candidate releases the repository lock state.
    let next = fixture.json_success(&["--json", "queue", "run", "--once"]);
    assert_eq!(next["data"]["processed"], 0);
}

#[test]
fn source_drift_after_enqueue_demotes_instead_of_reconciling() {
    let fixture = Fixture::new("/usr/bin/true");
    let submitted = fixture.submit("feature.txt");
    fixture.approve_and_enqueue(&submitted.pr_id);
    fs::write(submitted.path.join("late.txt"), "late\n").unwrap();
    git(&submitted.path, &["add", "late.txt"]);
    git(&submitted.path, &["commit", "-m", "late"]);
    let target = git_stdout(&fixture.repository, &["rev-parse", "dev"]);

    let run = fixture.json_success(&["--json", "queue", "run", "--once"]);

    assert_eq!(run["data"]["processed"], 0);
    assert_eq!(run["data"]["invalidated"][0], submitted.pr_id.as_str());
    assert_eq!(fixture.status(&submitted.pr_id), "ready");
    assert_eq!(
        git_stdout(&fixture.repository, &["rev-parse", "dev"]),
        target
    );
    assert_success(&fixture.lprm(&["--json", "pr", "sync", &submitted.pr_id]));
    assert_success(&fixture.lprm(&["--json", "pr", "check", &submitted.pr_id]));
    assert_success(&fixture.lprm(&["--json", "pr", "enqueue", &submitted.pr_id]));
    let run = fixture.json_success(&["--json", "queue", "run", "--once"]);
    assert_eq!(run["data"]["processed"], 1);
    assert_eq!(fixture.status(&submitted.pr_id), "merged");
}
