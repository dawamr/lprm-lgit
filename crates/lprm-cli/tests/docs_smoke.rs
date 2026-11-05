//! Keeps the documentation honest: every documented command must parse with the real CLI, every
//! command must be documented, the sample config must load, and the example workflow must run.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use lprm_cli::Cli;
use lprm_core::Config;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn documents() -> Vec<PathBuf> {
    let mut files = vec![root().join("README.md")];
    for entry in fs::read_dir(root().join("docs")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "md") {
            files.push(path);
        }
    }
    files
}

/// Splits a documented command line on whitespace, honouring double quotes.
fn split(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in line.chars() {
        match character {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Every `lprm ...` line inside a fenced `sh` block, with comments and placeholders resolved.
fn documented_commands() -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    for path in documents() {
        let text = fs::read_to_string(&path).unwrap();
        let mut in_shell = false;
        for line in text.lines() {
            if line.starts_with("```") {
                in_shell = line.trim() == "```sh";
                continue;
            }
            let line = line.trim();
            if in_shell && line.starts_with("lprm ") {
                let line = line.split(" #").next().unwrap();
                let mut words = split(line);
                for word in &mut words {
                    if word.starts_with('<') || word.starts_with('$') {
                        "placeholder".clone_into(word);
                    }
                }
                commands.push(words);
            }
        }
    }
    commands
}

#[test]
fn every_documented_command_parses_with_the_real_cli() {
    let commands = documented_commands();
    assert!(
        commands.len() > 30,
        "found only {} commands",
        commands.len()
    );
    for command in commands {
        if let Err(error) = Cli::try_parse_from(&command) {
            panic!("documented command does not parse: {command:?}\n{error}");
        }
    }
}

#[test]
fn every_command_is_documented() {
    let commands = documented_commands();
    let documented = |prefix: &[&str]| {
        commands.iter().any(|command| {
            let words: Vec<&str> = command
                .iter()
                .skip(1)
                .filter(|word| !word.starts_with("--"))
                .map(String::as_str)
                .collect();
            words.starts_with(prefix)
        })
    };
    for prefix in [
        &["init"][..],
        &["doctor"],
        &["config", "show"],
        &["worktree", "create"],
        &["worktree", "list"],
        &["worktree", "status"],
        &["worktree", "cleanup"],
        &["pr", "create"],
        &["pr", "list"],
        &["pr", "show"],
        &["pr", "diff"],
        &["pr", "ready"],
        &["pr", "sync"],
        &["pr", "check"],
        &["pr", "review"],
        &["pr", "approve"],
        &["pr", "enqueue"],
        &["pr", "close"],
        &["queue", "list"],
        &["queue", "run"],
        &["audit"],
        &["recover"],
    ] {
        assert!(documented(prefix), "undocumented command: {prefix:?}");
    }
}

#[test]
fn sample_config_loads_and_matches_the_defaults() {
    let sample = fs::read_to_string(root().join("docs/examples/lprm.sample.toml")).unwrap();
    let config = Config::from_toml(&sample).expect("sample config must be valid");
    assert_eq!(config.repository, Config::default().repository);
    assert_eq!(config.merge, Config::default().merge);
    assert_eq!(config.checks, Config::default().checks);
    assert_eq!(config.review, Config::default().review);
    assert_eq!(config.policy, Config::default().policy);
    assert_eq!(config.events, Config::default().events);
}

#[test]
fn example_agent_workflow_runs_against_a_temporary_repository() {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_lprm"));
    let path = format!(
        "{}:{}",
        binary.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new("/bin/sh")
        .arg(root().join("docs/examples/agent-workflow.sh"))
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ok: "));
}
