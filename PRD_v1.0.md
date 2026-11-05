# Product Requirements Document — LPRM

**Product:** Local Pull Request Manager (LPRM)
**Version:** 1.0 — Implementation Specification
**Date:** 2026-11-05
**Language:** Rust
**Status:** Ready for implementation planning
**Target:** macOS and Linux; entirely local/offline

## 1. Executive summary

LPRM is a local-first, CLI-driven pull-request workflow for projects managed by autonomous or human coding agents. The repository uses a single target integration branch, `dev` by default. Each agent works in an independent feature branch and linked Git worktree, creates a local pull request, passes deterministic validation and optional reviews, and merges via a serialized coordinator only. A Git hosting service, network connection, daemon, or remote is not required.

**Product invariant:** Only the merge coordinator may perform LPRM-managed integration into `dev`. This is a workflow rule; strong protection against an untrusted agent requires OS/filesystem permissions or separated repositories, not merely CLI rules or Git hooks.

## 2. Problem

Multiple agents can modify independent worktrees concurrently, but uncontrolled merges produce conflicts, stale approvals, overwritten work, untested integrated results, inconsistent audit trails, and difficult recovery. Git provides branches, worktrees and merges, but not local PR states, approval policy, queueing, or persistent review history.

## 3. Goals and non-goals

### Goals
1. Manage isolated agent worktrees created from a known `dev` commit.
2. Persist local PRs, revisions, checks, reviews, queue entries, and audit events.
3. Execute repeatable checks on exact source/target commit SHAs and invalidate stale results.
4. Prevent two LPRM coordinators from merging into the same target simultaneously.
5. Test the **integrated result**, not only the feature branch.
6. Make merge success, failure, crashes, and cleanup recoverable and auditable.
7. Expose stable JSON output, deterministic exit codes, and non-interactive commands.
8. Work without any Git host, network connection, or always-on service.

### Non-goals for v1
- GitHub/GitLab sync or remote review UI.
- Cross-machine distributed queue or shared filesystem coordination.
- Web UI, daemon, or always-running scheduler.
- General-purpose CI/CD replacement or production deployment.
- Automated AI code fixes, autonomous conflict resolution, or untrusted sandbox creation.
- Cryptographically enforced prevention of `git` writes by a process with full repo permission.
- Multi-target PRs, stacked PRs, submodule orchestration (detect and block unsupported cases in MVP).

## 4. Personas

- **Coding agent:** gets an isolated task worktree; commits changes; proposes PR; reads structured status and review feedback.
- **Review agent:** reads diff/metadata; emits structured findings; never gets implicit merge authority.
- **Merge coordinator:** exclusive owner of integrations; enforces freshness, policies, queue, and recovery.
- **Developer/operator:** inspects, overrides with justification, approves high-risk PRs, and repairs repository state.

## 5. Guiding architecture decisions

- Rust stable, Cargo workspace, modular monolith.
- Native Git CLI as primary Git backend, invoked safely through `std::process::Command` with explicit args; **never** run user-controlled text through a shell.
- SQLite with migrations, transactions and `busy_timeout`; WAL mode appropriate for local use.
- Structured JSON CLI output and human-readable text output, with redaction for credentials.
- Data is keyed by stable repository identity (canonical common Git directory) and unique PR IDs, not directory names alone.
- Keep state outside source-controlled worktree under configurable user data location (e.g. `${XDG_STATE_HOME:-~/.local/state}/lprm/` on Linux and `~/Library/Application Support/lprm/` on macOS); store a discovery marker/config under `.lprm.toml` only by opt-in.
- The central merge coordinator is a synchronous CLI process, not a daemon. Queue entries can be processed by explicit `lprm queue run` or a calling agent; automatic background scheduling is deferred.
- Prefer `git merge --no-ff` for understandable merge commits; use a temporary integration worktree and the latest target SHA to validate the actual integrated result.
- Supported baseline: Git version verified by `lprm doctor`; minimum Git feature support must include `git merge-tree --write-tree` (Git 2.38+ recommended baseline; validate exact capabilities at runtime).

### Crate suggestions (verify compatible current versions before pinning)

