# Changelog

## Unreleased

- `[worktree] setup_commands` run in each new worktree (for example `composer install`), with a timeout and logs; a failure keeps the worktree and returns an error. The report gains `setup`.
- `[worktree] clone_dependencies` copies dependency directories (nested paths allowed) with a copy-on-write clone, so worktrees are isolated and fast. The report gains `cloned_dependencies`.
- Breaking: `link_dependencies` now defaults to empty. Symlinked `vendor/` makes Composer's autoloader resolve `app/` to the source checkout. Set it explicitly to keep the old behavior.

## 0.1.2

- Worktree directories and branches are now named `wt-<id>-<slug>`. IDs often start with a digit, which made Pest reject the path as an invalid namespace and fail the test gate. Existing worktrees keep their old names.

## 0.1.1

- Release archives and the installer now cover Intel Macs (`x86_64-apple-darwin`).
- Weekly and lockfile-triggered `cargo audit` workflow.

## 0.1.0

First release.

- Isolated worktrees, local PRs with immutable revisions, exact-SHA diffs, and an audit journal.
- Required checks, conflict preflight, combined-tree validation, and guarded `--no-ff` merges.
- Durable merge queue with a repository lock, stale-evidence demotion, and crash recovery.
- `pr sync`, `pr review`, `pr approve`, protected-path risk, an external reviewer adapter, and opt-in automatic admission.
- Opt-in, bounded agent events.
- `scripts/install.sh` installs or updates `lgit` from a release with checksum verification; the release workflow attaches archives and the installer to a draft release.
- `worktree create` symlinks `vendors/*` and `node_modules/*` from the source checkout when present (`[worktree] link_dependencies`).
- Stable JSON envelope and exit codes 0 and 2 to 8.
- Installable agent skills (`lprm-contributor`, `lprm-coordinator`, `lprm-recovery`) and `scripts/install-skills.sh`.
