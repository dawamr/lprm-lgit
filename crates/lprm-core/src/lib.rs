//! Domain types and orchestration for LPRM.

mod config;
mod error;
mod evidence;
mod identity;
mod output;
mod pr;
mod review;
mod worktree;

pub use config::{
    ChecksConfig, Config, EventsConfig, MergeConfig, MergeStrategy, PolicyConfig, RepositoryConfig,
    ReviewConfig, ReviewMode, RiskLevel,
};
pub use error::{ErrorCode, LprmError};
pub use evidence::{AdmissionGates, EvidenceFreshness, FingerprintInput, RevisionFingerprint};
pub use identity::{AgentIdentity, require_role_separation};
pub use output::{ApiError, Envelope, SCHEMA_VERSION};
pub use pr::PullRequestStatus;
pub use review::{
    ApprovalEvidence, Finding, PolicyAssessment, REVIEW_SCHEMA_VERSION, ReviewReport,
    ReviewVerdict, ReviewerIdentity, Severity, assess_policy, glob_match, rules_review,
};
pub use worktree::{WorktreeSpec, link_dependency_directories, validate_managed_worktree_path};

/// The workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
