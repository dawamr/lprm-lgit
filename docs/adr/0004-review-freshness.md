# ADR-0004: Bind validation and review evidence to an exact revision tuple

## Status

Accepted

## Date

2026-11-05

## Context

Checks and approvals can become misleading when source code, target history, policy, or configured commands change after evidence was recorded.

## Decision

Every check, review, and approval belongs to one immutable PR revision containing the exact source SHA, target SHA, policy fingerprint, and check-configuration fingerprint. Queue admission and merge require current evidence for the complete tuple.

When any tuple member changes, LPRM creates or selects a new revision, marks prior evidence stale for admission, and returns the PR to `ready`. Historical evidence remains queryable and is never rewritten to appear current.

## Alternatives Considered

### Bind only to source SHA

Rejected because target changes can introduce conflicts or integration failures.

### Keep approvals across policy changes

Rejected because the meaning and required authority of an approval may have changed.

### Reuse a mutable revision row

Rejected because it destroys the audit trail and makes crash recovery ambiguous.

## Consequences

- Staleness is deterministic and auditable.
- Target movement can require repeated checks even when source code is unchanged.
- Fingerprint serialization must be canonical and schema-versioned.
