//! Native Git CLI adapter for LPRM.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lprm_core::LprmError;
use serde::Serialize;

const MINIMUM_GIT_VERSION: GitVersion = GitVersion {
    major: 2,
    minor: 38,
    patch: 0,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl GitVersion {
    /// Parses the numeric prefix from `git version` output.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when the output has no valid major and minor components.
    pub fn parse(version_output: &str) -> Result<Self, LprmError> {
        let token = version_output
            .split_whitespace()
            .find(|part| part.starts_with(|character: char| character.is_ascii_digit()))
            .ok_or_else(|| LprmError::git_precondition("Git returned an unrecognized version"))?;
        let mut components = token.split('.');

        let major = parse_numeric_component(components.next(), "major")?;
        let minor = parse_numeric_component(components.next(), "minor")?;
        let patch = components
            .next()
            .map_or(Ok(0), |value| parse_numeric_component(Some(value), "patch"))?;

        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RepositoryInfo {
    pub common_git_dir: PathBuf,
    pub worktree_root: PathBuf,
    pub has_submodules: bool,
    pub bare: bool,
    pub shallow: bool,
    pub git_dir_writable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub git: GitVersion,
    pub repository: RepositoryInfo,
    pub merge_tree_write_tree: bool,
}

#[derive(Clone, Debug)]
pub struct Git {
    executable: PathBuf,
}

impl Default for Git {
    fn default() -> Self {
        Self::new("git")
    }
}

impl Git {
    /// Creates an adapter for the given Git executable path or program name.
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    /// Reads and parses the installed Git version.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when Git is unavailable, fails, or emits an unknown version.
    pub fn version(&self) -> Result<GitVersion, LprmError> {
        let output = Command::new(&self.executable)
            .arg("version")
            .output()
            .map_err(|_| LprmError::git_precondition("Git executable is unavailable"))?;
        if !output.status.success() {
            return Err(LprmError::git_precondition("Git version detection failed"));
        }

        GitVersion::parse(&String::from_utf8_lossy(&output.stdout))
    }

    /// Resolves stable repository paths and supported repository properties.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when `path` is outside a non-bare working repository or
    /// Git returns an inaccessible path or invalid property.
    pub fn discover(&self, path: &Path) -> Result<RepositoryInfo, LprmError> {
        let common_git_dir = self.required_path(
            path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            "The current directory is not inside a Git repository",
        )?;
        let worktree_root = self.required_path(
            path,
            &["rev-parse", "--path-format=absolute", "--show-toplevel"],
            "LPRM requires a non-bare Git working tree",
        )?;
        let bare = self.required_boolean(path, &["rev-parse", "--is-bare-repository"])?;
        let shallow = self.required_boolean(path, &["rev-parse", "--is-shallow-repository"])?;
        let has_submodules = worktree_root.join(".gitmodules").exists()
            || self
                .run(path, &["ls-files", "--error-unmatch", "--", ".gitmodules"])?
                .status
                .success();
        let git_dir_writable =
            fs::metadata(&common_git_dir).is_ok_and(|metadata| !metadata.permissions().readonly());

        Ok(RepositoryInfo {
            common_git_dir,
            worktree_root,
            has_submodules,
            bare,
            shallow,
            git_dir_writable,
        })
    }

    /// Checks the runtime help for modern `merge-tree --write-tree` support.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when Git cannot be started.
    pub fn supports_merge_tree_write_tree(&self, path: &Path) -> Result<bool, LprmError> {
        let output = self.run(path, &["merge-tree", "-h"])?;
        let mut help = output.stdout;
        help.extend_from_slice(&output.stderr);
        Ok(String::from_utf8_lossy(&help).contains("--write-tree"))
    }

    /// Resolves a local branch to its exact commit object ID.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when the branch name is invalid, absent, or not a commit.
    pub fn resolve_local_branch(&self, path: &Path, branch: &str) -> Result<String, LprmError> {
        let valid = self.run(path, &["check-ref-format", "--branch", branch])?;
        if !valid.status.success() {
            return Err(LprmError::git_precondition(format!(
                "Invalid target branch {branch:?}"
            )));
        }

        let revision = format!("refs/heads/{branch}^{{commit}}");
        let output = self.run(path, &["rev-parse", "--verify", &revision])?;
        if !output.status.success() {
            return Err(LprmError::git_precondition(format!(
                "Target branch {branch:?} does not exist or has no commit"
            )));
        }
        let object_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !is_object_id(&object_id) {
            return Err(LprmError::git_precondition(
                "Git returned an invalid commit object ID",
            ));
        }
        Ok(object_id)
    }

    /// Creates a linked worktree and new branch at an exact commit object ID.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error for invalid names, existing paths, or failed Git creation.
    pub fn create_linked_worktree(
        &self,
        repository: &Path,
        branch: &str,
        worktree_path: &Path,
        start_point: &str,
    ) -> Result<(), LprmError> {
        if worktree_path.exists() {
            return Err(LprmError::git_precondition(format!(
                "Worktree path already exists: {}",
                worktree_path.display()
            )));
        }
        let valid = self.run(repository, &["check-ref-format", "--branch", branch])?;
        if !valid.status.success() {
            return Err(LprmError::git_precondition(
                "Generated worktree branch is invalid",
            ));
        }
        if !is_object_id(start_point) {
            return Err(LprmError::git_precondition(
                "Worktree start point is not a full commit object ID",
            ));
        }

        let output = Command::new(&self.executable)
            .arg("-C")
            .arg(repository)
            .args(["worktree", "add", "-b", branch])
            .arg(worktree_path)
            .arg(start_point)
            .output()
            .map_err(|_| LprmError::git_precondition("Git executable is unavailable"))?;
        if !output.status.success() {
            return Err(LprmError::git_precondition(
                "Git could not create the linked worktree",
            ));
        }
        Ok(())
    }

    fn required_path(
        &self,
        path: &Path,
        arguments: &[&str],
        failure_message: &str,
    ) -> Result<PathBuf, LprmError> {
        let output = self.run(path, arguments)?;
        if !output.status.success() {
            return Err(LprmError::git_precondition(failure_message));
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if value.is_empty() {
            return Err(LprmError::git_precondition(failure_message));
        }

        fs::canonicalize(value).map_err(|_| {
            LprmError::git_precondition("Git returned an inaccessible repository path")
        })
    }

    fn required_boolean(&self, path: &Path, arguments: &[&str]) -> Result<bool, LprmError> {
        let output = self.run(path, arguments)?;
        if !output.status.success() {
            return Err(LprmError::git_precondition(
                "Git repository inspection failed",
            ));
        }

        match String::from_utf8_lossy(&output.stdout).trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(LprmError::git_precondition(
                "Git returned an invalid boolean repository property",
            )),
        }
    }

    fn run(&self, path: &Path, arguments: &[&str]) -> Result<Output, LprmError> {
        Command::new(&self.executable)
            .arg("-C")
            .arg(path)
            .args(arguments)
            .output()
            .map_err(|_| LprmError::git_precondition("Git executable is unavailable"))
    }
}

/// Validates the Git executable and repository prerequisites required by LPRM v1.
///
/// # Errors
///
/// Returns a Git precondition error for unsupported Git, repositories, capabilities, or access.
pub fn doctor(path: &Path) -> Result<DoctorReport, LprmError> {
    let git = Git::default();
    let version = git.version()?;
    if version < MINIMUM_GIT_VERSION {
        return Err(LprmError::git_precondition("Git 2.38 or newer is required"));
    }

    let repository = git.discover(path)?;
    if repository.bare {
        return Err(LprmError::git_precondition(
            "LPRM requires a non-bare Git working tree",
        ));
    }
    if repository.has_submodules {
        return Err(LprmError::git_precondition(
            "Repositories with submodules are unsupported in LPRM v1",
        ));
    }
    if !repository.git_dir_writable {
        return Err(LprmError::git_precondition(
            "The common Git directory is not writable",
        ));
    }

    let merge_tree_write_tree = git.supports_merge_tree_write_tree(path)?;
    if !merge_tree_write_tree {
        return Err(LprmError::git_precondition(
            "Git does not support merge-tree --write-tree",
        ));
    }

    Ok(DoctorReport {
        git: version,
        repository,
        merge_tree_write_tree,
    })
}

fn parse_numeric_component(value: Option<&str>, name: &str) -> Result<u32, LprmError> {
    let numeric = value
        .unwrap_or_default()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    numeric.parse().map_err(|_| {
        LprmError::git_precondition(format!("Git returned an invalid {name} version component"))
    })
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{Git, GitVersion};

    fn run_git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(arguments)
            .status()
            .expect("git should start");
        assert!(status.success(), "git command failed: {arguments:?}");
    }

    #[test]
    fn parses_standard_and_vendor_git_versions() {
        assert_eq!(
            GitVersion::parse("git version 2.50.1").expect("version should parse"),
            GitVersion {
                major: 2,
                minor: 50,
                patch: 1,
            }
        );
        assert_eq!(
            GitVersion::parse("git version 2.50.1 (Apple Git-155)")
                .expect("vendor version should parse"),
            GitVersion {
                major: 2,
                minor: 50,
                patch: 1,
            }
        );
    }

    #[test]
    fn linked_worktrees_share_the_canonical_common_directory() {
        let fixture = tempdir().expect("temporary directory should be created");
        let repository = fixture.path().join("repository");
        let linked = fixture.path().join("linked");
        fs::create_dir(&repository).expect("repository directory should be created");
        run_git(&repository, &["init", "--quiet", "--initial-branch=dev"]);
        run_git(&repository, &["config", "user.name", "LPRM Test"]);
        run_git(
            &repository,
            &["config", "user.email", "lprm@example.invalid"],
        );
        fs::write(repository.join("README.md"), "fixture\n").expect("fixture should be written");
        run_git(&repository, &["add", "README.md"]);
        run_git(&repository, &["commit", "--quiet", "-m", "fixture"]);
        run_git(
            &repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/a",
                linked.to_str().expect("fixture path should be UTF-8"),
                "dev",
            ],
        );

        let git = Git::default();
        let primary = git
            .discover(&repository)
            .expect("primary should be discovered");
        let secondary = git.discover(&linked).expect("linked should be discovered");

        assert_eq!(primary.common_git_dir, secondary.common_git_dir);
        assert_ne!(primary.worktree_root, secondary.worktree_root);
    }

    #[test]
    fn detects_an_untracked_gitmodules_file() {
        let repository = tempdir().expect("temporary directory should be created");
        run_git(repository.path(), &["init", "--quiet"]);
        fs::write(repository.path().join(".gitmodules"), "[submodule \"x\"]\n")
            .expect("fixture should be written");

        let info = Git::default()
            .discover(repository.path())
            .expect("repository should be discovered");

        assert!(info.has_submodules);
    }
}
