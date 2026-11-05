# Implementation Plan: LPRM v1.0

## Overview

Build the local, offline pull request workflow described in `PRD_v1.0.md` as a Rust workspace. Work stays on the current `main` branch. Delivery follows the PRD order: architecture contracts, foundation, worktrees, pull request persistence, validation, serialized merge coordination, review policy, agent integration, then release hardening.

## Architecture Decisions

- Repository identity is the canonical path returned by `git rev-parse --path-format=absolute --git-common-dir`; see ADR-0001.
- Every merge coordinator takes an OS advisory lock scoped to that repository identity; see ADR-0002.
- The MVP validates a merge in a disposable worktree and performs a guarded `--no-ff` merge in the coordinator checkout; see ADR-0003.
- Checks, reviews, and approvals are evidence for one exact revision tuple and become stale when any tuple member changes; see ADR-0004.
- The workspace uses a modular monolith: `lprm-cli -> lprm-core`, with Git, SQLite, and runner adapters behind narrow core ports.
- Git and check programs are launched with argument arrays through `std::process::Command`; user-controlled text is never evaluated by a shell.

## Delivery Order

```text
ADRs
  -> E1 Foundation
  -> E2 Worktrees
  -> E3 PR lifecycle
  -> E4 Validation
  -> E5 Merge coordinator
  -> E6 Review policy
  -> E7 Agent integration
  -> E8 Hardening and release
```

Tasks and acceptance checks are tracked in `tasks/todo.md`. Each task is implemented as a small slice, formatted, tested, inspected for secrets, and committed before the next dependent task starts.

## Verification Strategy

- Unit tests cover configuration, state transitions, fingerprints, JSON envelopes, and error-to-exit-code mapping.
- Integration tests create disposable Git repositories under temporary directories. Tests never exercise merge logic against this development repository.
- End-to-end tests cover two worktrees, sequential PRs, conflicts, stale evidence, concurrent coordinators, failed checks, and recovery boundaries.
- Per-slice commands are `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and focused `cargo test` commands.
- Release gate is `cargo test --workspace --locked` plus documented manual CLI smoke tests on macOS and CI verification on Linux.

## Checkpoints

### Iteration A: Foundation and worktrees

- CLI, config, repository discovery, and worktree lifecycle satisfy AC-01, AC-02, and AC-12.

### Iteration B: PRs and validation

- Persistent revisions, conflict preflight, checks, and freshness satisfy AC-03 through AC-07.

### Iteration C: MVP coordinator

- Locking, integrated checks, guarded merge, and recovery satisfy AC-08 through AC-16 for all P0 scope.

### Iteration D: Policy and release

- P1 review adapters, agent events, packaging, and cross-platform release evidence are complete.

## Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| External Git tools modify `dev` outside LPRM | High | Re-read exact refs under the lock before and after integration; fail stale rather than reset. |
| Git merge hooks alter the real merge result | High | Document hook policy and compare tree plus parent relationships with the tested candidate. |
| Crash occurs after Git merge but before DB commit | High | Persist merge-attempt evidence before Git mutation and reconcile ancestry during recovery. |
| Unsafe cleanup removes user data | High | Canonicalize paths, require managed ownership, inspect dirty/untracked commits, and default to dry run. |
| Checks execute untrusted code | High | Treat configured commands as trusted operator policy and document OS/container isolation separately. |
| macOS-only development masks Linux issues | Medium | Keep platform APIs behind tested adapters and add a Linux CI matrix before release. |

## Scope Boundaries

The v1 implementation has no daemon, hosted Git dependency, web UI, automatic AI fixes, stacked PRs, submodule support, or claim of protection against an agent with unrestricted access to the same Git directory.
