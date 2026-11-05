//! Persistence for review runs and approvals, always bound to one exact revision.

use lprm_core::{ApprovalEvidence, LprmError, ReviewReport};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior};
use serde_json::json;

use super::{Store, TransitionEvidence, insert_audit_event, persistence_error};

/// Fails with a stale-revision error unless `evidence` names the latest immutable revision.
fn ensure_latest_revision(
    transaction: &Transaction<'_>,
    repository_id: &str,
    pull_request_id: &str,
    evidence: &TransitionEvidence,
) -> Result<(), LprmError> {
    let exists = transaction
        .query_row(
            "SELECT 1 FROM pull_requests WHERE repository_id = ?1 AND id = ?2",
            (repository_id, pull_request_id),
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| persistence_error("read pull request", error))?
        .is_some();
    if !exists {
        return Err(LprmError::invalid_argument("Unknown pull request ID"));
    }
    let latest = transaction
        .query_row(
            "SELECT id, source_sha, target_sha, policy_hash FROM pr_revisions \
             WHERE pr_id = ?1 ORDER BY revision_number DESC LIMIT 1",
            [pull_request_id],
            |row| {
                Ok([
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ])
            },
        )
        .map_err(|error| persistence_error("read latest revision", error))?;
    if latest
        != [
            evidence.revision_id.clone(),
            evidence.source_sha.clone(),
            evidence.target_sha.clone(),
            evidence.policy_hash.clone(),
        ]
    {
        return Err(LprmError::stale_revision("Review evidence is stale"));
    }
    Ok(())
}

impl Store {
    /// Persists a validated review report for the latest revision and audits it atomically.
    ///
    /// # Errors
    ///
    /// Returns a validation error for an invalid report, a stale-revision error when `evidence`
    /// is not the latest revision, or an internal error on persistence failure.
    pub fn record_review_run(
        &mut self,
        repository_id: &str,
        pull_request_id: &str,
        review_id: &str,
        evidence: &TransitionEvidence,
        report: &ReviewReport,
    ) -> Result<(), LprmError> {
        report.validate()?;
        let document = serde_json::to_string(report)
            .map_err(|_| LprmError::internal("Review report serialization failed"))?;
        let verdict = serde_json::to_value(report.verdict)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| LprmError::internal("Review verdict serialization failed"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start review transaction", error))?;
        ensure_latest_revision(&transaction, repository_id, pull_request_id, evidence)?;
        transaction
            .execute(
                "INSERT INTO review_runs(id, revision_id, reviewer_kind, verdict, findings_json, \
                 created_at) VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                (
                    review_id,
                    &evidence.revision_id,
                    &report.reviewer.kind,
                    verdict,
                    document,
                ),
            )
            .map_err(|error| persistence_error("record review run", error))?;
        let payload = json!({
            "reason": evidence.reason,
            "revision_id": evidence.revision_id,
            "source_sha": evidence.source_sha,
            "target_sha": evidence.target_sha,
            "policy_hash": evidence.policy_hash,
            "reviewer_kind": report.reviewer.kind,
            "finding_count": report.findings.len(),
        });
        insert_audit_event(
            &transaction,
            (&evidence.audit_id, repository_id, pull_request_id),
            ("review_recorded", &evidence.actor, &payload),
        )?;
        transaction
            .commit()
            .map_err(|error| persistence_error("commit review run", error))
    }

    /// Records an approval of the latest revision; `evidence.reason` is the justification.
    ///
    /// # Errors
    ///
    /// Returns a stale-revision error when `evidence` is not the latest revision, or an internal
    /// error on persistence failure.
    pub fn record_approval(
        &mut self,
        repository_id: &str,
        pull_request_id: &str,
        approval_id: &str,
        evidence: &TransitionEvidence,
        kind: &str,
    ) -> Result<(), LprmError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence_error("start approval transaction", error))?;
        ensure_latest_revision(&transaction, repository_id, pull_request_id, evidence)?;
        transaction
            .execute(
                "INSERT INTO approvals(id, revision_id, actor, kind, reason, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                (
                    approval_id,
                    &evidence.revision_id,
                    &evidence.actor,
                    kind,
                    &evidence.reason,
                ),
            )
            .map_err(|error| persistence_error("record approval", error))?;
        let payload = json!({
            "reason": evidence.reason,
            "kind": kind,
            "revision_id": evidence.revision_id,
            "source_sha": evidence.source_sha,
            "target_sha": evidence.target_sha,
            "policy_hash": evidence.policy_hash,
        });
        insert_audit_event(
            &transaction,
            (&evidence.audit_id, repository_id, pull_request_id),
            ("approval_recorded", &evidence.actor, &payload),
        )?;
        transaction
            .commit()
            .map_err(|error| persistence_error("commit approval", error))
    }

    /// Latest report per reviewer kind for one revision, in reviewer-kind order.
    ///
    /// # Errors
    ///
    /// Returns an internal error when rows cannot be read or decoded.
    pub fn review_reports(&self, revision_id: &str) -> Result<Vec<ReviewReport>, LprmError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT reviewer_kind, findings_json FROM review_runs \
                 WHERE revision_id = ?1 ORDER BY created_at, id",
            )
            .map_err(|error| persistence_error("prepare review query", error))?;
        let rows = statement
            .query_map([revision_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| persistence_error("query review runs", error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| persistence_error("decode review runs", error))?;
        let mut latest = std::collections::BTreeMap::new();
        for (kind, document) in rows {
            let report = ReviewReport::from_json(document.as_bytes())
                .map_err(|_| LprmError::internal("Stored review report is invalid"))?;
            latest.insert(kind, report);
        }
        Ok(latest.into_values().collect())
    }

    /// Approvals recorded against exactly one revision, oldest first.
    ///
    /// # Errors
    ///
    /// Returns an internal error when rows cannot be read.
    pub fn approvals(&self, revision_id: &str) -> Result<Vec<ApprovalEvidence>, LprmError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT actor, kind, reason FROM approvals WHERE revision_id = ?1 \
                 ORDER BY created_at, id",
            )
            .map_err(|error| persistence_error("prepare approval query", error))?;
        statement
            .query_map([revision_id], |row| {
                Ok(ApprovalEvidence {
                    actor: row.get(0)?,
                    kind: row.get(1)?,
                    reason: row.get(2)?,
                })
            })
            .map_err(|error| persistence_error("query approvals", error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| persistence_error("decode approvals", error))
    }

    /// The identity that created the PR, taken from its `pr_created` audit event.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the audit journal cannot be read.
    pub fn pull_request_creator(
        &self,
        repository_id: &str,
        pull_request_id: &str,
    ) -> Result<Option<String>, LprmError> {
        self.connection
            .query_row(
                "SELECT actor FROM audit_events WHERE repository_id = ?1 AND pr_id = ?2 \
                 AND event_type = 'pr_created' ORDER BY created_at, id LIMIT 1",
                (repository_id, pull_request_id),
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| persistence_error("read pull request creator", error))
    }
}
