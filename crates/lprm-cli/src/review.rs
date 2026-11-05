//! `pr review`, `pr approve`, and the review/approval policy shared with queue admission.

use std::path::PathBuf;
use std::time::Duration;

use lprm_core::{
    AgentIdentity, Config, LprmError, PolicyAssessment, ReviewMode, ReviewReport,
    RevisionFingerprint, assess_policy, require_role_separation, rules_review,
};
use lprm_git::Git;
use lprm_runner::{AdapterLimits, run_json_adapter};
use lprm_store::{PullRequestRecord, PullRequestRevisionRecord, Store};
use serde::Serialize;
use serde_json::{Value, json};

use super::{ReasonArgs, open_current_store, transition_evidence};

/// A pull request together with the repository facts needed to evaluate policy.
pub struct PolicyContext {
    git: Git,
    pub repository_root: PathBuf,
    pub config: Config,
    pub store: Store,
    pub repository_id: String,
    pub record: PullRequestRecord,
    pub revision: PullRequestRevisionRecord,
    pub fingerprint: RevisionFingerprint,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ReviewOutcome {
    pr_id: String,
    revision_id: String,
    reviews: Vec<ReviewReport>,
    assessment: PolicyAssessment,
}

#[derive(Debug, Serialize)]
struct ApprovalOutcome {
    pr_id: String,
    revision_id: String,
    approver: String,
    assessment: PolicyAssessment,
}

impl PolicyContext {
    /// Loads the PR and verifies its latest revision still matches the repository.
    ///
    /// # Errors
    ///
    /// Returns a stale-revision error when source, target, policy, or check config changed.
    pub fn load(pull_request_id: &str) -> Result<Self, LprmError> {
        let current_directory = std::env::current_dir()
            .map_err(|_| LprmError::internal("Current directory is unavailable"))?;
        let git = Git::default();
        let repository = git.discover(&current_directory)?;
        let config = Config::load_for_repository(&repository.worktree_root)?;
        let (store, layout) = open_current_store()?;
        let record = store
            .pull_request(layout.repository_id(), pull_request_id)?
            .ok_or_else(|| {
                LprmError::invalid_argument(format!("Unknown pull request ID {pull_request_id:?}"))
            })?;
        let revision = record
            .revisions
            .last()
            .cloned()
            .ok_or_else(|| LprmError::internal("Pull request has no revision history"))?;
        let fingerprint = RevisionFingerprint {
            source_sha: git
                .resolve_local_branch(&repository.worktree_root, &record.source_branch)?,
            target_sha: git
                .resolve_local_branch(&repository.worktree_root, &record.target_branch)?,
            policy_hash: config.policy_fingerprint()?,
            check_config_hash: config.check_config_fingerprint()?,
        };
        if revision.source_sha != fingerprint.source_sha
            || revision.target_sha != fingerprint.target_sha
            || revision.policy_hash != fingerprint.policy_hash
            || revision.check_config_hash != fingerprint.check_config_hash
        {
            return Err(LprmError::stale_revision(
                "Pull request revision changed; run pr sync first",
            ));
        }
        let diff = git.collect_diff(
            &repository.worktree_root,
            &revision.target_sha,
            &revision.source_sha,
        )?;
        let changed_paths = diff
            .files
            .iter()
            .flat_map(|file| std::iter::once(file.path.clone()).chain(file.old_path.clone()))
            .collect();
        Ok(Self {
            git,
            repository_root: repository.worktree_root,
            config,
            repository_id: layout.repository_id().to_owned(),
            store,
            record,
            revision,
            fingerprint,
            changed_paths,
        })
    }

