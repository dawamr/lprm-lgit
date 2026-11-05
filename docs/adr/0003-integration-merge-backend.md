# ADR-0003: Test in a disposable worktree and merge in the coordinator checkout

## Status

Accepted

## Date

2026-11-05

## Context

LPRM must test the combined source and target tree before changing `dev`, while keeping a checked-out target branch and its index consistent. A `--no-ff` merge created twice can have different commit IDs even when its content is identical.

## Decision

Under the repository lock, LPRM creates a disposable detached integration worktree at the exact target SHA, merges the exact source SHA with controlled identity, and runs integration checks there. It then re-reads the target ref and performs a guarded `git merge --no-ff --no-edit <source-sha>` in the clean coordinator checkout.

The real merge is accepted only when its tree matches the tested candidate and its parents are the approved target and source SHAs. LPRM records evidence before mutation so recovery can identify a Git-success/DB-failure boundary without repeating the merge.

## Alternatives Considered

### Update the checked-out branch from another worktree

Rejected because Git protects checked-out branches and the target worktree index could become inconsistent.

### `merge-tree`, `commit-tree`, and compare-and-swap `update-ref`

Deferred because safely synchronizing a checked-out target requires a separate coordinator-owned repository or stronger worktree handling.

### Test only the source branch

Rejected because independently valid changes can fail when combined with the current target.

## Consequences

- Integrated content is tested before target mutation.
- Hooks and environment differences can still alter the real merge, so tree and parent verification is mandatory.
- The coordinator checkout must have `dev` checked out and be clean for the MVP backend.
