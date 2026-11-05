//! Agent skills must be well formed, only mention real commands, and install cleanly.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use common::shell_commands_in;
use lprm_cli::Cli;
use tempfile::tempdir;

const SKILLS: [&str; 3] = ["lprm-contributor", "lprm-coordinator", "lprm-recovery"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn frontmatter(text: &str) -> Vec<(String, String)> {
    let body = text
        .strip_prefix("---\n")
        .expect("skill starts with frontmatter");
    let end = body.find("\n---\n").expect("frontmatter is closed");
    body[..end]
        .lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[test]
fn skills_have_valid_frontmatter() {
    for name in SKILLS {
        let text = fs::read_to_string(root().join("skills").join(name).join("SKILL.md")).unwrap();
        let fields = frontmatter(&text);
        let get = |key: &str| {
            fields.iter().find(|(field, _)| field == key).map_or_else(
                || panic!("{name}: missing {key}"),
                |(_, value)| value.as_str(),
            )
        };
        assert_eq!(get("name"), name);
        let description = get("description");
        assert!(description.starts_with("Use when"), "{name}: {description}");
        assert!(description.len() <= 1024, "{name}: description too long");
    }
}

#[test]
fn every_skills_command_parses_with_the_real_cli() {
    for name in SKILLS {
        let text = fs::read_to_string(root().join("skills").join(name).join("SKILL.md")).unwrap();
        let commands = shell_commands_in(
            &text
                .replace("\n   ```", "\n```")
                .replace("\n   lprm", "\nlprm"),
        );
        assert!(!commands.is_empty(), "{name} documents no commands");
        for command in commands {
            if let Err(error) = Cli::try_parse_from(&command)
                && !matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayVersion | clap::error::ErrorKind::DisplayHelp
                )
            {
                panic!("{name}: command does not parse: {command:?}\n{error}");
            }
        }
    }
}

fn installer(arguments: &[&str]) -> std::process::Output {
    Command::new("/bin/sh")
        .arg(root().join("scripts/install-skills.sh"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn installer_copies_links_and_uninstalls_idempotently() {
    let target = tempdir().unwrap();
    let dir = target.path().join("skills");
    let dir_arg = dir.to_str().unwrap();

    for _ in 0..2 {
        let output = installer(&["--dir", dir_arg]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for name in SKILLS {
        let installed = dir.join(name).join("SKILL.md");
        assert_eq!(
            fs::read_to_string(&installed).unwrap(),
            fs::read_to_string(root().join("skills").join(name).join("SKILL.md")).unwrap()
        );
    }

    assert!(installer(&["--dir", dir_arg, "--link"]).status.success());
    assert!(
        fs::symlink_metadata(dir.join(SKILLS[0]))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    assert!(
        installer(&["--dir", dir_arg, "--uninstall"])
            .status
            .success()
    );
    for name in SKILLS {
        assert!(!dir.join(name).exists());
    }
}

#[test]
fn installer_dry_run_changes_nothing_and_unknown_flags_fail() {
    let target = tempdir().unwrap();
    let dir = target.path().join("skills");
    let output = installer(&["--dir", dir.to_str().unwrap(), "--dry-run"]);
    assert!(output.status.success());
    assert!(!dir.exists());
    assert_eq!(installer(&["--bogus"]).status.code(), Some(2));
}
