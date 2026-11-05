# LPRM: Local Pull Request Manager

LPRM is a local-first command-line tool that gives coding agents and humans a pull-request workflow
without a Git host. Each agent works in an isolated Git worktree, proposes a local PR, passes
deterministic validation and optional review, and merges to the target branch (`dev` by default)
only through a serialized coordinator. Nothing needs a network, a remote, a daemon, or an API key.

## What you get

- Isolated task worktrees created from the exact SHA of `dev`.
- Local PRs with immutable revisions, exact-SHA diffs, and an audit journal.
- Required checks, conflict preflight, and a check on the combined merged tree before integration.
- Protected-path risk, separate-approver rules, and an optional external reviewer.
- A durable merge queue, a repository lock, and guarded `--no-ff` merges.
- Recovery after crashes at any point, without duplicate merges.
- Machine-readable JSON with stable exit codes, so agents never parse prose.

## Requirements

- macOS or Linux, Git 2.38 or newer (needs `git merge-tree --write-tree`), Rust 1.89 or newer.
- SQLite is bundled.

## Build

```sh
cargo build --release --locked
install -m 0755 target/release/lprm /usr/local/bin/lprm
lprm doctor
```

Release archives and checksums are described in [docs/install.md](docs/install.md).

## Five-minute tour

```sh
lprm init --target dev
lprm --actor codex worktree create --task "add greeting"
# ...commit inside the printed worktree path, then:
lprm --actor codex pr create --from <branch> --title "Add greeting"
lprm --actor validator pr ready <pr-id>
lprm --actor validator pr check <pr-id>
lprm --actor coordinator pr enqueue <pr-id>
lprm --actor merger queue run --once
```

Add `--json` to any command for the stable machine envelope. See [docs/cli-contract.md](docs/cli-contract.md)
for every command, and [docs/agent-workflows.md](docs/agent-workflows.md) for Hermes and Codex flows.

## Documentation

| Document | Purpose |
| --- | --- |
| [docs/install.md](docs/install.md) | Release archives, checksums, building from source |
| [docs/cli-contract.md](docs/cli-contract.md) | Commands, JSON envelope, exit codes, events |
| [docs/architecture.md](docs/architecture.md) | Crates, state machine, merge sequence, data model |
| [docs/threat-model.md](docs/threat-model.md) | Trust boundaries and what LPRM does not protect |
| [docs/runbook.md](docs/runbook.md) | Operating, recovering, and troubleshooting |
| [docs/agent-workflows.md](docs/agent-workflows.md) | End-to-end Hermes and Codex examples |
| [docs/examples/lprm.sample.toml](docs/examples/lprm.sample.toml) | Annotated sample configuration |
| [docs/adr](docs/adr) | Architecture decisions |
| [PRD_v1.0.md](PRD_v1.0.md) | Product requirements |

## Safety in one paragraph

LPRM cooperates with agents; it does not sandbox them. Anyone who can write to the same `.git`
directory can bypass it, identities are self-declared, and check commands run arbitrary code from
the repository. Read [docs/threat-model.md](docs/threat-model.md) before pointing it at untrusted
agents.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

The suite creates temporary repositories and needs no network. Task status lives in
[tasks/todo.md](tasks/todo.md).