- `clap` — CLI parsing, `serde` / `serde_json` — JSON, `toml` — configuration.
- `rusqlite` with bundled SQLite or compatible system SQLite — persistence.
- `thiserror` and `anyhow` (anyhow at binary boundary) — typed errors.
- `tracing`, `tracing-subscriber` — diagnostic events.
- `fs2` or an equivalent cross-platform file-locking library — repository locks.
- `tempfile` — integration and test directories.
- `uuid` or ULID crate — opaque PR/run identifiers.
- `assert_cmd` and integration tests with temporary Git repositories.

Crates are candidates, not a mandate to chase newest releases. Record versions and advisories in `Cargo.lock`.

## 6. Core workflow

```
                         ┌──────────┐
                         │ dev HEAD │
                         └────┬─────┘
            ┌─────────────────┼─────────────────┐
            ▼                 ▼                 ▼
        worktree A        worktree B         worktree C
         branch A          branch B           branch C
            │                 │                 │
            ▼                 ▼                 ▼
          PR #1             PR #2             PR #3
            └─────────────────┼─────────────────┘
                              ▼
                   Validation / Review Gates
                              │
                              ▼
                    Serialized Merge Queue
                              │
                              ▼
                 Integration worktree & tests
                              │
                              ▼
                       dev (no-ff merge)
```

### User journey

1. `lprm init --target dev` verifies repo, Git version, target, and config.
2. `lprm worktree create --task "add user roles"` creates a unique branch from current `dev` and an isolated directory. Default branch: `lprm/<short-id>-<slug>`.
3. Agent edits and commits. `lprm pr create --from <branch> --title ...` captures immutable source and target SHAs at PR revision creation. Creating a PR from a dirty worktree must fail with actionable guidance.
4. `lprm pr check <id>` collects diff, runs conflict preflight, configured checks, and creates check runs tied to exact SHAs.
5. `lprm pr review <id>` applies deterministic rules and optionally a pluggable AI review adapter. Review results are bound to exact PR revision and SHA pair.
6. `lprm pr approve <id>` records eligible approval; the system requires separate roles where enabled.
7. `lprm pr enqueue <id>` admits only valid PRs. `lprm queue run --once` takes the repo lock, re-reads target/source, verifies freshness, performs integration in a disposable worktree, runs integration checks, and merges using one supported backend.
8. Successful merge records target before/after, merge commit, run logs, and state `merged` transactionally. Cleanup is a separate safe operation.
9. If `dev` changed, all affected pending checks/approvals become stale; coordinator requeues for revalidation, never silently merges outdated results.

## 7. State machine

States: `draft`, `ready`, `checking`, `changes_requested`, `approved`, `queued`, `merging`, `merged`, `conflict`, `blocked`, `failed`, `closed`.

Allowed main path: `draft -> ready -> checking -> approved -> queued -> merging -> merged`.

- `checking -> changes_requested|conflict|blocked|failed|approved`.
- `changes_requested|conflict|blocked|failed -> ready` after source updates, configuration repairs or explicit retry.
- `approved|queued -> ready` when source SHA or target SHA changes, or required policy changed; stale checks and approvals are invalidated.
- `draft|ready|changes_requested|conflict|blocked|failed -> closed` by authorized caller.
- `merging` is recovery-sensitive; on startup inspect Git refs and event journal before transitioning.
- `merged` and `closed` are terminal for their PR ID; new changes need a new PR or new revision before merge, never mutate historical evidence.

**Revision semantics:** Each source HEAD update creates a new `pr_revision`. `check_runs` and `reviews` reference `revision_id`, `source_sha`, `target_sha`, and policy fingerprint. The current PR's display status is derived from the most recent revision and valid evidence.

## 8. CLI specification

Human-readable by default; machine JSON through `--json` or `--output json`; non-interactive mode through `--yes` where safe. Prompts are prohibited when `--json` is set unless explicitly allowed.

