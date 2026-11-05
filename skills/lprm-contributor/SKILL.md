---
name: lprm-contributor
description: Use when you are a coding agent working in a repository managed by LPRM (has .lprm.toml or the user mentions lgit, local PRs, or worktrees under lgit). Covers creating an isolated worktree, committing, opening and updating a local pull request, and reacting to stale or failed validation. Do not use for merging; that is the coordinator's job.
---

# LPRM contributor

You change code in your own worktree and propose it with a local PR. You never write to the
target branch (`dev` by default). A separate coordinator validates and merges.

Always pass `--json` and `--actor <your-stable-name>` (for example `codex`). Branch on the exit
code, and read `error.code` from stdout for details. `--actor` is a label for ownership and audit.

## Workflow

1. Create an isolated worktree from the exact target SHA:

   ```sh
   lgit --json --actor codex worktree create --task "short description"
   ```

   Read `data.path` and `data.branch`. Work and commit only inside that path. Do not edit the
   main checkout or another agent's worktree.

2. Commit your work inside the worktree. LPRM never stages or commits for you, and `pr create`
   refuses a dirty worktree, a missing branch, or a branch with no new commits.

3. Open the PR:

   ```sh
   lgit --json --actor codex pr create --from <branch> --title "Title" --body-file body.md
   ```

   Keep the returned `data.pr_id`.

4. If you commit more work, or the target moved, record a new revision:

   ```sh
   lgit --json --actor codex pr sync <pr-id>
   ```

5. Hand off to the coordinator. Check where the PR stands with `pr show <pr-id>`, and read the
   journal with `audit <pr-id>`:

   ```sh
   lgit --json pr show <pr-id>
   lgit --json audit <pr-id>
   ```

6. After the PR is `merged`, preview and then remove your worktree:

   ```sh
   lgit --json --actor codex worktree cleanup <id>
   lgit --json --actor codex --yes worktree cleanup <id>
   ```

   Cleanup refuses dirty, unowned, or unmerged worktrees. Never force it.

## Reacting to results

| Result | Do |
| --- | --- |
| Exit 0 | Continue |
| Exit 6 `STALE_REVISION` | Run `pr sync <pr-id>` and ask the coordinator to re-check |
| Exit 4 `CONFLICT` | Merge the target branch into your branch, commit, then `pr sync` |
| Exit 5 | Read `error.message`; fix the cause (a failed check, a missing approval, ownership) |
| Exit 7 `BUSY` | Wait a few seconds and retry the same command |
| Exit 3 | The repository is unsafe or unsupported; stop and tell the user |

## Rules

- Never run `git merge`, `git push`, or `git reset` against the target branch.
- Never put secrets in commits or in check output; logs are stored unredacted.
- Do not approve your own PR. Approvals must come from a different identity.
- If anything mentions `reconciling` or `NEEDS_OPERATOR_REVIEW`, stop and ask the user.
