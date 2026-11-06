# CLI contract

This is the stable machine interface. Version 1 of the envelope, error codes, and exit codes will
not change incompatibly; new fields are only ever added.

## Global options

| Option | Meaning |
| --- | --- |
| `--json` | Emit one single-line JSON envelope on stdout. Logs go to stderr. Never prompts. |
| `--output human\|json` | Same as above, selected by value. |
| `--actor <name>` | Self-declared caller identity. Falls back to `LPRM_ACTOR`, then `local-operator`. |
| `--yes` | Confirms destructive operations such as `worktree cleanup`. |

`LPRM_STATE_HOME` overrides where LPRM keeps its state (database, logs, lock). Default:
`~/Library/Application Support/lprm` on macOS, `$XDG_STATE_HOME/lprm` or `~/.local/state/lprm` on Linux.

## Envelope

```json
{"schema_version":1,"ok":true,"data":{},"error":null}
{"schema_version":1,"ok":false,"data":null,"error":{"code":"STALE_REVISION","message":"...","details":{},"retryable":true}}
```

## Exit codes and error codes

| Exit | `error.code` | Meaning |
| --- | --- | --- |
| 0 | none | Success |
| 2 | `INVALID_ARGUMENT` | Bad arguments, unknown ID, or invalid configuration |
| 3 | `GIT_PRECONDITION` | Git or repository state is unsafe or unsupported |
| 4 | `CONFLICT` | The PR conflicts with the target branch |
| 5 | `VALIDATION_FAILED` | A check, review, approval, ownership, or state-machine rule failed |
| 6 | `STALE_REVISION` | Evidence no longer matches the source, target, or policy; run `pr sync` |
| 7 | `BUSY` | Repository lock or database is held; retry later (`retryable: true`) |
| 8 | `INTERNAL` | Persistence or unexpected failure |

## Commands

```sh
lgit init --target dev
lgit doctor
lgit config show
lgit worktree create --task "short task description"
lgit worktree list
lgit worktree status <id>
lgit worktree cleanup <id>
lgit --yes worktree cleanup <id>
lgit pr create --from <branch> --title "Title" --body-file body.md
lgit pr list --status ready
lgit pr show <pr-id>
lgit pr diff <pr-id> --stat
lgit pr ready <pr-id>
lgit pr sync <pr-id>
lgit pr check <pr-id>
lgit pr review <pr-id>
lgit pr approve <pr-id> --reason "reviewed the auth change"
lgit pr enqueue <pr-id>
lgit pr close <pr-id> --reason "superseded"
lgit queue list
lgit queue run --once
lgit queue run --all
lgit audit <pr-id>
lgit recover --dry-run
lgit recover
```

| Command | Effect | `data` highlights |
| --- | --- | --- |
| `init` | Registers the repository and pins the target branch SHA. Idempotent. | repository ID, target |
| `doctor` | Verifies Git version, `merge-tree --write-tree`, and repository shape. | capabilities |
| `config show` | Prints the validated effective configuration. | config |
| `worktree create` | New branch `lprm/<id>-<slug>` and worktree from the exact target SHA; entries of `worktree.link_dependencies` (default `vendors`, `node_modules`) that exist in the source checkout are symlinked in. | `id`, `branch`, `path`, `target_sha`, `linked_dependencies` |
| `worktree list` / `status` | Registry merged with live Git state (clean, dirty, missing). | observations |
| `worktree cleanup` | Dry run by default; `--yes` removes only clean, owned, in-root, fully merged worktrees. | `dry_run`, refusal reasons |
| `pr create` | Snapshots source and target SHAs as revision 1. Refuses dirty, missing, or empty sources. | `pr_id`, `revisions` |
| `pr list` / `show` / `diff` | Read-only views. `diff` uses the stored revision SHAs. | records, patch or stats |
| `pr ready` | `draft` to `ready`. | status |
| `pr sync` | Appends a new immutable revision for the current source, target, and policy, and returns the PR to `ready`. | `revisions` |
| `pr check` | Conflict preflight, then required checks; persists evidence and moves to `approved`, `failed`, or `conflict`. | `suite`, `auto_enqueue` |
| `pr review` | Runs the rules reviewer (and the external adapter when configured) and stores the reports. | `reviews`, `assessment` |
| `pr approve` | Records an approval from someone other than the PR's creator. `--reason` is required for risky PRs. | `assessment` |
| `pr enqueue` | Admits an approved PR when every gate is current. | queue record |
| `pr close` | Closes an open PR. | status |
| `queue list` | Durable queue entries. | entries |
| `queue run` | One coordinator pass (`--once`) or until the queue is empty (`--all`). | `processed`, `attempts`, `invalidated` |
| `audit` | Ordered journal for a PR, with actor and exact SHAs. | events |
| `recover` | Reconciles interrupted merges from Git evidence. `--dry-run` only reports. | `merge_attempts`, `worktree_orphans` |

