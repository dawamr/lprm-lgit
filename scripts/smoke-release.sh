#!/bin/sh
# Clean-machine smoke test for a release archive.
#   scripts/smoke-release.sh dist/lprm-<version>-<target>.tar.gz
# Verifies the checksum, unpacks into an empty directory, and runs the shipped example workflow
# with only the unpacked binary on PATH.
set -eu

ARCHIVE=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

(
    cd "$(dirname "$ARCHIVE")"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "$(basename "$ARCHIVE").sha256"
    else
        shasum -a 256 -c "$(basename "$ARCHIVE").sha256"
    fi
)
tar -xzf "$ARCHIVE" -C "$WORK"
DIR=$(find "$WORK" -mindepth 1 -maxdepth 1 -type d | head -n 1)
test -x "$DIR/lprm"
"$DIR/lprm" --version | grep -q '^lprm '
cat "$DIR/BUILD-INFO"
PATH="$DIR:/usr/bin:/bin" sh "$DIR/docs/examples/agent-workflow.sh"
echo "release smoke test passed"