```
lprm init --target dev
lprm doctor [--json]
lprm config show
lprm worktree create --task "Feature name" [--agent hermes] [--json]
lprm worktree list [--json]
lprm worktree status <id>
lprm worktree cleanup <id> [--dry-run]
lprm pr create --from <branch> --title "..." [--body-file path] [--json]
lprm pr list [--status ready] [--json]
lprm pr show <id> [--json]
lprm pr diff <id> [--stat]
lprm pr ready <id>
lprm pr check <id> [--json]
lprm pr review <id> [--json]
lprm pr approve <id> [--reason "..."]
lprm pr enqueue <id>
lprm pr close <id> [--reason "..."]
lprm queue list [--json]
lprm queue run --once [--json]
lprm queue run --all [--json]
lprm audit <id> [--json]
lprm recover [--dry-run] [--json]
```

Avoid promise that `lprm pr merge` immediately merges independent of queue. Optionally alias to enqueue + run for single-user convenience, while retaining all gates.

### JSON output contract

```
{
  "schema_version": 1,
  "ok": true,
  "data": {
    "pr_id": "01...",
    "status": "ready",
    "source_branch": "lprm/01...-user-roles",
    "target_branch": "dev",
    "source_sha": "...",
    "target_sha": "..."
  },
  "error": null
}
```

Error shape: `{"code":"STALE_TARGET","message":"...","details":{},"retryable":true}`. Standard exit codes: `0` success, `2` invalid arguments/config, `3` Git/repo precondition, `4` conflict, `5` validation/review failure, `6` stale revision, `7` lock/busy, `8` persistence/internal failure. Use distinct errors rather than leaking raw Git stdout as machine output.

## 9. Configuration example

```toml
schema_version = 1
[repository]
target_branch = "dev"
worktree_root = "../.lprm-worktrees"
require_clean_target = true

[merge]
strategy = "no-ff"
serialize = true
revalidate_on_target_change = true
cleanup_after_merge = false
allow_fast_forward = false

[checks]
timeout_seconds = 900
required = ["format", "test"]

[checks.commands]
format = ["cargo", "fmt", "--all", "--", "--check"]
test = ["cargo", "test", "--workspace", "--locked"]

[review]
mode = "rules" # rules | external
require_approval_for_risk = ["high", "critical"]

[policy]
protected_paths = ["migrations/**", ".github/**", "infra/**", "auth/**"]
```

**Security:** check commands are trusted repo configuration, not automatically secure. Executing code from an untrusted PR is executing untrusted code. Run agent-authored code and checks in a constrained OS/container environment if agents are untrusted. Avoid passing secrets to check subprocesses.

## 10. Core domain and SQLite schema

Core entities:

- `repositories(id, common_git_dir, target_branch, created_at, config_hash)`
- `worktrees(id, repository_id, branch, absolute_path, owner_agent, created_at, cleaned_at)`
- `pull_requests(id, repository_id, worktree_id, title, body, source_branch, target_branch, status, created_at, updated_at, merged_at)`
- `pr_revisions(id, pr_id, revision_number, source_sha, target_sha, policy_hash, created_at)`
- `check_runs(id, revision_id, name, status, exit_code, duration_ms, stdout_path, stderr_path, started_at, finished_at)`
- `review_runs(id, revision_id, reviewer_kind, verdict, findings_json, created_at)`
- `approvals(id, revision_id, actor, kind, reason, created_at)`
- `merge_queue(id, pr_id, revision_id, priority, status, enqueued_at, started_at, finished_at)`
- `merge_attempts(id, queue_id, target_before_sha, source_sha, candidate_commit_sha, target_after_sha, state, error_code, started_at, finished_at)`
- `audit_events(id, repository_id, pr_id, event_type, actor, payload_json, created_at)`

Constraints: UNIQUE path/branch for active worktrees per repository; exactly one active merge attempt per repository; PR ID immutable; FKs enabled; timestamp in UTC ISO8601/RFC3339; schema migrations versioned; integrity and recovery tests.

SQLite is **not** authoritative for Git refs. After crash, reconcile DB and Git; never blindly rerun merge based only on a stale DB flag.

## 11. Git workflow and merge safety

### Preflight
- Resolve symbolic branch refs and fully qualified SHAs using Git CLI; reject missing, detached, unsafe or unrelated source by default.
- Verify clean source and target working trees (target may be un-checked-out only in an isolated integration repo design).
- Use `git merge-tree --write-tree <target-sha> <source-sha>` and interpret exit status 0=clean, 1=conflict, other=error; do not parse human-readable output as authoritative conflict structure.
- Detect symbolic ref races between preflight and merge.

