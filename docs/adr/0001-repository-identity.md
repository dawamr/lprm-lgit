# ADR-0001: Key repositories by canonical common Git directory

## Status

Accepted

## Date

2026-11-05

## Context

One Git repository can have several linked worktrees with different paths and per-worktree Git directories. LPRM needs one stable identity for configuration, persistence, locking, and audit records across all of them.

## Decision

Resolve repository identity with `git rev-parse --path-format=absolute --git-common-dir`, canonicalize the result, and store that canonical common-directory path as the repository key. A normal worktree path, branch name, remote URL, or directory name is never the identity.

If the path cannot be resolved or canonicalized, LPRM refuses repository-scoped mutation. Moving the repository creates a new local identity in v1; automatic identity migration is deferred.

## Alternatives Considered

### Working-tree root path

Rejected because every linked worktree has a different root.

### Remote URL

Rejected because LPRM must work without a remote and URLs can change or be shared by independent clones.

### Hash of repository contents

Rejected because it changes as commits are added and cannot identify an empty or newly initialized repository consistently.

## Consequences

- All linked worktrees share one database, lock, and queue scope.
- Symlink and relative-path aliases converge after canonicalization.
- Repository moves require explicit operator reconciliation rather than silent matching.
