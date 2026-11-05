# Agent identity and ownership

LPRM attributes local mutations to a validated, self-declared caller identity. Supply it with the global `--actor <identity>` option or the `LPRM_ACTOR` environment variable. An explicit option takes precedence over the environment. If neither is present, LPRM uses `local-operator`.

`lgit worktree create --agent <identity>` remains a compatibility alias. When `--actor` and `--agent` are both present, they must match. New worktrees always persist the resolved caller as `owner_agent`.

```console
LPRM_ACTOR=codex lgit --json worktree create --task "Add cache"
lgit --json --actor codex pr create --from task/add-cache-... --title "Add cache"
lgit --json --actor validator pr check <pr-id>
lgit --json --actor coordinator pr enqueue <pr-id>
lgit --json --actor coordinator queue run --once
```

PR creation and worktree cleanup reject callers whose identity differs from the persisted worktree owner. Other PR state changes, queue processing, and recovery record the actual caller in audit events. Approval persistence already requires an actor, and the core role-separation policy rejects the same identity acting as both contributor and approver.

Identity values must be non-empty, at most 128 characters, valid UTF-8, and free of surrounding whitespace or control characters.

This is cooperative attribution, not authentication. A local process can claim another identity, and agents sharing unrestricted access to the same Git common directory can bypass LPRM. Strong isolation requires separate operating-system credentials or a coordinator-owned repository, which is outside the MVP.
