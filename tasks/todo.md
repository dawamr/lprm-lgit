# LPRM v1.0 Task List

Each task inherits the project Definition of Done in `tasks/plan.md`. Dependencies refer to PRD task IDs.

## Architecture contracts

### Task ADR: Record the four normative architecture decisions

**Acceptance criteria:**
- [x] ADRs define repository identity, locking, merge backend, and evidence freshness with alternatives and consequences.

**Verification:**
- [x] Decisions agree with the PRD invariants and do not promise enforcement beyond cooperative local protection.

**Dependencies:** None

## Iteration A: Foundation and worktrees

### T01: Initialize the Rust workspace

**Acceptance criteria:**
- [x] The five recommended crates compile in a workspace and `lprm --help` is runnable.

**Verification:**
- [x] `cargo check --workspace --locked`

**Dependencies:** ADR

### T02: Define CLI, JSON, errors, and diagnostics

**Acceptance criteria:**
- [x] Commands parse noninteractively, JSON envelopes are deterministic, and error categories map to exit codes 2–8.

**Verification:**
- [x] Unit and CLI contract tests pass.

**Dependencies:** T01

### T03: Discover repositories and implement doctor

**Acceptance criteria:**
- [x] Repository identity, Git version/capability, permissions, and unsupported submodules are reported without requiring a remote.

**Verification:**
- [x] Temporary-repository doctor tests pass for valid and invalid cases.

**Dependencies:** T02

### T04: Load and validate configuration

**Acceptance criteria:**
- [x] Defaults and TOML schema v1 validate target, roots, checks, merge policy, and review policy.

**Verification:**
- [x] Config round-trip and invalid-config tests pass.

**Dependencies:** T02, T03

### T05: Create isolated worktrees

**Acceptance criteria:**
- [x] A unique branch and linked worktree are created from the exact current target SHA without modifying the target checkout.

**Verification:**
- [x] Temporary-repository creation tests assert branch base and distinct paths.

**Dependencies:** T03, T04, T09

### T06: List and inspect managed worktrees

**Acceptance criteria:**
- [x] List/status reports ownership, branch, path, HEAD, dirty state, and missing registrations.

**Verification:**
- [x] Managed, dirty, and missing worktree tests pass.

**Dependencies:** T05

### T07: Clean up worktrees safely

**Acceptance criteria:**
- [x] Cleanup defaults to dry run and refuses dirty, symlinked, outside-root, unrecognized, or uniquely committed worktrees.

**Verification:**
- [x] Adversarial path and dirty-data tests pass without deleting fixtures.

**Dependencies:** T06

### T08: Detect orphaned worktrees

**Acceptance criteria:**
- [x] Recovery classifies DB-only and Git-only worktree records and proposes non-destructive repairs.

**Verification:**
- [x] Orphan fixture tests pass.

**Dependencies:** T06, T20

## Iteration B: Pull requests and validation

### T09: Add SQLite schema and migrations

**Acceptance criteria:**
- [x] Versioned migrations create all PRD entities with foreign keys, uniqueness, WAL, and busy timeout.

**Verification:**
- [x] Fresh migration, reopen, transaction rollback, and integrity tests pass.

**Dependencies:** T01, ADR

### T10: Implement local PR lifecycle queries

**Acceptance criteria:**
- [x] Create/list/show/diff/close persist immutable IDs and revision history while rejecting dirty, missing, and empty sources.

**Verification:**
- [x] Temporary Git plus SQLite lifecycle tests pass.

**Dependencies:** T09, T13

### T11: Enforce the PR state machine

**Acceptance criteria:**
- [x] Only documented transitions are allowed and terminal states cannot be reopened under the same PR ID.

**Verification:**
- [x] Table-driven transition tests cover every state pair.

**Dependencies:** T09

### T12: Persist audit and evidence fingerprints

**Acceptance criteria:**
- [x] Every state transition records actor, time, reason, exact SHAs, revision, and policy fingerprint transactionally.

**Verification:**
- [x] Audit ordering and transaction rollback tests pass.

**Dependencies:** T09, T11

### T13: Collect diffs and commits

**Acceptance criteria:**
- [x] Exact source/target SHAs, reachable commits, names, stats, and patches are collected without shell evaluation.

**Verification:**
- [x] Malicious-filename and unrelated-history tests pass.

**Dependencies:** T03