### MVP merge implementation: serialized checkout merge

Use the main checkout of `dev` as coordinator-owned integration location and a separate temp integration worktree for tests. This avoids checkout/ref inconsistency and Git protections on updating a checked-out branch from elsewhere.

1. Acquire repository-level **cross-process** exclusive lock with timeout; include holder information and avoid unsafe stale-lock deletion.
2. Verify `dev` checked out in coordinator integration checkout, all worktrees clean as required, and exact source SHA.
3. Read `target_before_sha`, compare with check-run target SHA, policy hash, and required approvals.
4. Build a disposable integration worktree from `target_before_sha`, merge source SHA with `git merge --no-ff --no-edit` (with controlled author identity) and run integration checks on the combined tree.
5. Re-read `dev` SHA; if changed, discard candidate, mark stale and retry from validation. Keep merge lock for atomicity against other LPRM instances.
6. Merge the exact source SHA into the coordinator's checked-out clean `dev` using `git merge --no-ff --no-edit <source-sha>`, with managed commit identity and hooks policy documented.
7. Verify resulting tree/parents/content correspond to the tested candidate; if not, treat as failed and require revalidation (e.g. hooks, configuration, environment, timestamp-induced differences). A stronger alternative is to install the exact pretested merge commit via a controlled ref/checkout integration backend; defer until tested.
8. Record `target_after_sha` and merge metadata. If DB writing fails after Git merge, recover by inspecting `dev` commit ancestry and journal, without merging again.
9. Never auto-reset an unrelated user commit or use `git clean -fdx` without explicit operator approval.

**Important:** `--no-ff` merge creates a new commit whose SHA can differ between sandbox and actual merge, even if the tree is the same. Validate exact tree object and parent relationships (plus approved source/target SHAs), not just candidate commit SHA.

### Alternative advanced backend

Git low-level `merge-tree` + `commit-tree` + compare-and-swap `update-ref` can reduce checkout mutation, but when `dev` is checked out elsewhere the worktree/index can become inconsistent and Git might prohibit branch updates. Do not implement this backend in MVP without a separate bare integration repository or safe working-tree synchronization strategy and complete concurrency tests.

### Multi-process limitation

Advisory locking only serializes LPRM processes. External `git commit`, `git merge`, IDE tools or agents can change `dev` outside LPRM. Detect by SHA before/after; to enforce protection, use a separate coordinator-controlled repository/OS permission boundary. Add standalone integration repository mode in a later release.

## 12. Validation and review

Required gates:
- Source has committed changes and diverges from `dev` with at least one reachable commit not in `dev`.
- Preflight conflict free.
- All configured check commands exit 0 within timeout, logs stored safely.
- Integration checks pass on merged candidate.
- Latest source, target, policy and check config fingerprints match the revision being merged.
- Required review findings resolved; required approvals present and authorized.
- Working checkout clean and target ref unchanged.

Review findings: `severity` (info/low/medium/high/critical), `file`, `line?`, `code`, `message`, `suggestion?`, `resolved`. Merge blocked by unresolved high/critical findings under policy. AI review is advisory unless configured as a hard gate, and is never a replacement for deterministic tests.

External reviewer adapter for P1: subprocess receives request path or JSON on stdin, outputs JSON result with schema_version, reviewer identity, verdict, findings; controlled timeout and max payload; no shell, secret-free. Plugins/remote providers optional, never required for offline core.

## 13. Security and trust boundaries

- No arbitrary shell interpolation; Git args array only; use `--` separators where relevant.
- Verify all managed paths are under configured worktree root after canonicalization; refuse to delete symlink targets or paths outside root.
- `cleanup` checks dirty/untracked/unpushed commits and active worktree ownership; default dry run for destructive actions; never force-delete by default.
- Never copy `.env`, private keys, auth tokens or secrets automatically into worktrees.
- Logs must be size bounded, access-controlled, and scrub known secret patterns when possible; no claim that pattern-based redaction guarantees secrecy.
- Agent and reviewer do not inherit coordinator's merge credentials by default.
- Git hooks/config may execute code; decide explicit trust policy and test it.
- Code execution from a branch is not safe just because it is in a worktree; use sandboxing as a separate deployment concern.
- `dev` write protection cannot be guaranteed when agents have unrestricted access to the same `.git` directory. Document MVP as cooperative protection and P1 strong-isolation mode as a separate coordinator-owned repository.