    /// Evaluates review and approval gates using only evidence for the current revision.
    ///
    /// # Errors
    ///
    /// Returns an error when persisted evidence cannot be read.
    pub fn assess(&self) -> Result<PolicyAssessment, LprmError> {
        let creator = self
            .store
            .pull_request_creator(&self.repository_id, &self.record.id)?
            .unwrap_or_else(|| "local-operator".to_owned());
        Ok(assess_policy(
            &self.config,
            &creator,
            &self.changed_paths,
            &self.store.review_reports(&self.revision.id)?,
            &self.store.approvals(&self.revision.id)?,
        ))
    }
}

fn external_report(context: &PolicyContext) -> Result<ReviewReport, LprmError> {
    let review = &context.config.review;
    let files: Vec<Value> = context
        .git
        .collect_diff(
            &context.repository_root,
            &context.revision.target_sha,
            &context.revision.source_sha,
        )?
        .files
        .into_iter()
        .map(|file| {
            json!({
                "path": file.path,
                "old_path": file.old_path,
                "added_lines": file.added_lines,
                "deleted_lines": file.deleted_lines,
            })
        })
        .collect();
    let request = json!({
        "schema_version": 1,
        "pr_id": context.record.id,
        "title": context.record.title,
        "revision": {
            "id": context.revision.id,
            "source_sha": context.revision.source_sha,
            "target_sha": context.revision.target_sha,
        },
        "files": files,
    });
    let input = serde_json::to_vec(&request)
        .map_err(|_| LprmError::internal("Reviewer request serialization failed"))?;
    let output = run_json_adapter(
        &review.command,
        &context.repository_root,
        &input,
        AdapterLimits {
            timeout: Duration::from_secs(review.timeout_seconds),
            max_input_bytes: review.max_payload_bytes,
            max_output_bytes: review.max_payload_bytes,
        },
    )?;
    let mut report = ReviewReport::from_json(&output)?;
    // The adapter cannot claim to be the built-in reviewer.
    "external".clone_into(&mut report.reviewer.kind);
    Ok(report)
}

/// Runs the rules reviewer (and the external adapter when configured) for the current revision.
///
/// # Errors
///
/// Returns a stale-revision error for outdated PRs and a validation error when the external
/// reviewer fails, times out, or returns an invalid report.
pub fn review_pull_request(
    pull_request_id: &str,
    actor: &AgentIdentity,
) -> Result<Value, LprmError> {
    let mut context = PolicyContext::load(pull_request_id)?;
    let mut reports = vec![rules_review(
        &context.changed_paths,
        &context.config.policy.protected_paths,
    )];
    if context.config.review.mode == ReviewMode::External {
        reports.push(external_report(&context)?);
    }
    for report in &reports {
        let evidence = transition_evidence(
            &context.revision,
            actor,
            &format!("{} review recorded", report.reviewer.kind),
        );
        context.store.record_review_run(
            &context.repository_id,
            pull_request_id,
            &format!("review_{}", uuid::Uuid::now_v7().simple()),
            &evidence,
            report,
        )?;
    }
    let outcome = ReviewOutcome {
        pr_id: pull_request_id.to_owned(),
        revision_id: context.revision.id.clone(),
        reviews: context.store.review_reports(&context.revision.id)?,
        assessment: context.assess()?,
    };
    serde_json::to_value(outcome).map_err(|_| LprmError::internal("Review serialization failed"))
}

/// Records an eligible approval for the current revision.
///
/// # Errors
///
/// Returns a validation error when the approver is the contributor or a required justification
/// is missing, and a stale-revision error for outdated PRs.
pub fn approve_pull_request(
    arguments: &ReasonArgs,
    actor: &AgentIdentity,
) -> Result<Value, LprmError> {
    let mut context = PolicyContext::load(&arguments.id)?;
    let creator = context
        .store
        .pull_request_creator(&context.repository_id, &arguments.id)?
        .unwrap_or_else(|| "local-operator".to_owned());
    require_role_separation(&AgentIdentity::parse(&creator)?, actor)?;
    let before = context.assess()?;
    let reason = arguments.reason.as_deref().map_or("", str::trim);
    let justification_required = !before.blockers.is_empty() || before.approval_required;
    if justification_required && reason.is_empty() {
        return Err(LprmError::validation_failed(
            "Approving a risky or blocked pull request requires --reason",
        ));
    }
    let reason = if reason.is_empty() {
        "approved"
    } else {
        reason
    };
    context.store.record_approval(
        &context.repository_id,
        &arguments.id,
        &format!("approval_{}", uuid::Uuid::now_v7().simple()),
        &transition_evidence(&context.revision, actor, reason),
        "manual",
    )?;
    let outcome = ApprovalOutcome {
        pr_id: arguments.id.clone(),
        revision_id: context.revision.id.clone(),
        approver: actor.as_str().to_owned(),
        assessment: context.assess()?,
    };
    serde_json::to_value(outcome).map_err(|_| LprmError::internal("Approval serialization failed"))
}