### T14: Add merge-tree conflict preflight

**Acceptance criteria:**
- [x] Exit status distinguishes clean, conflict, and Git errors while leaving all worktree indexes and files unchanged.

**Verification:**
- [x] Clean and conflicting temporary-repository tests compare pre/post status and index trees.

**Dependencies:** T03, T13

### T15: Run configured checks

**Acceptance criteria:**
- [x] Argument-array commands run with timeout, bounded stdout/stderr files, stable statuses, and no shell interpolation.

**Verification:**
- [x] Success, failure, timeout, missing executable, and hostile-argument tests pass.

**Dependencies:** T04, T09

### T16: Invalidate stale evidence

**Acceptance criteria:**
- [x] Source, target, policy, or check-config changes invalidate prior checks, reviews, approvals, and queued admission.

**Verification:**
- [x] Each fingerprint input has an independent staleness test.

**Dependencies:** T12, T15

## Iteration C: Merge coordinator MVP

### T17: Serialize a durable merge queue

**Acceptance criteria:**
- [x] Queue admission enforces gates and one repository-scoped cross-process lock prevents duplicate active merges.

**Verification:**
- [x] Two concurrent coordinator processes produce exactly one lock holder and one merge attempt.

**Dependencies:** T09, T16

### T18: Validate disposable integrated candidates

**Acceptance criteria:**
- [x] A temporary detached worktree merges exact SHAs and runs required checks on the combined tree before target mutation.

**Verification:**
- [x] A pair of individually passing but jointly failing changes is blocked.

**Dependencies:** T14, T15, T17

### T19: Perform guarded no-ff integration

**Acceptance criteria:**
- [x] The coordinator rechecks refs, merges the exact source, verifies parents/tree, and atomically records success without duplicate integration.

**Verification:**
- [x] Sequential two-PR, target-race, hook-difference, and failed-merge tests pass.

**Dependencies:** T18

### T20: Reconcile interrupted operations

**Acceptance criteria:**
- [x] `recover` compares DB evidence with Git ancestry and classifies uncertain work for operator review without destructive rollback.

**Verification:**
- [x] Failure injection before merge, during candidate work, and after Git success is idempotently recoverable.

**Dependencies:** T17, T19

### T25: Stabilize the machine contract

**Acceptance criteria:**
- [x] Every P0 command supports deterministic JSON, stable errors, and zero prompts in machine mode.

**Verification:**
- [x] Golden JSON and exit-code tests cover success and each error class.

**Dependencies:** T02, T20

### T26: Track agent identity and ownership

**Acceptance criteria:**
- [x] Worktrees, PR actions, approvals, and audit events consistently record caller identity.

**Verification:**
- [x] Ownership and separation-policy tests pass.

**Dependencies:** T05, T12

### T29: Complete the offline end-to-end matrix

**Acceptance criteria:**
- [x] All P0 acceptance criteria run in ephemeral repositories without remote, keys, or network.

**Verification:**
- [x] `cargo test --workspace --locked`

**AC to test index** (all under `crates/*/tests`, run offline in temporary repositories):

| AC | Test |
| --- | --- |
| AC-01 | `cli_contract::init_persists_external_state_and_is_idempotent_without_a_remote` |
| AC-02 | `cli_contract::worktree_create_builds_distinct_checkouts_from_the_exact_target` |
| AC-03 | `cli_contract::pull_request_create_rejects_empty_dirty_and_missing_sources` |
| AC-04 | `machine_contract::*`, `cli_contract::json_mode_*`, `machine_flow` (no prompts) |
| AC-05 | `acceptance_matrix::ac05_conflict_preflight_leaves_the_checkout_untouched` |
| AC-06 | `acceptance_matrix::ac06_failing_required_check_blocks_the_queue_and_keeps_logs` |
| AC-07 | `acceptance_matrix::ac07_*`, `acceptance_matrix::source_drift_after_enqueue_*`, `evidence_freshness` |
| AC-08 | `acceptance_matrix::ac08_concurrent_queue_runs_create_one_merge`, `merge_queue` lock test |
| AC-09 | `acceptance_matrix::ac09_older_candidate_is_revalidated_against_the_new_target_head` |
| AC-10 | `acceptance_matrix::ac10_combined_tree_is_tested_before_integration` |
| AC-11 | `machine_flow::p0_machine_workflow_reaches_one_verified_merge_without_prompts`, `coordinator` |
| AC-12 | `cli_contract::worktree_cleanup_is_dry_run_by_default_and_refuses_data_loss` |
| AC-13 | `recovery::*` (before merge, during candidate, after Git success, unmatched target) |
| AC-14 | `machine_contract::busy_and_internal_failures_keep_their_public_exit_codes`, `cli_contract::doctor_*` |
| AC-15 | Whole suite: temporary repositories, no remote, keys, or network |

