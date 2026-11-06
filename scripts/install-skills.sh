#!/bin/sh
# Installs the LPRM agent skills into the skill directories of your coding agents.
#   scripts/install-skills.sh                 # every known agent directory that exists
#   scripts/install-skills.sh --dir PATH      # a specific skills directory (repeatable)
#   scripts/install-skills.sh --link          # symlink instead of copy, so updates follow the repo
#   scripts/install-skills.sh --uninstall     # remove the installed LPRM skills
#   scripts/install-skills.sh --dry-run       # show what would happen
# Known directories: ~/.claude/skills, ~/.codex/skills, ~/.agents/skills, ~/.config/opencode/skills.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
MODE=copy
DRY=0
UNINSTALL=0
DIRS=""

while [ $# -gt 0 ]; do
    case "$1" in
        --dir) DIRS="$DIRS
$2"; shift 2 ;;
        --link) MODE=link; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        --dry-run) DRY=1; shift ;;
        -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

if [ -z "$DIRS" ]; then
    for candidate in "$HOME/.claude/skills" "$HOME/.codex/skills" "$HOME/.agents/skills" "$HOME/.config/opencode/skills"; do
        [ -d "$candidate" ] && DIRS="$DIRS
$candidate"
    done
fi
if [ -z "$DIRS" ]; then
    echo "no agent skill directory found; pass --dir PATH" >&2
    exit 1
fi

run() { if [ "$DRY" -eq 1 ]; then echo "would: $*"; else "$@"; fi; }

echo "$DIRS" | while IFS= read -r dir; do
    [ -n "$dir" ] || continue
    for skill in "$ROOT"/skills/*/; do
        name=$(basename "$skill")
        dest="$dir/$name"
        if [ "$UNINSTALL" -eq 1 ]; then
            if [ -e "$dest" ] || [ -L "$dest" ]; then
                run rm -rf "$dest"
                echo "removed $dest"
            fi
            continue
        fi
        run mkdir -p "$dir"
        run rm -rf "$dest"
        if [ "$MODE" = link ]; then
            run ln -s "${skill%/}" "$dest"
        else
            run cp -R "${skill%/}" "$dest"
        fi
        echo "installed $dest ($MODE)"
    done
done
