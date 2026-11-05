use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ErrorCode, LprmError};

pub const SCHEMA_VERSION: u32 = 1;

/// Stable error object embedded in a machine-readable envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
    pub retryable: bool,
}

impl From<&LprmError> for ApiError {
    fn from(error: &LprmError) -> Self {
        Self {
            code: error.code,
            message: error.message.clone(),
            details: error.details.clone(),
            retryable: error.retryable,
        }
    }
}

/// Versioned output wrapper used by every JSON command response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Envelope<T> {
    pub schema_version: u32,
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<ApiError>,
}

impl<T> Envelope<T> {
    pub const fn success(data: T) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn failure(error: &LprmError) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            ok: false,
            data: None,
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::Envelope;
    use crate::LprmError;

    #[test]
    fn success_envelope_has_data_and_null_error() {
        let value = serde_json::to_value(Envelope::success(json!({"status": "ready"})))
            .expect("envelope should serialize");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["ok"], true);
        assert_eq!(value["data"]["status"], "ready");
        assert_eq!(value["error"], Value::Null);
    }

    #[test]
    fn failure_envelope_has_error_and_null_data() {
        let error = LprmError::invalid_argument("missing id");
        let value = serde_json::to_value(Envelope::<Value>::failure(&error))
            .expect("envelope should serialize");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["ok"], false);
        assert_eq!(value["data"], Value::Null);
        assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
    }
}
