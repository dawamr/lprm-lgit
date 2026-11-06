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
    for reference in ["HEAD", "refs/heads"] {
        if let Some(path) = git_path(reference) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(branch) = git_output(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git_path(&branch)
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn git_output(arguments: &[&str]) -> Option<String> {
    Command::new("git")
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn git_path(name: &str) -> Option<String> {
    git_output(&["rev-parse", "--path-format=absolute", "--git-path", name])
}
