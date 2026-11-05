# Runbook

## First-time setup

```sh
lprm doctor
lprm init --target dev
lprm config show
```

`doctor` must report Git 2.38 or newer and a working `merge-tree --write-tree`. `init` is
idempotent and never writes into your checkout. The target branch must exist and have a commit.
Keep the main checkout of `dev` clean: the coordinator refuses to merge into a dirty target.

## Daily operation

1. Agent: `lprm --actor <agent> worktree create --task "..."`, commit, then `pr create`.
2. Validator: `pr ready`, then `pr check`.
3. Optional: `pr review`, and `pr approve --reason "..."` by a second identity for risky changes.
4. Coordinator: `pr enqueue`, then `queue run --once` (or `--all` from a scheduler).
5. After merge, `worktree cleanup <id>` (dry run), then `lprm --yes worktree cleanup <id>`.

## Reading state

```sh
lprm --json pr list --status failed
lprm --json pr show <pr-id>
lprm --json audit <pr-id>
lprm --json queue list
lprm --json worktree list
```

Check logs live under `<state>/.../logs/`; the `suite` object returned by `pr check` lists each log path.

## Symptom guide

| Symptom | Meaning | Action |
| --- | --- | --- |
| Exit 6 `STALE_REVISION` | `dev`, the source branch, or policy moved after validation | `pr sync <id>`, then `pr check`, then `pr enqueue` |
| `queue run` returns `invalidated: [...]` | Queued PRs were demoted to `ready` because the target or source moved | Same as above for each listed PR |
| Exit 7 `BUSY` | Another coordinator holds the lock, or the database is busy | Wait and retry; it is `retryable` |
| Exit 5 on `pr enqueue`, message names a risk | Approval or review gate is open | `pr review`, then an approval from a different identity with `--reason` |
| PR is `failed` after `queue run` | The combined tree failed the required checks; the target is untouched | Fix on the branch, `pr sync`, `pr check`, `pr enqueue` |
| PR is `failed` after `pr check` | A required check failed | Read the stdout and stderr logs, fix, `pr sync` |
| PR is `conflict` | Merge preflight found a conflict | Rebase or merge `dev` into the branch, commit, `pr sync` |
| `queue run` exits 7 with "active merge attempt" | An attempt is `reconciling` | Run `lprm recover --dry-run`, then `lprm recover` |
| Exit 8 `INTERNAL` mentioning the database | Corrupt or unreadable state | See the next section |

## Recovering from a crash

Kill the process anywhere and nothing is lost. Run:

```sh
lprm recover --dry-run
lprm recover
```

Recovery compares each active attempt with Git ancestry and never resets, cleans, or rewrites refs:

- Target unchanged: the attempt is closed and the PR returns to `queued` to be retried.
- Exactly one merge commit with the tested parents and tree exists: it is recorded as merged, with no second merge.
- Anything else: the attempt stays `reconciling` and `NEEDS_OPERATOR_REVIEW` is reported. Inspect `git log dev`, decide what happened, and resolve it by hand. LPRM will not merge again until you do.

`recover --dry-run` also lists worktrees known to the database but missing from Git, and Git
worktrees that LPRM does not manage.

## Corrupt state

The database holds workflow metadata; Git holds the code, so merged history in `dev` is never at
risk. If the database is unusable, every command fails with exit 8 and touches nothing else.

1. Stop all coordinators.
2. Restore the state directory from your latest backup if you have one.
3. Without a backup, move the state directory aside and run `lprm init`. Open PRs and worktree
   registrations are not reconstructed (there is no command to adopt an existing worktree yet), so
   rebuild the work: create new worktrees and cherry-pick the commits from the old branches.
4. Keep the old directory; it is the only record of the previous audit trail.

## Maintenance

- Back up the state directory while no coordinator runs.
- Rotate old check logs by deleting `logs/` subdirectories of merged PRs.
- Upgrade by replacing the binary. Migrations run on first open and are idempotent.
