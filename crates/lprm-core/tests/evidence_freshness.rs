use lprm_core::{Config, FingerprintInput, PullRequestStatus, RevisionFingerprint};

fn fingerprint() -> RevisionFingerprint {
    RevisionFingerprint {
        source_sha: "source-1".to_owned(),
        target_sha: "target-1".to_owned(),
        policy_hash: "policy-1".to_owned(),
        check_config_hash: "checks-1".to_owned(),
    }
}

#[test]
fn each_fingerprint_input_independently_invalidates_all_evidence() {
    let recorded = fingerprint();
    let cases = [
        (
            FingerprintInput::SourceSha,
            RevisionFingerprint {
                source_sha: "source-2".to_owned(),
                ..fingerprint()
            },
        ),
        (
            FingerprintInput::TargetSha,
            RevisionFingerprint {
                target_sha: "target-2".to_owned(),
                ..fingerprint()
            },
        ),
        (
            FingerprintInput::Policy,
            RevisionFingerprint {
                policy_hash: "policy-2".to_owned(),
                ..fingerprint()
            },
        ),
        (
            FingerprintInput::CheckConfig,
            RevisionFingerprint {
                check_config_hash: "checks-2".to_owned(),
                ..fingerprint()
            },
        ),
    ];

    for (changed, current) in cases {
        let freshness = recorded.assess(&current, PullRequestStatus::Approved);

        assert_eq!(freshness.changed_inputs, [changed]);
        assert!(!freshness.checks_current);
        assert!(!freshness.reviews_current);
        assert!(!freshness.approvals_current);
        assert!(!freshness.queue_evidence_current);
        assert_eq!(freshness.status, PullRequestStatus::Ready);
    }
}

#[test]
fn exact_tuple_keeps_evidence_current_and_terminal_states_never_reopen() {
    let recorded = fingerprint();
    let current = recorded.assess(&recorded, PullRequestStatus::Approved);
    assert_eq!(current.changed_inputs, Vec::<FingerprintInput>::new());
    assert!(current.checks_current);
    assert!(current.reviews_current);
    assert!(current.approvals_current);
    assert!(current.queue_evidence_current);
    assert_eq!(current.status, PullRequestStatus::Approved);

    let stale = recorded.assess(
        &RevisionFingerprint {
            source_sha: "source-2".to_owned(),
            ..fingerprint()
        },
        PullRequestStatus::Merged,
    );
    assert_eq!(stale.status, PullRequestStatus::Merged);

    let stale_queued = recorded.assess(
        &RevisionFingerprint {
            target_sha: "target-2".to_owned(),
            ..fingerprint()
        },
        PullRequestStatus::Queued,
    );
    assert_eq!(stale_queued.status, PullRequestStatus::Ready);
}

#[test]
fn policy_and_check_configuration_have_independent_hashes() {
    let baseline = Config::default();
    let mut policy_changed = baseline.clone();
    policy_changed
        .policy
        .protected_paths
        .push("secrets/**".to_owned());
    let mut checks_changed = baseline.clone();
    checks_changed.checks.timeout_seconds += 1;

    assert_ne!(
        policy_changed.policy_fingerprint().unwrap(),
        baseline.policy_fingerprint().unwrap()
    );
    assert_eq!(
        policy_changed.check_config_fingerprint().unwrap(),
        baseline.check_config_fingerprint().unwrap()
    );
    assert_eq!(
        checks_changed.policy_fingerprint().unwrap(),
        baseline.policy_fingerprint().unwrap()
    );
    assert_ne!(
        checks_changed.check_config_fingerprint().unwrap(),
        baseline.check_config_fingerprint().unwrap()
    );
}
