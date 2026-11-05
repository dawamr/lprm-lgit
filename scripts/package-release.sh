#!/bin/sh
# Builds a reproducible release archive for one target.
#   scripts/package-release.sh [target-triple]
# Output: dist/lprm-<version>-<target>.tar.gz and a matching .sha256 file.
# Reproducibility: locked dependencies, remapped source paths, a fixed timestamp taken from the
# commit (SOURCE_DATE_EPOCH), sorted entries, and numeric root ownership in the archive.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
TARGET=${1:-$(rustc -vV | sed -n 's/^host: //p')}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
NAME="lprm-$VERSION-$TARGET"

if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    SOURCE_DATE_EPOCH=$(git log -1 --format=%ct 2>/dev/null || echo 0)
fi
export SOURCE_DATE_EPOCH
LPRM_BUILD_SHA=$(git rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
export LPRM_BUILD_SHA
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$ROOT=/build --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"

cargo build --release --locked --target "$TARGET" -p lprm-cli

STAGE="dist/stage/$NAME"
rm -rf "dist/stage" "dist/$NAME.tar.gz" "dist/$NAME.tar.gz.sha256"
mkdir -p "$STAGE/docs/examples"
cp "target/$TARGET/release/lprm" "$STAGE/lprm"
cp README.md CHANGELOG.md "$STAGE/"
cp docs/*.md "$STAGE/docs/"
cp docs/examples/lprm.sample.toml docs/examples/agent-workflow.sh "$STAGE/docs/examples/"
printf 'version=%s\ntarget=%s\ncommit=%s\nsource_date_epoch=%s\n' \
    "$VERSION" "$TARGET" "$LPRM_BUILD_SHA" "$SOURCE_DATE_EPOCH" > "$STAGE/BUILD-INFO"
find "$STAGE" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} + 2>/dev/null \
    || find "$STAGE" -exec touch -h -t "$(date -u -r "$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S)" {} +

(
    cd dist/stage
    find "$NAME" | LC_ALL=C sort > ../files.list
    if tar --version 2>/dev/null | grep -q "GNU tar"; then
        tar --no-recursion --owner=0 --group=0 --numeric-owner -T ../files.list -cf - | gzip -n -9 > "../$NAME.tar.gz"
    else
        tar --no-recursion --uid 0 --gid 0 --numeric-owner -T ../files.list -cf - | gzip -n -9 > "../$NAME.tar.gz"
    fi
)
rm -rf dist/stage dist/files.list

(
    cd dist
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256"
    else
        shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256"
    fi
)
echo "dist/$NAME.tar.gz"
cat "dist/$NAME.tar.gz.sha256"
