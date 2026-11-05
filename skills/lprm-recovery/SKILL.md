---
name: lprm-recovery
description: Use when an LPRM run was interrupted or is blocked - a crash or kill during queue run, a repository with an active or reconciling merge attempt, BUSY errors that never clear, orphaned worktrees, or NEEDS_OPERATOR_REVIEW. Walks through safe inspection and recovery without duplicate merges.
---

# LPRM recovery

LPRM can be killed at any point without losing work or merging twice. Recovery compares the
database with Git ancestry and never resets, cleans, or rewrites refs.

## Steps

1. Look first. This changes nothing:

   ```sh
   lprm --json recover --dry-run
   lprm --json queue list
   ```

2. Read the `merge_attempts` entries. Each has a classification:

   | Classification | Meaning | What `recover` does |
   | --- | --- | --- |
   | `not_integrated` | The target never moved | Closes the attempt and requeues the PR |
   | `verified` | Git already holds the one tested merge commit | Records it as merged; no second merge |
   | `needs_operator_review` | Git evidence is ambiguous or the target moved unexpectedly | Leaves the attempt `reconciling` and reports `NEEDS_OPERATOR_REVIEW` |

3. If the plan looks right, apply it:

   ```sh
   lprm --json --actor hermes recover
   ```

4. Confirm with `lprm --json pr show <pr-id>` and `lprm --json audit <pr-id>`.

## When it says needs operator review

Stop. Do not retry, delete files, or edit the database. Show the user the attempt, the target
branch log (`git log --oneline -n 10 <target>`), and the PR's audit trail, and let them decide.
LPRM refuses new merges until the attempt is resolved, by design.

## Other symptoms

| Symptom | Action |
| --- | --- |
| Exit 7 `BUSY` that persists | Another process holds the repository lock or database. Check for a running `lprm queue run`; wait for it. Never delete the lock file. |
| `worktree_orphans` reported | A worktree exists in only one of Git or the database. Report it; do not remove it yourself. |
| Exit 8 mentioning the database | State is corrupt or unreadable. Stop and tell the user; see the runbook. |
