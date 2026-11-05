use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Stable machine-readable error categories from the CLI contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidArgument,
    GitPrecondition,
    Conflict,
    ValidationFailed,
    StaleRevision,
    Busy,
    Internal,
}

impl ErrorCode {
    /// Returns the stable process exit code for this category.
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::InvalidArgument => 2,
            Self::GitPrecondition => 3,
            Self::Conflict => 4,
            Self::ValidationFailed => 5,
            Self::StaleRevision => 6,
            Self::Busy => 7,
            Self::Internal => 8,
        }
    }
}

/// An operator-facing error with a stable machine category.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct LprmError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
    pub retryable: bool,
}

impl LprmError {
    pub fn new(
        code: ErrorCode,
        message: impl Into<String>,
        details: Value,
        retryable: bool,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            retryable,
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::InvalidArgument,
            message,
            Value::Object(Map::default()),
            false,
        )
    }

    pub fn git_precondition(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::GitPrecondition,
            message,
            Value::Object(Map::default()),
            false,
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::Internal,
            message,
            Value::Object(Map::default()),
            false,
        )
    }

    pub const fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }
}

#[cfg(test)]
mod tests {
    use super::ErrorCode;

    #[test]
    fn error_codes_map_to_the_public_exit_contract() {
        assert_eq!(ErrorCode::InvalidArgument.exit_code(), 2);
        assert_eq!(ErrorCode::GitPrecondition.exit_code(), 3);
        assert_eq!(ErrorCode::Conflict.exit_code(), 4);
        assert_eq!(ErrorCode::ValidationFailed.exit_code(), 5);
        assert_eq!(ErrorCode::StaleRevision.exit_code(), 6);
        assert_eq!(ErrorCode::Busy.exit_code(), 7);
        assert_eq!(ErrorCode::Internal.exit_code(), 8);
    }
}
