use crate::LprmError;

const MAX_IDENTITY_CHARACTERS: usize = 128;

/// A caller-supplied identity used for cooperative ownership and audit attribution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentIdentity(String);

impl AgentIdentity {
    /// Validates an identity at the CLI boundary without silently normalizing it.
    pub fn parse(value: &str) -> Result<Self, LprmError> {
        if value.is_empty() {
            return Err(LprmError::invalid_argument(
                "Agent identity must not be empty",
            ));
        }
        if value.trim() != value || value.chars().any(char::is_control) {
            return Err(LprmError::invalid_argument(
                "Agent identity must not contain surrounding whitespace or control characters",
            ));
        }
        if value.chars().count() > MAX_IDENTITY_CHARACTERS {
            return Err(LprmError::invalid_argument(format!(
                "Agent identity must not exceed {MAX_IDENTITY_CHARACTERS} characters"
            )));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Enforces that two policy roles are held by distinct, self-declared identities.
pub fn require_role_separation(
    contributor: &AgentIdentity,
    approver: &AgentIdentity,
) -> Result<(), LprmError> {
    if contributor == approver {
        return Err(LprmError::validation_failed(
            "Contributor and approver identities must be different",
        ));
    }
    Ok(())
}
