use std::fs;
use std::path::Path;
use std::time::Duration;

use lprm_core::{ChecksConfig, LprmError};
use lprm_git::{Git, GuardedMerge, MergeRecovery};
use lprm_runner::{CheckRunner, CheckSuiteResult};
use lprm_store::{
    CandidateMergeEvidence, CompleteMergeAttempt, MergeAttemptRecord, RepositoryLock,
    RequeueMergeAttempt, StartMergeAttempt, StateLayout, Store,
};
use serde::Serialize;

pub struct CoordinatorOptions<'a> {
    pub repository: &'a Path,
    pub layout: &'a StateLayout,
    pub checks: &'a ChecksConfig,
    pub lock_timeout: Duration,
    pub max_output_bytes: usize,
    pub actor: &'a str,
}

#[derive(Debug, Serialize)]
pub struct CoordinatorReport {
    pub attempt: MergeAttemptRecord,
    pub checks: CheckSuiteResult,
}

pub struct MergeRecoveryOptions<'a> {
    pub repository: &'a Path,
    pub layout: &'a StateLayout,
    pub dry_run: bool,
    pub lock_timeout: Duration,
    pub actor: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MergeRecoveryReport {
    pub attempt_id: String,
    pub pr_id: String,
    pub classification: &'static str,
    pub action: &'static str,
    pub observed_target_sha: String,
}

/// Reconciles active database attempts with exact Git ancestry evidence.
///
/// The operation never mutates Git. In dry-run mode it also leaves SQLite unchanged.
///
/// # Errors
///
/// Returns a stable lock, Git, validation, or persistence error when recovery cannot safely
/// classify and persist the observed state.
pub fn recover_merges(
    options: &MergeRecoveryOptions<'_>,
) -> Result<Vec<MergeRecoveryReport>, LprmError> {
    options.layout.prepare()?;
    let _lock = RepositoryLock::acquire(
        options.layout.lock_path(),
        options.lock_timeout,
        "recover-merges",
    )?;
    let mut store = Store::open(options.layout.database_path())?;
    let target_branch = store
        .repository_target(options.layout.repository_id())?
        .ok_or_else(|| LprmError::git_precondition("Repository initialization is missing"))?;
    let attempts = store
        .list_merge_attempts(options.layout.repository_id())?
        .into_iter()
        .filter(|attempt| {
            matches!(
                attempt.state.as_str(),
                "preparing" | "testing" | "integrating" | "reconciling"
            )
        })
        .collect::<Vec<_>>();
    let git = Git::default();
    let mut reports = Vec::with_capacity(attempts.len());
    for attempt in attempts {
        let recovery = git.classify_merge_recovery(
            options.repository,
            &target_branch,
            &attempt.target_before_sha,
            &attempt.source_sha,
            attempt.candidate_commit_sha.as_deref(),
        )?;
        reports.push(reconcile_attempt(options, &mut store, &attempt, recovery)?);
    }
    Ok(reports)
}

fn reconcile_attempt(
    options: &MergeRecoveryOptions<'_>,
    store: &mut Store,
    attempt: &MergeAttemptRecord,
    recovery: MergeRecovery,
) -> Result<MergeRecoveryReport, LprmError> {
    match recovery {
        MergeRecovery::NotIntegrated { target_sha } => {
            if !options.dry_run {
                store.requeue_interrupted_merge(
                    options.layout.repository_id(),
                    &RequeueMergeAttempt {
                        attempt_id: attempt.id.clone(),
                        audit_id: format!("audit_{}", uuid::Uuid::now_v7().simple()),
                        actor: options.actor.to_owned(),
                        target_before_sha: attempt.target_before_sha.clone(),
                        source_sha: attempt.source_sha.clone(),
                    },
                )?;
            }
            Ok(MergeRecoveryReport {
                attempt_id: attempt.id.clone(),
                pr_id: attempt.pr_id.clone(),
                classification: "not_integrated",
                action: if options.dry_run {
                    "would_requeue"
                } else {
                    "requeued"
                },
                observed_target_sha: target_sha,
            })
        }
        MergeRecovery::Verified(merged) => {
            let candidate_commit_sha = attempt.candidate_commit_sha.clone().ok_or_else(|| {
                LprmError::internal("Verified recovery is missing candidate evidence")
            })?;
            if !options.dry_run {
                store.complete_merge_attempt(
                    options.layout.repository_id(),
                    &CompleteMergeAttempt {
                        attempt_id: attempt.id.clone(),
                        audit_id: format!("audit_{}", uuid::Uuid::now_v7().simple()),
                        actor: options.actor.to_owned(),
                        target_before_sha: attempt.target_before_sha.clone(),
                        source_sha: attempt.source_sha.clone(),
                        candidate_commit_sha,
                        target_after_sha: merged.target_after_sha.clone(),
                    },
                )?;
            }
            Ok(MergeRecoveryReport {
                attempt_id: attempt.id.clone(),
                pr_id: attempt.pr_id.clone(),
                classification: "verified",
                action: if options.dry_run {
                    "would_complete"
                } else {
                    "completed"
                },
                observed_target_sha: merged.target_after_sha,
            })
        }
        MergeRecovery::NeedsOperatorReview { target_sha } => {
            if !options.dry_run {
                store.mark_merge_attempt_reconciling(
                    options.layout.repository_id(),
                    &attempt.id,
                    "NEEDS_OPERATOR_REVIEW",
                )?;
            }
            Ok(MergeRecoveryReport {
                attempt_id: attempt.id.clone(),
                pr_id: attempt.pr_id.clone(),
                classification: "needs_operator_review",
                action: if options.dry_run {
                    "would_mark_for_operator_review"
                } else {
                    "marked_for_operator_review"
                },
                observed_target_sha: target_sha,
            })
        }
    }
}

