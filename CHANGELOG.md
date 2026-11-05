# Changelog

## 0.1.0

First release.

- Isolated worktrees, local PRs with immutable revisions, exact-SHA diffs, and an audit journal.
- Required checks, conflict preflight, combined-tree validation, and guarded `--no-ff` merges.
- Durable merge queue with a repository lock, stale-evidence demotion, and crash recovery.
- `pr sync`, `pr review`, `pr approve`, protected-path risk, an external reviewer adapter, and opt-in automatic admission.
- Opt-in, bounded agent events.
- Stable JSON envelope and exit codes 0 and 2 to 8.
- Installable agent skills (`lprm-contributor`, `lprm-coordinator`, `lprm-recovery`) and `scripts/install-skills.sh`.