## 14. Observability and recoverability

- JSON logging to stderr; machine result to stdout. Redaction and maximum log sizes.
- Audit every transition and merge attempt with actor, time, source/target SHAs, and reason.
- `lprm doctor` checks Git support, repository common git dir, permissions, SQLite health, active lock and orphan worktrees.
- `lprm recover` identifies incomplete runs, compares Git state to DB, marks uncertain merges as `needs_operator_review` rather than duplicating work.
- No destructive automatic rollback: if merge fails before commit, abort only LPRM-owned temporary integration; do not reset unrelated work.

## 15. Acceptance criteria (release blockers)

**AC-01** `init` succeeds offline in a local Git repo with `dev`, no remote configured.

**AC-02** Two simultaneous agents can create distinct branches/worktrees without modifying the main `dev` working tree.

**AC-03** PR creation fails on dirty source, missing source branch, or empty diff; it must not stage/commit changes automatically.

**AC-04** CLI has deterministic JSON schema, exit codes, and command help; no interactive prompt during `--json`.

**AC-05** Conflict preflight detects conflicting merges without touching tracked/untracked contents or index.

**AC-06** A failing required check prevents queue merge; logs and exit codes remain inspectable.

**AC-07** All validations/approvals are tied to exact revision SHA pair and policy fingerprint; changing source or target invalidates them.

**AC-08** Two concurrent `queue run` processes cannot create duplicate merges.

**AC-09** If another PR is merged first, an older candidate is revalidated against the new target head.

**AC-10** The integrated combined tree (including effects of both PRs) receives required tests before dev integration.

**AC-11** Successful merge produces one no-ff merge commit on `dev` with correct target and feature parents, source changes present, audit recorded.

**AC-12** Worktree cleanup refuses dirty, outside-root, or unrecognized worktrees and never destroys data by default.

**AC-13** Killing process before merge, during integration, and after Git success but before DB commit is recoverable without duplicate merge.

**AC-14** Exit with a clear blocked state when Git capability, repository state, lock, check runner, or database condition is unsafe.

**AC-15** Full integration test suite creates ephemeral repos and runs without Git hosting, API keys, or network.

**AC-16** `README`, operator runbook, sample config, security model, and agent command contract are complete.

## 16. Test plan

- Unit: config validation, state-machine transitions, policy fingerprinting, JSON schema, error mapping.
- Integration: init, create two worktrees, commit changes, PR lifecycle, merge one after another.
- Conflict: two branches edit same region; PR stays blocked, `dev` intact.
- Concurrent: run two coordinator processes; verify exactly one lock holder and no duplicate commit.
- Staleness: source commit changes after approval; target moves after check; policy changes; ensure recheck.
- Failure injection: check timeout, invalid executable, SQLite busy, kill during merge, corrupted DB, disk exhaustion (where feasible).
- Security: path traversal, symlink cleanup, branch name validation, malicious filenames, hostile check argument strings, redaction.
- Cross-platform: Linux and macOS test matrices; no Windows promise in v1.

## 17. Implementation backlog and dependencies

**E1 Foundation — P0**
- T01 Initialize Rust workspace, binary and core modules.
- T02 CLI args, JSON output schema, typed errors, logging.
- T03 Repo discovery, `git` binary/version detection, `doctor`.
- T04 Config parsing, defaults, validation, schema/version handling.

**E2 Worktrees — P0**
- T05 Create feature branch from exact dev SHA and linked worktree.
- T06 Worktree list/status/ownership and dirty checks.
- T07 Safe cleanup and protected path checks.
- T08 Recovery of missing/orphaned worktrees.

**E3 PR lifecycle — P0**
- T09 SQLite schema/migrations/transactions.
- T10 PR create/list/show/diff/close and revision history.
- T11 State machine and transition guards.
- T12 Audit journal and SHA/policy fingerprint evidence model.

