# Agent skills

LPRM ships three skills in [`skills/`](../skills) that teach a coding agent the workflow, so it
does not have to rediscover the commands or the recovery rules.

| Skill | Use it for |
| --- | --- |
| `lprm-contributor` | Working in an isolated worktree, opening and syncing a PR, reacting to exit codes |
| `lprm-coordinator` | Checks, reviews, approvals, queue admission, and merging with `queue run` |
| `lprm-recovery` | Interrupted runs, `reconciling` attempts, `BUSY` that does not clear |

Each skill is a folder with a `SKILL.md` (frontmatter `name` and `description`, then instructions),
the format Claude Code, Codex, OpenCode, and other agents load from their skills directory.

## Install

```sh
scripts/install-skills.sh                    # every existing ~/.claude, ~/.codex, ~/.agents, ~/.config/opencode skills dir
scripts/install-skills.sh --dir ~/.hermes/skills   # any other agent's skills directory
scripts/install-skills.sh --link             # symlink, so pulling this repository updates the skills
scripts/install-skills.sh --dry-run          # show what would be done
scripts/install-skills.sh --uninstall        # remove the LPRM skills again
```

The script only touches folders named `lprm-*` inside the directories you select, and re-running
it replaces them. Release archives include `skills/` and the installer.

Pair `lprm-contributor` with the agent that writes code and `lprm-coordinator` with the agent that
validates and merges, and give them different `--actor` names (see [agent-workflows.md](agent-workflows.md)).

A test parses every `lgit` command in the skills with the real CLI and exercises the installer, so
the skills cannot drift from the implementation.
