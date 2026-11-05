# Agent workflows: Hermes and Codex

Two roles cover the common setup. A **contributor** agent (Codex here) works in its own worktree
and proposes PRs. A **coordinator** agent (Hermes here) validates, gates, and merges. Use distinct
`--actor` names: ownership checks, approval separation, and the audit trail depend on them.
Always pass `--json`, branch on the exit code, and parse `error.code` for recovery decisions.

The runnable version of this page is [examples/agent-workflow.sh](examples/agent-workflow.sh). It
is executed by the test suite against a temporary repository.

## Codex: from task to PR

```sh
lgit --json --actor codex worktree create --task "add feature"
```

The response gives `data.path` and `data.branch`. Work and commit only inside that path:

```sh
git -C "$WORKTREE" add -A
git -C "$WORKTREE" commit -m "Add feature"
lgit --json --actor codex pr create --from "$BRANCH" --title "Add feature" --body-file body.md
```

`pr create` refuses dirty worktrees, missing branches, and branches with no new commits. It never
stages or commits for you. If the PR later needs changes, commit more work and then:

```sh
lgit --json --actor codex pr sync <pr-id>
```

## Hermes: validate, gate, merge

```sh
lgit --json --actor hermes pr ready <pr-id>
lgit --json --actor hermes pr check <pr-id>
lgit --json --actor hermes pr review <pr-id>
lgit --json --actor hermes pr enqueue <pr-id>
lgit --json --actor hermes queue run --once
```

Protected paths raise the risk to `high`. Then `pr enqueue` exits 5 until a different identity approves:

```sh
lgit --json --actor operator pr approve <pr-id> --reason "reviewed the migration"
```

To queue low-risk PRs automatically, set `policy.auto_enqueue_risks = ["low"]`; `pr check` then
reports `auto_enqueue` in its response and no separate `enqueue` call is needed.

## Decision table for agents

| Result | Do |
| --- | --- |
| Exit 0 | Continue |
| Exit 6 `STALE_REVISION` | `pr sync <id>`, then `pr check` and `pr enqueue` again |
| Exit 7 `BUSY` | Sleep briefly and retry the same command |
| Exit 4 `CONFLICT` | Merge `dev` into the branch, commit, `pr sync` |
| Exit 5 on `pr check` | Read the log paths in the response, fix, commit, `pr sync` |
| Exit 5 on `pr enqueue` | Follow `error.message`: review, approval, or a failed gate |
| `queue run` lists `invalidated` | Repeat `pr sync`, `pr check`, `pr enqueue` for those PRs |
| Anything about `reconciling` | Stop and run `lgit recover --dry-run`; ask a human if it reports `NEEDS_OPERATOR_REVIEW` |

## After a crash

Any process can be killed at any point. Restart with:

```sh
lgit --json --actor hermes recover --dry-run
lgit --json --actor hermes recover
```

Recovery completes a merge that Git already performed, or requeues one that never started. It
never merges twice.

## Notifications

Agents that prefer being told over polling can set `[events] command` in `.lprm.toml`. The command
receives one JSON line on stdin per event (`pr.queued`, `pr.merged`, `pr.invalidated`, and so on).
Events are advisory; always confirm state with `pr show` before acting on one.

## Rules of the road

- Never run `git merge` or `git push` into `dev` yourself. The coordinator is the only writer.
- Never edit another agent's worktree. Ownership checks will refuse cleanup and PR creation.
- Treat `--actor` as a label, not a credential. Pick a stable name per agent.
- Do not put secrets in check output; logs are stored unredacted.
