use std::path::Path;

use lprm_core::{ErrorCode, ReviewReport, rules_review};
use lprm_store::{NewPullRequest, NewRevision, Store, TransitionEvidence};

const SOURCE: &str = "1111111111111111111111111111111111111111";
const TARGET: &str = "0000000000000000000000000000000000000000";

fn store_with_pr() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store
        .register_repository("repo-1", Path::new("/tmp/repo/.git"), "dev", "config")
        .unwrap();
    store
        .register_worktree("wt-1", "repo-1", "feature/a", Path::new("/tmp/wt"), None)
        .unwrap();
    store
        .create_pull_request(&NewPullRequest {
            id: "pr-1".to_owned(),
            repository_id: "repo-1".to_owned(),
            worktree_id: Some("wt-1".to_owned()),
            title: "A".to_owned(),
            body: String::new(),
            source_branch: "feature/a".to_owned(),
            target_branch: "dev".to_owned(),
            revision_id: "revision-1".to_owned(),
            source_sha: SOURCE.to_owned(),
            target_sha: TARGET.to_owned(),
            policy_hash: "policy".to_owned(),
            check_config_hash: "checks".to_owned(),
            audit_id: "audit-0".to_owned(),
            actor: "codex".to_owned(),
        })
        .unwrap();
    store
}

fn evidence(revision: &str, audit: &str, actor: &str) -> TransitionEvidence {
    TransitionEvidence {
        audit_id: audit.to_owned(),
        actor: actor.to_owned(),
        reason: "because".to_owned(),
        revision_id: revision.to_owned(),
        source_sha: SOURCE.to_owned(),
        target_sha: TARGET.to_owned(),
        policy_hash: "policy".to_owned(),
    }
}

fn report(paths: &[&str]) -> ReviewReport {
    let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
    rules_review(&paths, &["auth/**".to_owned()])
}

#[test]
fn reviews_and_approvals_are_persisted_with_audit_and_creator() {
    let mut store = store_with_pr();
    let first = evidence("revision-1", "audit-1", "reviewer");
    store
        .record_review_run(
            "repo-1",
            "pr-1",
            "review-1",
            &first,
            &report(&["auth/a.rs"]),
        )
        .unwrap();
    store
        .record_approval(
            "repo-1",
            "pr-1",
            "approval-1",
            &evidence("revision-1", "audit-2", "operator"),
            "manual",
        )
        .unwrap();

    let reports = store.review_reports("revision-1").unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].findings[0].code, "PROTECTED_PATH");
    let approvals = store.approvals("revision-1").unwrap();
    assert_eq!(approvals[0].actor, "operator");
    assert_eq!(
        store.pull_request_creator("repo-1", "pr-1").unwrap(),
        Some("codex".to_owned())
    );
    let events = store.audit_events("repo-1", Some("pr-1")).unwrap();
    let kinds: Vec<_> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    assert_eq!(
        kinds,
        ["pr_created", "review_recorded", "approval_recorded"]
    );
}

#[test]
fn rerunning_a_reviewer_keeps_only_its_latest_report() {
    let mut store = store_with_pr();
    store
        .record_review_run(
            "repo-1",
            "pr-1",
            "review-1",
            &evidence("revision-1", "audit-1", "r"),
            &report(&["auth/a.rs"]),
        )
        .unwrap();
    store
        .record_review_run(
            "repo-1",
            "pr-1",
            "review-2",
            &evidence("revision-1", "audit-2", "r"),
            &report(&["src/a.rs"]),
        )
        .unwrap();

    let reports = store.review_reports("revision-1").unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].findings.len(), 0);
}

#[test]
fn stale_evidence_is_rejected_and_new_revisions_start_clean() {
    let mut store = store_with_pr();
    store
        .record_approval(
            "repo-1",
            "pr-1",
            "approval-1",
            &evidence("revision-1", "audit-1", "operator"),
            "manual",
        )
        .unwrap();
    store
        .add_pull_request_revision(
            "repo-1",
            "pr-1",
            &NewRevision {
                revision_id: "revision-2".to_owned(),
                source_sha: "2222222222222222222222222222222222222222".to_owned(),
                target_sha: TARGET.to_owned(),
                policy_hash: "policy".to_owned(),
                check_config_hash: "checks".to_owned(),
                audit_id: "audit-rev".to_owned(),
                actor: "codex".to_owned(),
            },
        )
        .unwrap();

    let stale = store
        .record_approval(
            "repo-1",
            "pr-1",
            "approval-2",
            &evidence("revision-1", "audit-2", "operator"),
            "manual",
        )
        .unwrap_err();
    assert_eq!(stale.code, ErrorCode::StaleRevision);
    assert_eq!(store.approvals("revision-2").unwrap().len(), 0);
    assert_eq!(store.review_reports("revision-2").unwrap().len(), 0);
    assert_eq!(store.approvals("revision-1").unwrap().len(), 1);
}
