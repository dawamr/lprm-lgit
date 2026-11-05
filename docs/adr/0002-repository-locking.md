# ADR-0002: Serialize coordinators with a repository-scoped advisory file lock

## Status

Accepted

## Date

2026-11-05

## Context

Two LPRM CLI processes can attempt to merge concurrently. SQLite transactions alone cannot protect Git refs and working-tree operations across the full integration sequence.

## Decision

Each repository has one lock file in LPRM's external state directory, derived from the repository identity. A coordinator acquires an exclusive OS advisory lock before inspecting or mutating queue and Git integration state, writes diagnostic holder metadata without treating it as ownership authority, and holds the lock through final Git/DB reconciliation.

Lock acquisition has a bounded timeout and returns the stable busy error. LPRM never deletes a lock merely because holder metadata appears old; the operating system releases the advisory lock when the process exits.

## Alternatives Considered

### SQLite row or transaction lock

Rejected because a long database transaction around subprocesses is fragile and does not directly serialize external Git operations.

### Lock-file existence

Rejected because crashes leave stale files and safe ownership cannot be inferred from age or PID alone.

### In-process mutex

Rejected because it cannot coordinate separate CLI processes.

## Consequences

- LPRM coordinators on one machine serialize predictably.
- External Git programs remain outside this cooperative lock and are handled through SHA race checks.
- Shared-filesystem and cross-machine correctness are outside v1 scope.