**Dependencies:** T01–T20, T25, T26

### T30: Add concurrency, crash, and adversarial coverage

**Acceptance criteria:**
- [x] Race, path, symlink, hostile filename, corruption, busy DB, timeout, and crash boundaries have repeatable tests.

**Coverage map:**
- Race: `lprm-git` guarded-merge target race, `acceptance_matrix::ac08_*`, `adversarial::repeated_concurrent_queue_runs_*`.
- Path/symlink: `adversarial::cleanup_refuses_a_worktree_path_swapped_for_a_symlink`, cleanup outside-root tests.
- Hostile names: `adversarial::hostile_task_names_*`, `lprm-git` hostile filename diff, runner hostile arguments.
- Corruption: `adversarial::corrupt_database_fails_closed_*`.
- Busy DB: `adversarial::busy_database_reports_retryable_busy_*` (SQLite BUSY/LOCKED maps to exit 7).
- Timeout: `lprm-runner` timeout and descendant tests.
- Crash: `recovery::*`.

**Verification:**
- [x] Stress tests pass repeatedly on macOS (five consecutive runs of the adversarial, acceptance, and recovery suites). Linux is still unverified.

**Dependencies:** T20, T29

### T32: Finish operator and agent documentation

**Acceptance criteria:**
- [x] README, architecture, CLI contract, threat model, runbook, sample config, and agent examples match implemented behavior.

**Verification:**
- [x] Every documented command is exercised by a smoke script or test.

**Dependencies:** T25, T29

## Iteration D: Review policy and release

### T21: Model review findings

**Acceptance criteria:**
- [x] Versioned findings persist severity, location, code, message, suggestion, resolution, and verdict.

**Verification:**
- [x] Schema and serialization tests pass.

**Dependencies:** T09, T12

### T22: Enforce protected-path risk and approvals

**Acceptance criteria:**
- [x] Policy classifies changed paths and blocks unresolved high/critical risk without an eligible approval.

**Verification:**
- [x] Glob, role separation, stale approval, and override-reason tests pass.

**Dependencies:** T13, T16, T21

### T23: Add an external reviewer adapter

**Acceptance criteria:**
- [x] A subprocess receives/returns versioned JSON under time and size limits without shell or inherited secrets.

**Verification:**
- [x] Valid, malformed, oversized, failed, and timed-out adapter tests pass.

**Dependencies:** T15, T21

### T24: Implement automatic queue admission policy

**Acceptance criteria:**
- [x] Configured low-risk PRs can enqueue only after all deterministic and review gates are current.

**Verification:**
- [x] Admission matrix tests pass.

**Dependencies:** T16, T22, T23

### T27: Document Hermes and Codex workflows

**Acceptance criteria:**
- [x] Examples show isolated worktree creation through merge/recovery using only stable CLI commands.

**Verification:**
- [x] Example command sequences run against a temporary repository.

**Dependencies:** T25, T26, T32

### T28: Emit optional agent events and callbacks

**Acceptance criteria:**
- [x] Versioned events are opt-in, bounded, non-shell subprocess calls that cannot bypass coordinator gates.

**Verification:**
- [x] Delivery failure and malformed callback tests do not corrupt state.

**Dependencies:** T25, T26

### T31: Package macOS and Linux releases

**Acceptance criteria:**
- [ ] Reproducible release builds, checksums, version metadata, and installation instructions exist for both targets.

**Verification:**
- [ ] Clean-machine smoke tests pass on macOS and Linux.

**Dependencies:** T29, T30, T32

## Final release checkpoint

- [ ] All PRD acceptance criteria AC-01 through AC-16 pass.
- [ ] Formatting, clippy, full locked tests, documentation smoke tests, and dependency advisory review pass.
- [ ] Working tree is clean and atomic commits describe every delivered slice.
- [ ] Human review approves release and tag creation.
