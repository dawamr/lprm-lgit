# Installing LPRM

## Requirements

- macOS or Linux (x86-64 or ARM64).
- Git 2.38 or newer (`git --version`). LPRM needs `git merge-tree --write-tree`.
- No other runtime dependencies. SQLite is built in.

## From a release archive

Archives are named `lgit-<version>-<target>.tar.gz`, with a matching `.sha256` file. Targets are
`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, and `aarch64-unknown-linux-gnu`.

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
