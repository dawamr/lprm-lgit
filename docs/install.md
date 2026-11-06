# Installing LPRM

## Requirements

- macOS or Linux (x86-64 or ARM64).
- Git 2.38 or newer (`git --version`). LPRM needs `git merge-tree --write-tree`.
- No other runtime dependencies. SQLite is built in.

## Quick install (no Rust needed)

The release binaries are prebuilt and statically include SQLite, so you only need Git 2.38 or
newer at runtime. Rust is needed only to build from source.

```sh
curl -fsSL https://github.com/dawamr/lprm-lgit/releases/latest/download/install.sh | sh
```

The installer detects macOS ARM64, Linux x86-64, or Linux ARM64, downloads the matching archive,
verifies its SHA-256 checksum, and installs `lgit` into `~/.local/bin` atomically. Options:

```sh
sh install.sh --version v0.1.0     # a specific release
sh install.sh --bin-dir /usr/local/bin
sh install.sh --with-skills        # also copy the agent skills into known agent directories
sh install.sh --dry-run            # show what would happen
sh install.sh --uninstall
```

To update, run it again: it replaces the binary when a newer release exists and does nothing when
you are current (`--force` reinstalls). Prefer to read the script first? Download `install.sh`
from the release page and run it locally. Windows has no prebuilt binary yet.

## From a release archive

Archives are named `lgit-<version>-<target>.tar.gz`, with a matching `.sha256` file. Targets are
`aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, and
`aarch64-unknown-linux-gnu`.

```sh
shasum -a 256 -c lgit-0.1.0-aarch64-apple-darwin.tar.gz.sha256
tar -xzf lgit-0.1.0-aarch64-apple-darwin.tar.gz
install -m 0755 lgit-0.1.0-aarch64-apple-darwin/lgit /usr/local/bin/lgit
lgit --version
lgit doctor
```

On Linux use `sha256sum -c` instead of `shasum -a 256 -c`. `lgit --version` prints the version,
the commit, and the target triple. `BUILD-INFO` inside the archive records the same facts.

## From source

```sh
cargo build --release --locked
install -m 0755 target/release/lgit /usr/local/bin/lgit
```

Rust 1.89 or newer is required.

## Building the archives yourself

```sh
sh scripts/package-release.sh                 # host target
sh scripts/package-release.sh x86_64-unknown-linux-gnu
sh scripts/smoke-release.sh dist/lgit-0.1.0-aarch64-apple-darwin.tar.gz
```

The script uses locked dependencies, remapped paths, the commit timestamp, sorted archive entries
and numeric root ownership, so rebuilding the same commit with the same toolchain yields the same
checksum. The smoke script verifies the checksum, unpacks into an empty directory, and runs the
example agent workflow with only the unpacked binary on `PATH`.

## First run

```sh
cd your-repository
lgit doctor
lgit init --target dev
```

Copy [examples/lprm.sample.toml](examples/lprm.sample.toml) to `.lprm.toml` and edit the checks.
Then follow the [runbook](runbook.md).
