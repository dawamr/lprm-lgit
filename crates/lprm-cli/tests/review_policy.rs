//! Review, approval, external-reviewer, and auto-admission policy through the public CLI.

mod common;

use std::process::Output;

use common::{Fixture, Submitted, assert_success, git, git_stdout};
use serde_json::Value;

fn actor_run(fixture: &Fixture, actor: &str, arguments: &[&str]) -> Output {
    let mut command = vec!["--json", "--actor", actor];
    command.extend_from_slice(arguments);
    fixture.lprm(&command)
}

fn error_message(output: &Output) -> String {
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    document["error"]["message"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

fn checked(fixture: &Fixture, file: &str) -> Submitted {
    let submitted = fixture.submit(file);
    let id = &submitted.pr_id;
    assert_success(&actor_run(fixture, "validator", &["pr", "ready", id]));
    assert_success(&actor_run(fixture, "validator", &["pr", "check", id]));
    submitted
}

#[test]
fn protected_path_needs_a_separate_approver_with_a_reason() {
    let fixture = Fixture::new("/usr/bin/true");
    let pr = checked(&fixture, "auth/login.txt");
    let id = &pr.pr_id;

    let reviewed = fixture.json_success(&["--json", "--actor", "reviewer", "pr", "review", id]);
    assert_eq!(reviewed["data"]["assessment"]["risk"], "high");
    assert_eq!(
        reviewed["data"]["reviews"][0]["findings"][0]["code"],
        "PROTECTED_PATH"
    );

    let blocked = actor_run(&fixture, "coordinator", &["pr", "enqueue", id]);
    assert_eq!(blocked.status.code(), Some(5));
    assert!(error_message(&blocked).contains("requires an approval"));
    let own = actor_run(
        &fixture,
        "local-operator",
        &["pr", "approve", id, "--reason", "self"],
    );
    assert_eq!(own.status.code(), Some(5));
    assert!(error_message(&own).contains("must be different"));

    let no_reason = actor_run(&fixture, "operator", &["pr", "approve", id]);
    assert_eq!(no_reason.status.code(), Some(5));
    assert!(error_message(&no_reason).contains("--reason"));

    let approved = fixture.json_success(&[
        "--json",
        "--actor",
        "operator",
        "pr",
        "approve",
        id,
        "--reason",
        "reviewed auth change",
    ]);
    assert_eq!(
        approved["data"]["assessment"]["required_approvals_present"],
        true
    );
    assert_success(&actor_run(&fixture, "coordinator", &["pr", "enqueue", id]));
    assert_success(&actor_run(&fixture, "merger", &["queue", "run", "--once"]));
    let audit = fixture.json_success(&["--json", "audit", id]);
    let kinds: Vec<&str> = audit["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["event_type"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"review_recorded") && kinds.contains(&"approval_recorded"));
}

#[test]
fn approval_does_not_survive_a_new_revision() {
    let fixture = Fixture::new("/usr/bin/true");
    let pr = checked(&fixture, "auth/login.txt");
    let id = &pr.pr_id;
    assert_success(&actor_run(
        &fixture,
        "operator",
        &["pr", "approve", id, "--reason", "ok"],
    ));

    std::fs::write(pr.path.join("auth/extra.txt"), "late\n").unwrap();
    git(&pr.path, &["add", "."]);
    git(&pr.path, &["commit", "-m", "late"]);
    let stale = actor_run(&fixture, "coordinator", &["pr", "enqueue", id]);
    assert_eq!(stale.status.code(), Some(6));

    assert_success(&actor_run(&fixture, "validator", &["pr", "sync", id]));
    assert_success(&actor_run(&fixture, "validator", &["pr", "check", id]));
    let blocked = actor_run(&fixture, "coordinator", &["pr", "enqueue", id]);
    assert_eq!(
        blocked.status.code(),
        Some(5),
        "the old approval must not carry over"
    );
    let outdated = actor_run(
        &fixture,
        "operator",
        &["pr", "approve", id, "--reason", "again"],
    );
    assert_success(&outdated);
    assert_success(&actor_run(&fixture, "coordinator", &["pr", "enqueue", id]));
}

fn external_config(script: &str) -> String {
    format!(
        "[review]\nmode = \"external\"\ntimeout_seconds = 5\ncommand = [\"/bin/sh\", \"-c\", '''{script}''']\n"
    )
}

const PASS: &str = r#"cat >/dev/null; echo '{"schema_version":1,"reviewer":{"kind":"external","name":"bot"},"verdict":"pass","findings":[]}';"#;
const CRITICAL: &str = r#"cat >/dev/null; echo '{"schema_version":1,"reviewer":{"kind":"external","name":"bot"},"verdict":"changes_requested","findings":[{"severity":"critical","file":"src/a.txt","line":1,"code":"SECRET","message":"hard-coded secret"}]}';"#;

#[test]
fn external_review_is_required_and_can_block_until_approved() {
    let fixture = Fixture::with_options("[\"/usr/bin/true\"]", &external_config(CRITICAL));
    let pr = checked(&fixture, "src/a.txt");
    let id = &pr.pr_id;

    let missing = actor_run(&fixture, "coordinator", &["pr", "enqueue", id]);
    assert_eq!(missing.status.code(), Some(5));
    assert!(error_message(&missing).contains("External review has not run"));

    let reviewed = fixture.json_success(&["--json", "--actor", "reviewer", "pr", "review", id]);
    assert_eq!(reviewed["data"]["assessment"]["risk"], "critical");
    let blocked = actor_run(&fixture, "coordinator", &["pr", "enqueue", id]);
    assert_eq!(blocked.status.code(), Some(5));

    assert_success(&actor_run(
        &fixture,
        "operator",
        &["pr", "approve", id, "--reason", "accepted"],
    ));
    assert_success(&actor_run(&fixture, "coordinator", &["pr", "enqueue", id]));
}

#[test]
fn passing_external_review_allows_low_risk_admission() {
    let fixture = Fixture::with_options("[\"/usr/bin/true\"]", &external_config(PASS));
    let pr = checked(&fixture, "src/a.txt");
    assert_success(&actor_run(
        &fixture,
        "reviewer",
        &["pr", "review", &pr.pr_id],
    ));
    assert_success(&actor_run(
        &fixture,
        "coordinator",
        &["pr", "enqueue", &pr.pr_id],
    ));
}

#[test]
fn broken_external_reviewers_fail_closed() {
    for script in [
        "cat >/dev/null; echo 'not json';",
        "cat >/dev/null; exit 3;",
        "cat >/dev/null; echo '{\"schema_version\":9}';",
        "sleep 30;",
    ] {
        let fixture = Fixture::with_options("[\"/usr/bin/true\"]", &external_config(script));
        let pr = checked(&fixture, "src/a.txt");
        let review = actor_run(&fixture, "reviewer", &["pr", "review", &pr.pr_id]);
        assert_eq!(review.status.code(), Some(5), "{script}");
        let enqueue = actor_run(&fixture, "coordinator", &["pr", "enqueue", &pr.pr_id]);
        assert_eq!(enqueue.status.code(), Some(5), "{script}");
    }
}

fn auto_config(risks: &str) -> String {
    format!("[policy]\nauto_enqueue_risks = {risks}\nprotected_paths = [\"auth/**\"]\n")
}

#[test]
fn auto_admission_matrix() {
    // (listed risks, file, expect auto_enqueue present, expect queued)
    let cases = [
        ("[]", "src/a.txt", false, false),
        ("[\"low\"]", "src/a.txt", true, true),
        ("[\"low\"]", "auth/a.txt", false, false),
        ("[\"high\"]", "auth/a.txt", true, false),
    ];
    for (risks, file, present, queued) in cases {
        let fixture = Fixture::with_options("[\"/usr/bin/true\"]", &auto_config(risks));
        let pr = fixture.submit(file);
        assert_success(&actor_run(
            &fixture,
            "validator",
            &["pr", "ready", &pr.pr_id],
        ));
        let check =
            fixture.json_success(&["--json", "--actor", "validator", "pr", "check", &pr.pr_id]);
        let auto = &check["data"]["auto_enqueue"];
        assert_eq!(!auto.is_null(), present, "{risks} {file}: {check}");
        if present {
            assert_eq!(auto["queued"], queued, "{risks} {file}: {check}");
        }
        let queue = fixture.json_success(&["--json", "queue", "list"]);
        assert_eq!(queue["data"].as_array().unwrap().len(), usize::from(queued));
    }
}

#[test]
fn auto_admitted_pr_merges_through_the_normal_coordinator() {
    let fixture = Fixture::with_options("[\"/usr/bin/true\"]", &auto_config("[\"low\"]"));
    let pr = fixture.submit("src/a.txt");
    assert_success(&actor_run(
        &fixture,
        "validator",
        &["pr", "ready", &pr.pr_id],
    ));
    assert_success(&actor_run(
        &fixture,
        "validator",
        &["pr", "check", &pr.pr_id],
    ));
    let before = git_stdout(&fixture.repository, &["rev-parse", "dev"]);

    let run = fixture.json_success(&["--json", "queue", "run", "--once"]);

    assert_eq!(run["data"]["processed"], 1);
    assert_ne!(
        git_stdout(&fixture.repository, &["rev-parse", "dev"]),
        before
    );
}
