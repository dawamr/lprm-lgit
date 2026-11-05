use lprm_core::{AgentIdentity, ErrorCode, require_role_separation};

#[test]
fn agent_identity_accepts_stable_bounded_values() {
    let identity = AgentIdentity::parse("codex:local/worker-1").expect("identity should parse");

    assert_eq!(identity.as_str(), "codex:local/worker-1");
}

#[test]
fn agent_identity_rejects_ambiguous_or_unbounded_values() {
    for invalid in ["", " codex", "codex ", "codex\nworker"] {
        let error = AgentIdentity::parse(invalid).expect_err("identity should be rejected");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    let oversized = "a".repeat(129);
    assert_eq!(
        AgentIdentity::parse(&oversized)
            .expect_err("oversized identity should be rejected")
            .code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn role_separation_rejects_self_approval_identity() {
    let contributor = AgentIdentity::parse("codex").unwrap();
    let same_actor = AgentIdentity::parse("codex").unwrap();
    let reviewer = AgentIdentity::parse("hermes").unwrap();

    let error = require_role_separation(&contributor, &same_actor)
        .expect_err("one identity must not fill both roles");
    assert_eq!(error.code, ErrorCode::ValidationFailed);
    require_role_separation(&contributor, &reviewer)
        .expect("distinct identities should satisfy role separation");
}
