use lprm_core::PullRequestStatus;

#[test]
fn transition_matrix_covers_every_state_pair() {
    use PullRequestStatus::{
        Approved, Blocked, ChangesRequested, Checking, Closed, Conflict, Draft, Failed, Merged,
        Merging, Queued, Ready,
    };

    let allowed = [
        (Draft, Ready),
        (Draft, Closed),
        (Ready, Checking),
        (Ready, Closed),
        (Checking, ChangesRequested),
        (Checking, Approved),
        (Checking, Conflict),
        (Checking, Blocked),
        (Checking, Failed),
        (ChangesRequested, Ready),
        (ChangesRequested, Closed),
        (Approved, Ready),
        (Approved, Queued),
        (Queued, Ready),
        (Queued, Merging),
        (Merging, Merged),
        (Merging, Failed),
        (Conflict, Ready),
        (Conflict, Closed),
        (Blocked, Ready),
        (Blocked, Closed),
        (Failed, Ready),
        (Failed, Closed),
    ];

    let mut pairs_checked = 0;
    for from in PullRequestStatus::ALL {
        for to in PullRequestStatus::ALL {
            let expected = allowed.contains(&(from, to));
            assert_eq!(
                from.can_transition_to(to),
                expected,
                "unexpected transition decision for {from:?} -> {to:?}"
            );
            assert_eq!(
                from.transition_to(to).is_ok(),
                expected,
                "unexpected transition result for {from:?} -> {to:?}"
            );
            pairs_checked += 1;
        }
    }

    assert_eq!(pairs_checked, 12 * 12);
}

#[test]
fn public_status_names_match_the_persistence_contract() {
    let names = PullRequestStatus::ALL.map(PullRequestStatus::as_str);

    assert_eq!(
        names,
        [
            "draft",
            "ready",
            "checking",
            "changes_requested",
            "approved",
            "queued",
            "merging",
            "merged",
            "conflict",
            "blocked",
            "failed",
            "closed",
        ]
    );
}

#[test]
fn terminal_states_never_reopen() {
    for terminal in [PullRequestStatus::Merged, PullRequestStatus::Closed] {
        assert!(
            PullRequestStatus::ALL
                .into_iter()
                .all(|next| !terminal.can_transition_to(next))
        );
    }
}