## Admission gates

`pr enqueue` succeeds only when all of these hold for the latest revision:

1. Status is `approved`, produced by a passing `pr check` for the same source, target, policy, and check configuration.
2. The merge preflight is conflict free.
3. Review gate: no unresolved `high` or `critical` finding and no `changes_requested` verdict, unless an eligible approval exists. In `external` mode a review from the adapter must exist.
4. Approval gate: if the assessed risk is in `review.require_approval_for_risk`, an approval from an identity other than the PR creator exists.

Reviews and approvals belong to one revision. A new revision starts with none.

## Queue behaviour

`queue run` holds the repository lock throughout. Before starting an attempt it demotes queued PRs
whose validated target head or source head moved: the entry becomes `stale`, the PR returns to
`ready`, and the PR ID appears in `invalidated`. Run `pr sync`, `pr check`, and `pr enqueue` again.

For each attempt the coordinator builds a disposable detached worktree from the exact target SHA,
merges the exact source SHA with `--no-ff`, and runs the required checks there. A rejected
candidate marks the PR `failed` and leaves the target untouched. A passing candidate is merged on
the real target with a guarded compare-and-swap, and the resulting tree and parents must match the
tested candidate. Any other failure leaves the attempt `reconciling` for `lgit recover`.

## Review adapter protocol

With `review.mode = "external"`, `pr review` starts `review.command` without a shell, in the repository root,
with an empty environment, and writes one request to stdin:

```json
{"schema_version":1,"pr_id":"...","title":"...","revision":{"id":"...","source_sha":"...","target_sha":"..."},"files":[{"path":"src/a.rs","old_path":null,"added_lines":3,"deleted_lines":1}]}
```

It must print one report on stdout and exit 0:

```json
{"schema_version":1,"reviewer":{"kind":"external","name":"my-reviewer"},"verdict":"pass","findings":[{"severity":"high","file":"src/a.rs","line":10,"code":"SECRET","message":"hard-coded token","suggestion":"read it from the environment","resolved":false}]}
```

`verdict` is `pass` or `changes_requested`; `severity` is `info`, `low`, `medium`, `high`, or `critical`.
Timeouts, oversized payloads, non-zero exits, and malformed or unknown-field reports fail closed
with exit 5 and store nothing.

## Events

When `[events] command` is set, LPRM writes one JSON event to that command's stdin after each
relevant command. Delivery is best effort and one way: output is discarded, failures are logged to
stderr, and events can never change a decision.

```json
{"schema_version":1,"event":"pr.merged","actor":"merger","pr_id":"...","status":null,"detail":null}
```

Events: `pr.created`, `pr.ready`, `pr.revised`, `pr.checked`, `pr.check_failed`, `pr.reviewed`,
`pr.approved`, `pr.queued`, `pr.closed`, `pr.merged`, `pr.invalidated`.
