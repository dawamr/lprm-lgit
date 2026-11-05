#!/bin/sh
# End-to-end example used in docs/agent-workflows.md. It builds a throwaway repository, then walks
# one task from isolated worktree to verified merge using only stable `lgit --json` commands.
# Requires `lgit` on PATH. Safe to run: everything lives under a temporary directory.
set -eu

# Extracts a top-level string field from the single-line JSON envelope (no jq needed).
field() { sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p" | head -n 1; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
export LPRM_STATE_HOME="$WORK/state"
REPO="$WORK/repo"
mkdir "$REPO"
cd "$REPO"
git init -q -b dev
git config user.name "Example"
git config user.email "example@example.invalid"
echo "base" > base.txt
cat > .lprm.toml <<'TOML'
schema_version = 1
[checks]
timeout_seconds = 30
required = ["has-feature"]
[checks.commands]
has-feature = ["/bin/sh", "-c", "test -f feature.txt || test -f base.txt"]
TOML
git add -A
git commit -q -m "base"

lgit --json init --target dev > /dev/null

# Codex: isolated worktree, commit, propose.
CREATED=$(lgit --json --actor codex worktree create --task "add feature")
BRANCH=$(echo "$CREATED" | field branch)
WORKTREE=$(echo "$CREATED" | field path)
echo "feature" > "$WORKTREE/feature.txt"
git -C "$WORKTREE" add feature.txt
git -C "$WORKTREE" commit -q -m "Add feature"
PR=$(lgit --json --actor codex pr create --from "$BRANCH" --title "Add feature" | field pr_id)

# Hermes: validate, gate, and merge. Separate identities keep the audit trail honest.
lgit --json --actor hermes pr ready "$PR" > /dev/null
lgit --json --actor hermes pr check "$PR" > /dev/null
lgit --json --actor hermes pr review "$PR" > /dev/null
lgit --json --actor hermes pr enqueue "$PR" > /dev/null
lgit --json --actor hermes queue run --once > /dev/null

STATUS=$(lgit --json pr show "$PR" | field status)
[ "$STATUS" = "merged" ] || { echo "expected merged, got $STATUS" >&2; exit 1; }

# The target now has one --no-ff merge commit with the tested parents.
PARENTS=$(git rev-list --parents -n 1 dev | wc -w)
[ "$PARENTS" -eq 3 ] || { echo "expected a merge commit" >&2; exit 1; }

# Nothing is left to recover, and the audit trail names every actor.
lgit --json --actor hermes recover --dry-run > /dev/null
lgit --json audit "$PR" | grep -q '"actor":"codex"'
lgit --json audit "$PR" | grep -q '"actor":"hermes"'

# Clean up the merged worktree: dry run first, then for real as the owner.
lgit --json --actor codex worktree cleanup "$(echo "$CREATED" | field id)" > /dev/null
lgit --json --actor codex --yes worktree cleanup "$(echo "$CREATED" | field id)" > /dev/null
echo "ok: $PR merged"
