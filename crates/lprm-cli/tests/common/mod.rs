//! Shared offline fixture for CLI integration tests.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::{TempDir, tempdir};

pub struct Fixture {
    _temporary: TempDir,
    pub repository: PathBuf,
    pub state: PathBuf,
}

pub struct Submitted {
    pub pr_id: String,
    pub path: PathBuf,
}

impl Fixture {
    pub fn new(check_command: &str) -> Self {
        Self::with_check(&format!("[\"{check_command}\"]"))
    }

    /// `check_array` is a TOML array literal holding the `gate` command and its arguments.
    pub fn with_check(check_array: &str) -> Self {
        Self::with_options(check_array, "")
    }

    /// `extra_toml` is appended to `.lprm.toml`, e.g. `[review]` or `[policy]` sections.
    pub fn with_options(check_array: &str, extra_toml: &str) -> Self {
        let temporary = tempdir().unwrap();
        let repository = temporary.path().join("repository");
        let state = temporary.path().join("state");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-b", "dev"]);
        git(&repository, &["config", "user.name", "LPRM Test"]);
        git(
            &repository,
            &["config", "user.email", "lprm-test@example.invalid"],
        );
        fs::write(repository.join("base.txt"), "base\n").unwrap();
        fs::write(
            repository.join(".lprm.toml"),
            format!(
                "schema_version = 1\n[checks]\ntimeout_seconds = 5\nrequired = [\"gate\"]\n[checks.commands]\ngate = {check_array}\n{extra_toml}\n"
            ),
        )
        .unwrap();
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "base"]);
        let fixture = Self {
            _temporary: temporary,
            repository,
            state,
        };
        assert_success(&fixture.lprm(&["--json", "init", "--target", "dev"]));
        fixture
    }

    pub fn lprm(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lgit"))
            .current_dir(&self.repository)
            .env("LPRM_STATE_HOME", &self.state)
            .args(arguments)
            .output()
            .unwrap()
    }

    pub fn status(&self, pr_id: &str) -> String {
        let shown = self.json_success(&["--json", "pr", "show", pr_id]);
        shown["data"]["status"].as_str().unwrap().to_owned()
    }

    pub fn approve_and_enqueue(&self, pr_id: &str) {
        assert_success(&self.lprm(&["--json", "pr", "ready", pr_id]));
        assert_success(&self.lprm(&["--json", "pr", "check", pr_id]));
        assert_success(&self.lprm(&["--json", "pr", "enqueue", pr_id]));
    }

    pub fn json_success(&self, arguments: &[&str]) -> Value {
        let output = self.lprm(arguments);
        assert_success(&output);
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Creates a worktree and a PR that adds `file`, returning the PR ID and worktree path.
    pub fn submit(&self, file: &str) -> Submitted {
        let worktree = self.json_success(&["--json", "worktree", "create", "--task", file]);
        let path = PathBuf::from(worktree["data"]["path"].as_str().unwrap());
        let branch = worktree["data"]["branch"].as_str().unwrap();
        let target = path.join(file);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, "content\n").unwrap();
        git(&path, &["add", file]);
        git(&path, &["commit", "-m", file]);
        let created =
            self.json_success(&["--json", "pr", "create", "--from", branch, "--title", file]);
        Submitted {
            pr_id: created["data"]["pr_id"].as_str().unwrap().to_owned(),
            path,
        }
    }
}

pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Git failed: {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn git_stdout(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success(), "Git failed: {arguments:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub fn checkout_snapshot(repository: &Path) -> [String; 3] {
    [
        git_stdout(repository, &["status", "--porcelain=v1", "-uall"]),
        git_stdout(repository, &["write-tree"]),
        git_stdout(repository, &["rev-parse", "HEAD"]),
    ]
}

/// Splits a documented command line on whitespace, honouring double quotes.
pub fn split_words(line: &str) -> Vec<String> {
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
pub fn shell_commands_in(text: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut in_shell = false;
    for line in text.lines() {
        if line.starts_with("```") || line.trim_start().starts_with("```") {
            in_shell = line.trim() == "```sh";
            continue;
        }
        let line = line.trim();
        if in_shell && line.starts_with("lgit ") {
            let line = line.split(" #").next().unwrap();
            let mut words = split_words(line);
            for word in &mut words {
                if word.starts_with('<') || word.starts_with('$') {
                    "placeholder".clone_into(word);
                }
            }
            commands.push(words);
        }
    }
    commands
}
