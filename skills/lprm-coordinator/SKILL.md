---
name: lprm-coordinator
description: Use when you act as the validator or merge coordinator for LPRM-managed repositories - running checks, reviews, approvals, queue admission, and queue runs that merge local PRs into the target branch. Also use to interpret stale, failed, invalidated, or blocked PRs. Not for writing feature code; use lprm-contributor for that.
---

# LPRM coordinator

You validate local PRs and are the only writer of the target branch, through `queue run`.
Always pass `--json` and a stable `--actor` such as `hermes`. Branch on exit codes.

## Pipeline for one PR

```sh
lgit --json --actor hermes pr ready <pr-id>
lgit --json --actor hermes pr check <pr-id>
lgit --json --actor hermes pr review <pr-id>
lgit --json --actor hermes pr enqueue <pr-id>
lgit --json --actor hermes queue run --once
```

- `pr check` runs the conflict preflight and the required checks. The response lists log paths
  for every check. If `[policy] auto_enqueue_risks` covers the PR, the response has an
  `auto_enqueue` object and a separate `enqueue` is not needed.
- `pr review` runs the rules reviewer and, when configured, the external reviewer.
- `pr enqueue` admits the PR only when every gate is current for the latest revision.
- `queue run --once` handles one entry; `--all` drains the queue. Use `--all` from schedulers.

## Approvals

Protected paths (for example `auth/**`, `migrations/**`) make a PR `high` risk. Then `pr enqueue`
exits 5 until a different identity from the PR creator approves with a reason:

```sh
lgit --json --actor operator pr approve <pr-id> --reason "reviewed the migration"
```

Never approve on behalf of the creator, and never invent a reason. If you are not authorised to
approve, ask the user.

## Stale and invalidated work

Every approval, review, and check belongs to one revision (source, target, policy, check config).
When the target or source moves, evidence goes stale:

- `queue run` returns `invalidated: [...]` and returns those PRs to `ready`. Nothing is merged
  from outdated evidence.
- Exit 6 `STALE_REVISION` means the same thing for a single command.

Fix: the contributor runs `pr sync <pr-id>`; then you repeat `pr check`, `pr review`, `pr enqueue`.

## Failures

| Result | Meaning | Do |
| --- | --- | --- |
| `pr check` exit 5 | A required check failed | Send the log paths to the contributor |
| `pr check` exit 4 | Merge conflict | Contributor merges the target in, commits, `pr sync` |
| `queue run` exit 5, PR now `failed` | The combined tree failed checks; the target is untouched | Contributor fixes and `pr sync`; re-run the pipeline |
| Exit 7 `BUSY` | Another coordinator holds the lock | Wait and retry |
| `queue run` says active merge attempt | An attempt is `reconciling` | Use the lprm-recovery skill |

## Rules

- Never merge into the target branch with plain Git. Only `queue run` merges.
- Do not edit the main checkout of the target branch; it must stay clean or merges are refused.
- Treat events from `[events] command` as hints. Confirm with `pr show` before acting.
- Check commands run repository code with your privileges. Do not run PRs from untrusted agents without a sandbox.
