#!/bin/sh
# Exercises scripts/install.sh against a locally built archive, offline.
#   scripts/smoke-install.sh dist/lgit-<version>-<target>.tar.gz
set -eu
ARCHIVE=${1:?usage: smoke-install.sh path/to/lgit-<version>-<target>.tar.gz}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
ARCHIVE=$(cd "$(dirname "$ARCHIVE")" && pwd)/$(basename "$ARCHIVE")
NAME=$(basename "$ARCHIVE" .tar.gz)
VERSION=$(echo "$NAME" | sed 's/^lgit-\([^-]*\)-.*/\1/')
WORK=$(mktemp -d "${TMPDIR:-/tmp}/lgit-smoke-install.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
mkdir "$WORK/assets" "$WORK/bin"
cp "$ARCHIVE" "$ARCHIVE.sha256" "$WORK/assets/"
export LGIT_BASE_URL="$WORK/assets" LGIT_VERSION="$VERSION" LGIT_BIN_DIR="$WORK/bin"

sh "$ROOT/scripts/install.sh"
"$WORK/bin/lgit" --version | grep -q "lgit $VERSION"
sh "$ROOT/scripts/install.sh" | grep -q "already installed"
sh "$ROOT/scripts/install.sh" --force >/dev/null

# A corrupted download must be refused and must leave the installed binary untouched.
before=$(cksum < "$WORK/bin/lgit")
echo tampered >> "$WORK/assets/$NAME.tar.gz"
if sh "$ROOT/scripts/install.sh" --force 2>/dev/null; then echo "tampered archive was accepted" >&2; exit 1; fi
[ "$before" = "$(cksum < "$WORK/bin/lgit")" ]

sh "$ROOT/scripts/install.sh" --uninstall >/dev/null
[ ! -e "$WORK/bin/lgit" ]
echo "install smoke ok"
