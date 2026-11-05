//! Domain types and orchestration for LPRM.

mod error;
mod output;

pub use error::{ErrorCode, LprmError};
pub use output::{ApiError, Envelope, SCHEMA_VERSION};

/// The workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