**E4 Validation — P0**
- T13 Diff and commit collection.
- T14 `merge-tree` conflict preflight.
- T15 Check runner, timeouts, stdout/stderr capture.
- T16 Freshness and invalidation logic.

**E5 Merge coordinator — P0**
- T17 Cross-process repository lock and durable queue.
- T18 Disposable integration worktree and integrated tests.
- T19 Controlled no-ff merge and candidate tree verification.
- T20 Failure reconciliation and `recover` command.

**E6 Review policy — P1**
- T21 Review findings and severity schema.
- T22 Protected-path/risk policy and approval model.
- T23 External AI reviewer subprocess adapter.
- T24 Auto-merge admission policies.

**E7 Agent integration — P0/P1**
- T25 Noninteractive machine contract and stable errors (P0).
- T26 Agent identity and ownership fields (P0).
- T27 Integration examples for Hermes/Codex via CLI (P1).
- T28 Optional agent event JSON output and callbacks (P1).

**E8 Hardening and release — P0/P1**
- T29 End-to-end temporary Git repo test matrix (P0).
- T30 Concurrency, crash and adversarial tests (P0).
- T31 Release packaging for Linux/macOS (P1).
- T32 Documentation, runbook, examples and versioning (P0).

### Suggested implementation sequencing

1. **Iteration A:** E1, E2 — reliable Git/worktree foundation.
2. **Iteration B:** E3, E4 — PR persistence + checks and revisions.
3. **Iteration C:** E5, T25/T26, T29/T30/T32 — complete **MVP**.
4. **Iteration D:** E6 and remaining E7/E8 — optional AI review and polished release.

Each task must ship code, tests, docs changes, and an artifact demonstrating its acceptance criteria. Avoid parallel work on Git merge coordination until the contract and invariants are settled.

## 18. Recommended workspace layout

```
lprm/
├── Cargo.toml                   # workspace
├── Cargo.lock
├── crates/
│   ├── lprm-cli/                # binary, clap, stdout contracts
│   ├── lprm-core/               # domain, FSM, policy, orchestration
│   ├── lprm-git/                # Git CLI adapter / repository operations
│   ├── lprm-store/              # SQLite persistence and migrations
│   └── lprm-runner/             # validation commands and reviewers
├── tests/                       # temp-repo end-to-end suites
├── examples/
│   └── lprm.toml
├── docs/
│   ├── architecture.md
│   ├── cli-contract.md
│   ├── threat-model.md
│   ├── runbook.md
│   └── adr/
└── README.md
```

Dependency direction: `cli -> core`; `core` defines Git/store/runner interfaces; adapters implement interfaces; avoid core importing concrete CLI or DB types. Do not introduce trait abstraction for every function; prefer narrow ports at testable domain boundaries.

## 19. Definition of done

An agent can create two worktrees from `dev`, propose two PRs, run required validation and reviews without any remote, enqueue both, and have the coordinator serialize merges to `dev`, safely invalidating stale checks, preserving a readable audit, and recovering from interrupted execution. A failed merge cannot corrupt or silently erase the developer's main working directory or pending agent work. All specified P0 acceptance criteria pass on macOS and Linux.

## 20. Agent implementation instruction

Treat this PRD as normative. Begin with architecture decision records for repo identity, locking, integration-merge backend, and review freshness. Implement E1–E5 in order (parallelize independent tests/docs only), create small commits, write temporary-Git-repository integration tests early, and never modify an existing user's working repository to test merge logic. Do not add a daemon, hosted Git dependency, UI, or AI provider until the MVP gates are demonstrably correct. Report each completed task with: changed files, key design choices, tests run/results, failure modes, and remaining risks. Ask for human approval before destructive Git operations or deleting user-owned worktrees. Treat all secrets as unavailable to LPRM unless explicitly injected by the operator.

## 21. References

- Git merge-tree official documentation: https://git-scm.com/docs/git-merge-tree
- Git worktree official documentation: https://git-scm.com/docs/git-worktree
- Git merge official documentation: https://git-scm.com/docs/git-merge
- Git update-ref official documentation: https://git-scm.com/docs/git-update-ref
