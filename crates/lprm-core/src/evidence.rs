use serde::{Deserialize, Serialize};

use crate::PullRequestStatus;

/// The complete immutable tuple to which checks, reviews, and approvals belong.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevisionFingerprint {
    pub source_sha: String,
    pub target_sha: String,
    pub policy_hash: String,
    pub check_config_hash: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintInput {
    SourceSha,
    TargetSha,
    Policy,
    CheckConfig,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceFreshness {
    pub changed_inputs: Vec<FingerprintInput>,
    pub checks_current: bool,
    pub reviews_current: bool,
    pub approvals_current: bool,
    pub queue_evidence_current: bool,
    pub status: PullRequestStatus,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdmissionGates {
    pub conflict_free: bool,
    pub required_checks_passed: bool,
    pub required_reviews_passed: bool,
    pub required_approvals_present: bool,
}

impl AdmissionGates {
    pub const fn all_satisfied(self) -> bool {
        self.conflict_free
            && self.required_checks_passed
            && self.required_reviews_passed
            && self.required_approvals_present
    }
}

impl RevisionFingerprint {
    /// Compares current repository evidence to the immutable tuple on a PR revision.
    pub fn assess(&self, current: &Self, status: PullRequestStatus) -> EvidenceFreshness {
        let mut changed_inputs = Vec::new();
        if self.source_sha != current.source_sha {
            changed_inputs.push(FingerprintInput::SourceSha);
        }
        if self.target_sha != current.target_sha {
            changed_inputs.push(FingerprintInput::TargetSha);
        }
        if self.policy_hash != current.policy_hash {
            changed_inputs.push(FingerprintInput::Policy);
        }
        if self.check_config_hash != current.check_config_hash {
            changed_inputs.push(FingerprintInput::CheckConfig);
        }
        let fresh = changed_inputs.is_empty();
        let status = if !fresh
            && matches!(
                status,
                PullRequestStatus::Approved | PullRequestStatus::Queued
            ) {
            PullRequestStatus::Ready
        } else {
            status
        };

        EvidenceFreshness {
            changed_inputs,
            checks_current: fresh,
            reviews_current: fresh,
            approvals_current: fresh,
            queue_evidence_current: fresh,
            status,
        }
    }
}
