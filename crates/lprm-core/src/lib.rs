//! Domain types and orchestration for LPRM.

mod config;
mod error;
mod output;
mod worktree;

pub use config::{
    ChecksConfig, Config, MergeConfig, MergeStrategy, PolicyConfig, RepositoryConfig, ReviewConfig,
    ReviewMode, RiskLevel,
};
pub use error::{ErrorCode, LprmError};
pub use output::{ApiError, Envelope, SCHEMA_VERSION};
pub use worktree::WorktreeSpec;

/// The workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