/// Runs at most one queued merge while retaining the repository lock through reconciliation.
///
/// # Errors
///
/// Returns a stable busy, Git, validation, stale-revision, or persistence error. Any failure after
/// an attempt starts leaves it in `reconciling` so another coordinator cannot integrate blindly.
pub fn run_once(options: &CoordinatorOptions<'_>) -> Result<Option<CoordinatorReport>, LprmError> {
    let runner = CheckRunner::new(options.max_output_bytes)?;
    options.layout.prepare()?;
    let integration_root = options.layout.repository_dir().join("integration");
    fs::create_dir_all(&integration_root).map_err(|error| {
        LprmError::internal(format!(
            "Unable to create integration worktree root: {error}"
        ))
    })?;
    let _lock = RepositoryLock::acquire(
        options.layout.lock_path(),
        options.lock_timeout,
        "queue-run-once",
    )?;
    let mut store = Store::open(options.layout.database_path())?;
    let target_branch = store
        .repository_target(options.layout.repository_id())?
        .ok_or_else(|| LprmError::git_precondition("Repository initialization is missing"))?;
    let Some(attempt) = store.start_next_merge_attempt(
        options.layout.repository_id(),
        &StartMergeAttempt {
            attempt_id: format!("attempt_{}", uuid::Uuid::now_v7().simple()),
            audit_id: format!("audit_{}", uuid::Uuid::now_v7().simple()),
            actor: options.actor.to_owned(),
        },
    )?
    else {
        return Ok(None);
    };

    let result = run_active_attempt(
        options,
        &runner,
        &mut store,
        &target_branch,
        &integration_root,
        &attempt,
    );
    result.map(Some)
}

fn run_active_attempt(
    options: &CoordinatorOptions<'_>,
    runner: &CheckRunner,
    store: &mut Store,
    target_branch: &str,
    integration_root: &Path,
    attempt: &MergeAttemptRecord,
) -> Result<CoordinatorReport, LprmError> {
    let git = Git::default();
    let candidate = git
        .create_integration_candidate(
            options.repository,
            integration_root,
            &attempt.target_before_sha,
            &attempt.source_sha,
        )
        .map_err(|problem| {
            reconcile(store, options.layout.repository_id(), &attempt.id, problem)
        })?;
    let checks = runner
        .run_required(
            options.checks,
            candidate.path(),
            &options.layout.logs_dir().join(&attempt.id),
        )
        .map_err(|problem| {
            reconcile(store, options.layout.repository_id(), &attempt.id, problem)
        })?;
    if !checks.passed {
        return Err(reconcile(
            store,
            options.layout.repository_id(),
            &attempt.id,
            LprmError::validation_failed("Integrated candidate failed required checks"),
        ));
    }
    store
        .record_candidate_ready(
            options.layout.repository_id(),
            &CandidateMergeEvidence {
                attempt_id: attempt.id.clone(),
                candidate_commit_sha: candidate.commit_sha().to_owned(),
            },
        )
        .map_err(|problem| {
            reconcile(store, options.layout.repository_id(), &attempt.id, problem)
        })?;
    let merged = git
        .guarded_merge(
            options.repository,
            target_branch,
            &attempt.target_before_sha,
            &attempt.source_sha,
            candidate.tree_sha(),
        )
        .map_err(|problem| {
            reconcile(store, options.layout.repository_id(), &attempt.id, problem)
        })?;
    let GuardedMerge::Verified(merged) = merged else {
        return Err(reconcile(
            store,
            options.layout.repository_id(),
            &attempt.id,
            LprmError::validation_failed(
                "Integrated target differs from the tested candidate and requires reconciliation",
            ),
        ));
    };
    let completed = store
        .complete_merge_attempt(
            options.layout.repository_id(),
            &CompleteMergeAttempt {
                attempt_id: attempt.id.clone(),
                audit_id: format!("audit_{}", uuid::Uuid::now_v7().simple()),
                actor: options.actor.to_owned(),
                target_before_sha: attempt.target_before_sha.clone(),
                source_sha: attempt.source_sha.clone(),
                candidate_commit_sha: candidate.commit_sha().to_owned(),
                target_after_sha: merged.target_after_sha,
            },
        )
        .map_err(|problem| {
            reconcile(store, options.layout.repository_id(), &attempt.id, problem)
        })?;
    Ok(CoordinatorReport {
        attempt: completed,
        checks,
    })
}

fn reconcile(
    store: &mut Store,
    repository_id: &str,
    attempt_id: &str,
    problem: LprmError,
) -> LprmError {
    let code = match serde_json::to_value(problem.code) {
        Ok(serde_json::Value::String(code)) => code,
        _ => "INTERNAL".to_owned(),
    };
    match store.mark_merge_attempt_reconciling(repository_id, attempt_id, &code) {
        Ok(_) => problem,
        Err(persistence) => LprmError::internal(format!(
            "{}; additionally unable to mark merge reconciliation: {}",
            problem.message, persistence.message
        )),
    }
}
