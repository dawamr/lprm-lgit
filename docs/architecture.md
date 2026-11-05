# Architecture

## Crates

| Crate | Responsibility |
| --- | --- |
| `lprm-core` | Pure domain: config schema, PR state machine, evidence fingerprints, review findings and policy, identities, error codes, JSON envelope. No I/O beyond reading `.lprm.toml`. |
| `lprm-git` | Every Git interaction through argument arrays: discovery, worktrees, exact-SHA diffs, `merge-tree` preflight, disposable candidates, guarded merges, recovery classification. |
| `lprm-store` | SQLite (WAL) persistence, migrations, transactions, the repository lock, and the audit journal. |
| `lprm-runner` | Check runner with timeouts, process-group kill, bounded private logs, and the JSON subprocess adapter used by reviewers and events. |
| `lprm-cli` | Argument parsing, command handlers, the coordinator, recovery, events, and output. |

## State outside the repository

LPRM keeps its database, logs, and lock under the state directory (see the CLI contract), keyed by
the repository's canonical common Git directory ([ADR-0001](adr/0001-repository-identity.md)).
Nothing LPRM-specific is written into your checkout except worktrees under the configured root.

## PR state machine

```
draft -> ready -> checking -> approved -> queued -> merging -> merged
                     |            |          |          |
        changes_requested  conflict  blocked  failed     +--> failed (candidate rejected pre-merge)
```

- `checking` can end in `approved`, `conflict`, `blocked`, `failed`, or `changes_requested`.
- `approved` and `queued` return to `ready` when source, target, or policy evidence goes stale.
- `merging` becomes `merged`, or `failed` when the disposable candidate is rejected before the target is touched. Interrupted merges are resolved by `recover` from Git evidence.
- `merged` and `closed` are terminal. Historical evidence is never mutated: a changed PR gets a new revision.

## Revisions and freshness

Each revision is immutable and carries four inputs: source SHA, target SHA, policy hash, and
check-config hash ([ADR-0004](adr/0004-review-freshness.md)). Check runs, reviews, and approvals
reference a revision ID, so a new revision starts with no evidence. Every transition re-reads the
repository and rejects mismatches with `STALE_REVISION`.

## Merge sequence

1. Take the repository lock ([ADR-0002](adr/0002-repository-locking.md)).
2. Demote queued PRs whose validated target or source head moved.
3. Claim the next queue entry and create a `preparing` attempt in one transaction.
4. Create a detached worktree from the exact target SHA, merge the exact source SHA with `--no-ff`, and run the required checks on that combined tree.
5. Record the candidate, then merge on the real target only if the target ref still equals the tested SHA ([ADR-0003](adr/0003-integration-merge-backend.md)).
6. Verify the resulting tree and parents equal the tested candidate, then finish attempt, queue entry, PR, and audit event in one transaction.
7. Release the lock. Any failure after step 3 leaves the attempt `reconciling`, which blocks further merges until `recover` classifies it.

## Data model

SQLite tables: `repositories`, `worktrees`, `pull_requests`, `pr_revisions`, `check_runs`,
`review_runs`, `approvals`, `merge_queue`, `merge_attempts`, `audit_events`. Foreign keys are on,
a partial unique index permits one active attempt per repository, and every multi-row change runs
in an `IMMEDIATE` transaction. SQLite lock contention surfaces as `BUSY`.

## Identity

`--actor` is attribution, not authentication ([agent-identity.md](agent-identity.md)). Worktrees
record their owner, PR creation and cleanup require the owner, and approvals require a different
identity from the PR creator.
