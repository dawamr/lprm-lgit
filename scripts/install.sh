#!/bin/sh
# Installs or updates the lgit binary from a GitHub release. No Rust toolchain is needed.
#   curl -fsSL https://github.com/dawamr/lprm-lgit/releases/latest/download/install.sh | sh
#   sh install.sh [--version vX.Y.Z] [--bin-dir DIR] [--with-skills] [--force] [--dry-run]
#   sh install.sh --uninstall [--bin-dir DIR]
# Re-running the installer updates to the latest release. The archive checksum is always verified.
# Environment: LGIT_VERSION, LGIT_BIN_DIR (default ~/.local/bin), LGIT_REPO (owner/name),
#              LGIT_BASE_URL (directory or URL holding the release assets, for mirrors and tests).
set -eu

REPO=${LGIT_REPO:-dawamr/lprm-lgit}
VERSION=${LGIT_VERSION:-latest}
BIN_DIR=${LGIT_BIN_DIR:-$HOME/.local/bin}
BASE_URL=${LGIT_BASE_URL:-}
WITH_SKILLS=0
FORCE=0
DRY=0
UNINSTALL=0

die() { echo "install: $*" >&2; exit 1; }
say() { echo "install: $*"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION=$2; shift 2 ;;
        --bin-dir) [ $# -ge 2 ] || die "--bin-dir needs a value"; BIN_DIR=$2; shift 2 ;;
        --with-skills) WITH_SKILLS=1; shift ;;
        --force) FORCE=1; shift ;;
        --dry-run) DRY=1; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

if [ "$UNINSTALL" = 1 ]; then
    if [ "$DRY" = 1 ]; then say "would remove $BIN_DIR/lgit"; else rm -f "$BIN_DIR/lgit"; say "removed $BIN_DIR/lgit"; fi
    exit 0
fi

fetch() { # fetch URL DEST
    case "$1" in
        /*) cp "$1" "$2" ;;
        *) curl -fsSL --retry 3 -o "$2" "$1" ;;
    esac
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os/$arch" in
        Darwin/arm64) echo aarch64-apple-darwin ;;
        Darwin/x86_64) echo x86_64-apple-darwin ;;
        Linux/x86_64|Linux/amd64) echo x86_64-unknown-linux-gnu ;;
        Linux/aarch64|Linux/arm64) echo aarch64-unknown-linux-gnu ;;
        *) die "no prebuilt binary for $os/$arch; build from source (see docs/install.md)" ;;
    esac
}

sha256_check() { # sha256_check FILE.sha256 (run in the archive directory)
    if command -v sha256sum >/dev/null 2>&1; then sha256sum -c "$1" >/dev/null
    elif command -v shasum >/dev/null 2>&1; then shasum -a 256 -c "$1" >/dev/null
    else die "sha256sum or shasum is required to verify the download"; fi
}

command -v git >/dev/null 2>&1 || say "warning: git was not found; lgit needs Git 2.38 or newer at runtime"
if [ -z "$BASE_URL" ]; then
    command -v curl >/dev/null 2>&1 || die "curl is required"
fi
TARGET=$(detect_target)

if [ "$VERSION" = latest ]; then
    [ -z "$BASE_URL" ] || die "set LGIT_VERSION when using LGIT_BASE_URL"
    final=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
        || die "could not resolve the latest release of $REPO"
    VERSION=${final##*/}
    case "$VERSION" in v[0-9]*) ;; *) die "no published release found for $REPO" ;; esac
fi
case "$VERSION" in v*) TAG=$VERSION ;; *) TAG=v$VERSION ;; esac
NUMBER=${TAG#v}
NAME="lgit-$NUMBER-$TARGET"
[ -n "$BASE_URL" ] || BASE_URL="https://github.com/$REPO/releases/download/$TAG"

INSTALLED=""
if [ -x "$BIN_DIR/lgit" ]; then
    INSTALLED=$("$BIN_DIR/lgit" --version 2>/dev/null | awk '{print $2}' || true)
fi
if [ "$INSTALLED" = "$NUMBER" ] && [ "$FORCE" = 0 ]; then
    say "lgit $NUMBER is already installed in $BIN_DIR (use --force to reinstall)"
    exit 0
fi
if [ "$DRY" = 1 ]; then
    say "would install $NAME from $BASE_URL into $BIN_DIR (currently: ${INSTALLED:-none})"
    exit 0
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/lgit-install.XXXXXX")
trap 'rm -rf "$WORK"' EXIT INT TERM
say "downloading $NAME"
fetch "$BASE_URL/$NAME.tar.gz" "$WORK/$NAME.tar.gz" || die "download failed: $BASE_URL/$NAME.tar.gz"
fetch "$BASE_URL/$NAME.tar.gz.sha256" "$WORK/$NAME.tar.gz.sha256" || die "checksum download failed"
(cd "$WORK" && sha256_check "$NAME.tar.gz.sha256") || die "checksum mismatch; refusing to install"
mkdir "$WORK/unpack"
tar -xzf "$WORK/$NAME.tar.gz" -C "$WORK/unpack"
[ -f "$WORK/unpack/$NAME/lgit" ] || die "archive does not contain lgit"

mkdir -p "$BIN_DIR"
install -m 0755 "$WORK/unpack/$NAME/lgit" "$BIN_DIR/.lgit.new.$$"
mv -f "$BIN_DIR/.lgit.new.$$" "$BIN_DIR/lgit"
if [ -n "$INSTALLED" ]; then say "updated lgit $INSTALLED -> $NUMBER"; else say "installed lgit $NUMBER"; fi

if [ "$WITH_SKILLS" = 1 ]; then
    sh "$WORK/unpack/$NAME/scripts/install-skills.sh"
fi

"$BIN_DIR/lgit" --version
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) say "add $BIN_DIR to PATH, for example: export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
