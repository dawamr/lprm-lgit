# Threat model

LPRM is cooperative workflow tooling for one machine. It prevents mistakes and makes them visible.
It is not a security boundary against a hostile agent.

## What LPRM protects against

| Risk | Control |
| --- | --- |
| Concurrent agents overwriting each other | One worktree and branch per task, created from the exact target SHA; the main checkout is never edited |
| Merging stale or untested results | Immutable revisions bound to source, target, policy, and check-config hashes; combined-tree checks before integration |
| Two coordinators merging at once | OS advisory lock plus a one-active-attempt database index |
| Half-finished merges after a crash | Evidence-first recovery from Git ancestry; no resets, cleans, or forced rollbacks |
| Accidental data loss | Cleanup is a dry run by default and refuses dirty, outside-root, symlinked, unowned, or unmerged worktrees |
| Shell injection through names | Git and checks run from argument arrays; task names are slugged; `--` separators are used |
| Runaway or noisy commands | Timeouts with process-group kill, bounded private logs, bounded reviewer and event payloads |
| Self-approval of risky changes | Approvals must come from a different identity than the PR creator and be tied to one revision |
| Untrusted reviewer output | Strict versioned schema, size and time limits, scrubbed environment, fail closed |

## What LPRM does not protect against

- **Same-repository write access.** An agent that can run `git` in the same `.git` directory can move `dev` directly. Protection of the target branch is cooperative. For strong isolation, run the coordinator in a repository the agents cannot write to and have them submit commits to it.
- **Identity forgery.** `--actor` and `LPRM_ACTOR` are self-declared. Separation of duties is only as strong as your control over who can invoke the CLI as whom.
- **Malicious check, review, or event commands.** They are trusted configuration from the repository and run with your privileges. Anyone who can change `.lprm.toml` on the target branch can run code. Treat that file like CI configuration and protect it with the same care.
- **Code execution from PR contents.** Running checks on an agent's branch executes the agent's code. A worktree is not a sandbox. Use a container or separate OS user if agents are untrusted.
- **Git hooks and Git configuration.** Hooks in the repository can execute during merges. LPRM compares the resulting tree and parents with the tested candidate and refuses to record a mismatch, but it does not stop the hook from running.
- **Secret leakage in logs.** Logs are size-bounded and stored with private permissions. They are not redacted, so do not print secrets from checks.
- **Disk and state tampering.** Someone with access to the state directory can edit the database or lock file. Keep it private to your user.

## Secrets

LPRM never copies `.env` files, keys, or tokens into worktrees. Check commands inherit the
environment of the process that invoked LPRM; external reviewers and event commands start with an
empty environment (`PATH` and `LANG` only). Give the coordinator only the credentials it needs.

## Failure behaviour

Unsafe or unknown conditions stop with a typed error and exit code rather than guessing:
unsupported Git, dirty target checkout, held lock, corrupt database, ambiguous Git evidence
(`reconciling`), and invalid reviewer output.
