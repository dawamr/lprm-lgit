use serde::{Deserialize, Serialize};

use crate::LprmError;

/// The persisted lifecycle status of a local pull request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestStatus {
    Draft,
    Ready,
    Checking,
    ChangesRequested,
    Approved,
    Queued,
    Merging,
    Merged,
    Conflict,
    Blocked,
    Failed,
    Closed,
}

impl PullRequestStatus {
    /// Every status in stable persistence and API order.
    pub const ALL: [Self; 12] = [
        Self::Draft,
        Self::Ready,
        Self::Checking,
        Self::ChangesRequested,
        Self::Approved,
        Self::Queued,
        Self::Merging,
        Self::Merged,
        Self::Conflict,
        Self::Blocked,
        Self::Failed,
        Self::Closed,
    ];

    /// Returns the stable lowercase name used by SQLite and JSON contracts.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Ready => "ready",
            Self::Checking => "checking",
            Self::ChangesRequested => "changes_requested",
            Self::Approved => "approved",
            Self::Queued => "queued",
            Self::Merging => "merging",
            Self::Merged => "merged",
            Self::Conflict => "conflict",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Closed => "closed",
        }
    }

    /// Reports whether the PRD permits moving directly to `next`.
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Draft
                    | Self::ChangesRequested
                    | Self::Conflict
                    | Self::Blocked
                    | Self::Failed,
                Self::Ready | Self::Closed
            ) | (Self::Ready, Self::Checking | Self::Closed)
                | (
                    Self::Checking,
                    Self::ChangesRequested
                        | Self::Approved
                        | Self::Conflict
                        | Self::Blocked
                        | Self::Failed
                )
                | (Self::Approved, Self::Ready | Self::Queued)
                | (Self::Queued, Self::Ready | Self::Merging)
                | (Self::Merging, Self::Merged | Self::Failed)
        )
    }

    /// Applies one guarded transition.
    ///
    /// Recovery code must inspect Git refs and the event journal before calling this method for a
    /// PR currently in `merging`.
    ///
    /// # Errors
    ///
    /// Returns a validation error when the transition is not part of the documented state machine.
    pub fn transition_to(self, next: Self) -> Result<Self, LprmError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(LprmError::validation_failed(format!(
                "Pull request cannot transition from {} to {}",
                self.as_str(),
                next.as_str()
            )))
        }
    }
}
