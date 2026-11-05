//! Embeds the commit and target triple so `lprm --version` identifies the exact build.

use std::process::Command;

fn main() {
    let commit = std::env::var("LPRM_BUILD_SHA")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=LPRM_BUILD_INFO={commit} {target}");
    println!("cargo:rerun-if-env-changed=LPRM_BUILD_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
