//! Native Git CLI adapter for LPRM.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lprm_core::LprmError;
use serde::Serialize;
use tempfile::TempDir;

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorktreeObservation {
    pub present: bool,
    pub actual_branch: Option<String>,
    pub head_sha: Option<String>,
    pub dirty: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GitWorktree {
    pub path: PathBuf,
    pub head_sha: Option<String>,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub added_lines: Option<u64>,
    pub deleted_lines: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiffCollection {
    pub target_sha: String,
    pub source_sha: String,
    pub commits: Vec<String>,
    pub files: Vec<DiffFile>,
    pub patch: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MergePreflight {
    Clean { tree_sha: String },
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IntegratedMerge {
    pub target_after_sha: String,
    pub tree_sha: String,
    pub parents: Vec<String>,
    pub worktree_clean: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "merge", rename_all = "snake_case")]
pub enum GuardedMerge {
    Verified(IntegratedMerge),
    Diverged(IntegratedMerge),
}

#[derive(Clone, Debug)]
pub struct Git {
    executable: PathBuf,
}

#[derive(Debug)]
pub struct IntegrationCandidate {
    git: Git,
    repository: PathBuf,
    container: Option<TempDir>,
    path: PathBuf,
    commit_sha: String,
    tree_sha: String,
    target_sha: String,
    source_sha: String,
}

impl IntegrationCandidate {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn commit_sha(&self) -> &str {
        &self.commit_sha
    }

    pub fn tree_sha(&self) -> &str {
        &self.tree_sha
    }

    pub fn target_sha(&self) -> &str {
        &self.target_sha
    }

    pub fn source_sha(&self) -> &str {
        &self.source_sha
    }

    fn merge_exact_source(&mut self) -> Result<(), LprmError> {
        let merged = Command::new(&self.git.executable)
            .arg("-C")
            .arg(&self.path)
            .args(["merge", "--no-ff", "--no-edit", "--no-gpg-sign"])
            .arg(&self.source_sha)
            .env("GIT_AUTHOR_NAME", "LPRM Coordinator")
            .env("GIT_AUTHOR_EMAIL", "lprm@localhost")
            .env("GIT_COMMITTER_NAME", "LPRM Coordinator")
            .env("GIT_COMMITTER_EMAIL", "lprm@localhost")
            .output()
            .map_err(|_| LprmError::git_precondition("Git executable is unavailable"))?;
        if !merged.status.success() {
            return Err(LprmError::validation_failed(
                "Git could not merge the exact source into the integration candidate",
            ));
        }
        let commit = self
            .git
            .run(&self.path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        let tree = self
            .git
            .run(&self.path, &["rev-parse", "--verify", "HEAD^{tree}"])?;
        if !commit.status.success() || !tree.status.success() {
            return Err(LprmError::git_precondition(
                "Git could not inspect the integration candidate",
            ));
        }
        self.commit_sha = parse_single_object_id(&commit.stdout, "candidate commit")?;
        self.tree_sha = parse_single_object_id(&tree.stdout, "candidate tree")?;
        let parents = self.git.run(
            &self.path,
            &["rev-list", "--parents", "-n", "1", &self.commit_sha],
        )?;
        if !parents.status.success() {
            return Err(LprmError::git_precondition(
                "Git could not inspect integration candidate parents",
            ));
        }
        let object_ids = parse_parent_ids(&parents.stdout)?;
        if object_ids
            != [
                self.commit_sha.clone(),
                self.target_sha.clone(),
                self.source_sha.clone(),
            ]
        {
            return Err(LprmError::git_precondition(
                "Integration candidate parents do not match the exact target and source",
            ));
        }
        Ok(())
    }

    fn cleanup(&mut self) -> Result<(), LprmError> {
        if self.container.is_none() {
            return Ok(());
        }
        let output = Command::new(&self.git.executable)
            .arg("-C")
            .arg(&self.repository)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output()
            .map_err(|_| LprmError::git_precondition("Git executable is unavailable"))?;
        if !output.status.success() {
            return Err(LprmError::git_precondition(
                "Git could not remove the disposable integration worktree",
            ));
        }
        self.container.take();
        Ok(())
    }
}

impl Drop for IntegrationCandidate {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
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

    /// Builds a detached no-fast-forward merge candidate from two exact commits.
    ///
    /// The returned guard owns the disposable worktree and removes it, including dirty check
    /// output, when dropped. The repository's target ref and coordinator checkout are untouched.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when either commit is invalid, the worktree or merge cannot
    /// be created, or the resulting commit does not have the exact target and source parents.
    pub fn create_integration_candidate(
        &self,
        repository: &Path,
        temporary_root: &Path,
        target_sha: &str,
        source_sha: &str,
    ) -> Result<IntegrationCandidate, LprmError> {
        self.verify_commit(repository, target_sha, "target")?;
        self.verify_commit(repository, source_sha, "source")?;
        if !temporary_root.is_dir() {
            return Err(LprmError::git_precondition(
                "Integration worktree root must be an existing directory",
            ));
        }
        let container = tempfile::Builder::new()
            .prefix("lprm-integration-")
            .tempdir_in(temporary_root)
            .map_err(|error| {
                LprmError::internal(format!(
                    "Unable to create disposable integration directory: {error}"
                ))
            })?;
        let path = container.path().join("worktree");
        let added = Command::new(&self.executable)
            .arg("-C")
            .arg(repository)
            .args(["worktree", "add", "--detach"])
            .arg(&path)
            .arg(target_sha)
            .output()
            .map_err(|_| LprmError::git_precondition("Git executable is unavailable"))?;
        if !added.status.success() {
            return Err(LprmError::git_precondition(
                "Git could not create the disposable integration worktree",
            ));
        }
        let mut candidate = IntegrationCandidate {
            git: self.clone(),
            repository: repository.to_owned(),
            container: Some(container),
            path,
            commit_sha: String::new(),
            tree_sha: String::new(),
            target_sha: target_sha.to_owned(),
            source_sha: source_sha.to_owned(),
        };
        candidate.merge_exact_source()?;
        Ok(candidate)
    }

    /// Merges an exact source into the checked-out target and verifies candidate equivalence.
    ///
    /// The operation rejects a changed target before invoking merge. A successful Git merge is
    /// reported as divergent rather than accepted when its parents, tree, branch ref, or worktree
    /// cleanliness differ from the tested candidate.
    ///
    /// # Errors
    ///
    /// Returns a stale-revision error for a target race, a validation error when Git safely rejects
    /// and aborts the merge, or a precondition/internal error for unsafe repository state.
    pub fn guarded_merge(
        &self,
        repository: &Path,
        target_branch: &str,
        target_before_sha: &str,
        source_sha: &str,
        candidate_tree_sha: &str,
    ) -> Result<GuardedMerge, LprmError> {
        self.verify_commit(repository, target_before_sha, "target")?;
        self.verify_commit(repository, source_sha, "source")?;
        self.verify_tree(repository, candidate_tree_sha)?;
        self.verify_guarded_merge_start(repository, target_branch, target_before_sha)?;

        let merged = Command::new(&self.executable)
            .arg("-C")
            .arg(repository)
            .args(["merge", "--no-ff", "--no-edit", "--no-gpg-sign"])
            .arg(source_sha)
            .env("GIT_AUTHOR_NAME", "LPRM Coordinator")
            .env("GIT_AUTHOR_EMAIL", "lprm@localhost")
            .env("GIT_COMMITTER_NAME", "LPRM Coordinator")
            .env("GIT_COMMITTER_EMAIL", "lprm@localhost")
            .output()
            .map_err(|_| LprmError::git_precondition("Git executable is unavailable"))?;
        if !merged.status.success() {
            self.abort_rejected_merge(repository, target_before_sha)?;
            return Err(LprmError::validation_failed(
                "Git rejected the guarded merge and the target was restored",
            ));
        }

        let evidence = self.inspect_integrated_merge(repository)?;
        let target_after = self.resolve_local_branch(repository, target_branch)?;
        let exact_parents =
            evidence.parents == [target_before_sha.to_owned(), source_sha.to_owned()];
        let verified = target_after == evidence.target_after_sha
            && evidence.tree_sha == candidate_tree_sha
            && exact_parents
            && evidence.worktree_clean;
        if verified {
            Ok(GuardedMerge::Verified(evidence))
        } else {
            Ok(GuardedMerge::Diverged(evidence))
        }
    }

    /// Inspects the current branch, HEAD, and dirty state of a registered worktree.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when a present path cannot be inspected as a worktree.
    pub fn observe_worktree(&self, worktree_path: &Path) -> Result<WorktreeObservation, LprmError> {
        if !worktree_path.exists() {
            return Ok(WorktreeObservation {
                present: false,
                actual_branch: None,
                head_sha: None,
                dirty: None,
            });
        }

        let head = self.run(worktree_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        if !head.status.success() {
            return Err(LprmError::git_precondition(format!(
                "Registered worktree cannot resolve HEAD: {}",
                worktree_path.display()
            )));
        }
        let head_sha = String::from_utf8_lossy(&head.stdout).trim().to_owned();
        if !is_object_id(&head_sha) {
            return Err(LprmError::git_precondition(
                "Git returned an invalid worktree HEAD",
            ));
        }

        let branch = self.run(
            worktree_path,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        )?;
        let actual_branch = branch
            .status
            .success()
            .then(|| String::from_utf8_lossy(&branch.stdout).trim().to_owned());
        let status = self.run(
            worktree_path,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
        if !status.status.success() {
            return Err(LprmError::git_precondition(
                "Registered worktree status inspection failed",
            ));
        }

        Ok(WorktreeObservation {
            present: true,
            actual_branch,
            head_sha: Some(head_sha),
            dirty: Some(!status.stdout.is_empty()),
        })
    }

    /// Reports whether a managed branch contains commits not reachable from the target SHA.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when refs cannot be inspected safely.
    pub fn has_unique_commits(
        &self,
        repository: &Path,
        target_sha: &str,
        branch: &str,
    ) -> Result<bool, LprmError> {
        if !is_object_id(target_sha) {
            return Err(LprmError::git_precondition(
                "Cleanup target is not a full commit object ID",
            ));
        }
        let valid = self.run(repository, &["check-ref-format", "--branch", branch])?;
        if !valid.status.success() {
            return Err(LprmError::git_precondition(
                "Registered worktree branch is invalid",
            ));
        }
        let range = format!("{target_sha}..refs/heads/{branch}");
        let output = self.run(repository, &["rev-list", "--count", &range])?;
        if !output.status.success() {
            return Err(LprmError::git_precondition(
                "Git could not inspect unique worktree commits",
            ));
        }
        let count = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u64>()
            .map_err(|_| LprmError::git_precondition("Git returned an invalid commit count"))?;
        Ok(count > 0)
    }

    /// Removes a clean linked worktree without force or branch deletion.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when Git refuses or cannot perform the removal.
    pub fn remove_linked_worktree(
        &self,
        repository: &Path,
        worktree_path: &Path,
    ) -> Result<(), LprmError> {
        let output = Command::new(&self.executable)
            .arg("-C")
            .arg(repository)
            .args(["worktree", "remove"])
            .arg(worktree_path)
            .output()
            .map_err(|_| LprmError::git_precondition("Git executable is unavailable"))?;
        if !output.status.success() {
            return Err(LprmError::git_precondition(
                "Git refused to remove the managed worktree",
            ));
        }
        Ok(())
    }

    /// Lists Git's registered worktrees using the stable porcelain NUL-delimited format.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when Git fails or returns malformed UTF-8 records.
    pub fn list_worktrees(&self, repository: &Path) -> Result<Vec<GitWorktree>, LprmError> {
        let output = self.run(repository, &["worktree", "list", "--porcelain", "-z"])?;
        if !output.status.success() {
            return Err(LprmError::git_precondition(
                "Git worktree registry inspection failed",
            ));
        }
        parse_worktree_porcelain(&output.stdout)
    }

    /// Collects source-only commits and tree differences for two exact commit object IDs.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when either object ID is invalid or missing, the histories
    /// are unrelated, Git fails, or a changed path cannot be represented by the UTF-8 JSON API.
    pub fn collect_diff(
        &self,
        repository: &Path,
        target_sha: &str,
        source_sha: &str,
    ) -> Result<DiffCollection, LprmError> {
        self.verify_commit(repository, target_sha, "target")?;
        self.verify_commit(repository, source_sha, "source")?;

        let merge_base = self.run(repository, &["merge-base", target_sha, source_sha])?;
        if !merge_base.status.success() {
            return if merge_base.status.code() == Some(1) {
                Err(LprmError::git_precondition(
                    "Source and target commit histories are unrelated",
                ))
            } else {
                Err(LprmError::git_precondition(
                    "Git could not compare source and target histories",
                ))
            };
        }

        // Source-only reachability is the documented rev-list set subtraction operation.
        // Source: https://git-scm.com/docs/git-rev-list#_description
        let excluded_target = format!("^{target_sha}");
        let commits = self.run(
            repository,
            &["rev-list", "--reverse", source_sha, &excluded_target, "--"],
        )?;
        if !commits.status.success() {
            return Err(LprmError::git_precondition(
                "Git could not list source-only commits",
            ));
        }
        let commits = parse_object_ids(&commits.stdout)?;

        // NUL-delimited numstat preserves unusual pathnames without Git's quoting rules.
        // Source: https://git-scm.com/docs/git-diff#Documentation/git-diff.txt--z
        let stats = self.run(
            repository,
            &[
                "diff",
                "--numstat",
                "-z",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                target_sha,
                source_sha,
                "--",
            ],
        )?;
        if !stats.status.success() {
            return Err(LprmError::git_precondition(
                "Git could not collect changed file statistics",
            ));
        }
        let files = parse_numstat(&stats.stdout)?;

        let patch = self.run(
            repository,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                target_sha,
                source_sha,
                "--",
            ],
        )?;
        if !patch.status.success() {
            return Err(LprmError::git_precondition(
                "Git could not collect the source patch",
            ));
        }
        let patch = String::from_utf8(patch.stdout).map_err(|_| {
            LprmError::git_precondition("Git patch contains a non-UTF-8 changed path")
        })?;

        Ok(DiffCollection {
            target_sha: target_sha.to_owned(),
            source_sha: source_sha.to_owned(),
            commits,
            files,
            patch,
        })
    }

    /// Computes the merge result for exact commits without touching an index or working tree.
    ///
    /// # Errors
    ///
    /// Returns a Git precondition error when either commit is invalid, Git cannot perform the
    /// preflight, or a successful result does not contain a full top-level tree object ID.
    pub fn merge_preflight(
        &self,
        repository: &Path,
        target_sha: &str,
        source_sha: &str,
    ) -> Result<MergePreflight, LprmError> {
        self.verify_commit(repository, target_sha, "target")?;
        self.verify_commit(repository, source_sha, "source")?;

        // Modern merge-tree does not read or write the worktree or index. Its documented status
        // contract is 0=clean, 1=conflict, everything else=error.
        // Source: https://git-scm.com/docs/git-merge-tree#_exit_status
        let output = self.run(
            repository,
            &["merge-tree", "--write-tree", target_sha, source_sha],
        )?;
        match output.status.code() {
            Some(0) => {
                let tree_sha = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                if !is_object_id(&tree_sha) {
                    return Err(LprmError::git_precondition(
                        "Git returned an invalid preflight tree object ID",
                    ));
                }
                Ok(MergePreflight::Clean { tree_sha })
            }
            Some(1) => Ok(MergePreflight::Conflict),
            _ => Err(LprmError::git_precondition(
                "Git could not perform merge conflict preflight",
            )),
        }
    }

    fn verify_commit(
        &self,
        repository: &Path,
        object_id: &str,
        role: &str,
    ) -> Result<(), LprmError> {
        if !is_object_id(object_id) {
            return Err(LprmError::git_precondition(format!(
                "Diff {role} is not a full commit object ID"
            )));
        }
        let revision = format!("{object_id}^{{commit}}");
        let output = self.run(repository, &["rev-parse", "--verify", &revision])?;
        if !output.status.success() {
            return Err(LprmError::git_precondition(format!(
                "Diff {role} commit does not exist"
            )));
        }
        Ok(())
    }

    fn verify_tree(&self, repository: &Path, object_id: &str) -> Result<(), LprmError> {
        if !is_object_id(object_id) {
            return Err(LprmError::git_precondition(
                "Candidate tree is not a full object ID",
            ));
        }
        let output = self.run(repository, &["cat-file", "-t", object_id])?;
        if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != "tree" {
            return Err(LprmError::git_precondition(
                "Candidate tree object does not exist",
            ));
        }
        Ok(())
    }

    fn verify_guarded_merge_start(
        &self,
        repository: &Path,
        target_branch: &str,
        target_before_sha: &str,
    ) -> Result<(), LprmError> {
        let observation = self.observe_worktree(repository)?;
        if observation.actual_branch.as_deref() != Some(target_branch) {
            return Err(LprmError::git_precondition(format!(
                "Coordinator checkout must have target branch {target_branch:?} checked out"
            )));
        }
        if observation.dirty != Some(false) {
            return Err(LprmError::validation_failed(
                "Coordinator checkout must be clean before integration",
            ));
        }
        let target_sha = self.resolve_local_branch(repository, target_branch)?;
        if target_sha != target_before_sha
            || observation.head_sha.as_deref() != Some(target_before_sha)
        {
            return Err(LprmError::stale_revision(
                "Target branch changed after candidate validation",
            ));
        }
        Ok(())
    }

    fn abort_rejected_merge(
        &self,
        repository: &Path,
        target_before_sha: &str,
    ) -> Result<(), LprmError> {
        let head = self.run(repository, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        if !head.status.success()
            || parse_single_object_id(&head.stdout, "guarded merge HEAD")? != target_before_sha
        {
            return Err(LprmError::internal(
                "Rejected merge changed target HEAD and requires reconciliation",
            ));
        }
        let merge_head = self.run(repository, &["rev-parse", "--verify", "MERGE_HEAD"])?;
        if merge_head.status.success() {
            let aborted = self.run(repository, &["merge", "--abort"])?;
            if !aborted.status.success() {
                return Err(LprmError::internal(
                    "Git could not abort the rejected guarded merge",
                ));
            }
        }
        let observation = self.observe_worktree(repository)?;
        if observation.head_sha.as_deref() != Some(target_before_sha)
            || observation.dirty != Some(false)
        {
            return Err(LprmError::internal(
                "Rejected merge left repository state requiring reconciliation",
            ));
        }
        Ok(())
    }

    fn inspect_integrated_merge(&self, repository: &Path) -> Result<IntegratedMerge, LprmError> {
        let observation = self.observe_worktree(repository)?;
        let commit_sha = observation
            .head_sha
            .ok_or_else(|| LprmError::internal("Integrated target HEAD is unavailable"))?;
        let tree = self.run(repository, &["rev-parse", "--verify", "HEAD^{tree}"])?;
        if !tree.status.success() {
            return Err(LprmError::internal("Integrated target tree is unavailable"));
        }
        let tree_sha = parse_single_object_id(&tree.stdout, "integrated tree")?;
        let parents = self.run(
            repository,
            &["rev-list", "--parents", "-n", "1", &commit_sha],
        )?;
        if !parents.status.success() {
            return Err(LprmError::internal(
                "Integrated target parents are unavailable",
            ));
        }
        let mut object_ids = parse_parent_ids(&parents.stdout)?;
        if object_ids.first() != Some(&commit_sha) {
            return Err(LprmError::internal(
                "Integrated target parent evidence is inconsistent",
            ));
        }
        object_ids.remove(0);
        Ok(IntegratedMerge {
            target_after_sha: commit_sha,
            tree_sha,
            parents: object_ids,
            worktree_clean: observation.dirty == Some(false),
        })
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

fn parse_worktree_porcelain(output: &[u8]) -> Result<Vec<GitWorktree>, LprmError> {
    let mut records = Vec::new();
    let mut current: Option<GitWorktree> = None;
    for field in output.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let Some(record) = current.take() {
                records.push(record);
            }
            continue;
        }
        let field = std::str::from_utf8(field).map_err(|_| {
            LprmError::git_precondition("Git worktree registry contains a non-UTF-8 path")
        })?;
        if let Some(path) = field.strip_prefix("worktree ") {
            if let Some(record) = current.replace(GitWorktree {
                path: PathBuf::from(path),
                head_sha: None,
                branch: None,
            }) {
                records.push(record);
            }
        } else if let Some(head) = field.strip_prefix("HEAD ") {
            if let Some(record) = current.as_mut() {
                record.head_sha = Some(head.to_owned());
            }
        } else if let Some(branch) = field.strip_prefix("branch refs/heads/")
            && let Some(record) = current.as_mut()
        {
            record.branch = Some(branch.to_owned());
        }
    }
    if let Some(record) = current {
        records.push(record);
    }
    Ok(records)
}

fn parse_object_ids(output: &[u8]) -> Result<Vec<String>, LprmError> {
    let output = std::str::from_utf8(output)
        .map_err(|_| LprmError::git_precondition("Git returned non-UTF-8 commit IDs"))?;
    output
        .lines()
        .map(|line| {
            if is_object_id(line) {
                Ok(line.to_owned())
            } else {
                Err(LprmError::git_precondition(
                    "Git returned an invalid commit object ID",
                ))
            }
        })
        .collect()
}

fn parse_single_object_id(output: &[u8], role: &str) -> Result<String, LprmError> {
    let object_id = std::str::from_utf8(output)
        .map_err(|_| LprmError::git_precondition(format!("Git returned a non-UTF-8 {role}")))?
        .trim();
    if is_object_id(object_id) {
        Ok(object_id.to_owned())
    } else {
        Err(LprmError::git_precondition(format!(
            "Git returned an invalid {role} object ID"
        )))
    }
}

fn parse_parent_ids(output: &[u8]) -> Result<Vec<String>, LprmError> {
    let output = std::str::from_utf8(output).map_err(|_| {
        LprmError::git_precondition("Git returned non-UTF-8 integration candidate parents")
    })?;
    output
        .split_whitespace()
        .map(|value| {
            if is_object_id(value) {
                Ok(value.to_owned())
            } else {
                Err(LprmError::git_precondition(
                    "Git returned an invalid integration candidate parent",
                ))
            }
        })
        .collect()
}

fn parse_numstat(output: &[u8]) -> Result<Vec<DiffFile>, LprmError> {
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut files = Vec::new();
    while let Some(record) = fields.next() {
        let first_tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| {
                LprmError::git_precondition("Git returned malformed changed file statistics")
            })?;
        let second_tab = record[first_tab + 1..]
            .iter()
            .position(|byte| *byte == b'\t')
            .map(|position| first_tab + 1 + position)
            .ok_or_else(|| {
                LprmError::git_precondition("Git returned malformed changed file statistics")
            })?;
        let added_lines = parse_numstat_count(&record[..first_tab])?;
        let deleted_lines = parse_numstat_count(&record[first_tab + 1..second_tab])?;
        let inline_path = &record[second_tab + 1..];
        let (old_path, path) = if inline_path.is_empty() {
            let old_path = fields.next().ok_or_else(|| {
                LprmError::git_precondition("Git omitted a renamed file's old path")
            })?;
            let path = fields.next().ok_or_else(|| {
                LprmError::git_precondition("Git omitted a renamed file's new path")
            })?;
            (Some(parse_diff_path(old_path)?), parse_diff_path(path)?)
        } else {
            (None, parse_diff_path(inline_path)?)
        };
        files.push(DiffFile {
            path,
            old_path,
            added_lines,
            deleted_lines,
        });
    }
    Ok(files)
}

fn parse_numstat_count(value: &[u8]) -> Result<Option<u64>, LprmError> {
    if value == b"-" {
        return Ok(None);
    }
    let value = std::str::from_utf8(value)
        .map_err(|_| LprmError::git_precondition("Git returned an invalid line count"))?;
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| LprmError::git_precondition("Git returned an invalid line count"))
}

fn parse_diff_path(value: &[u8]) -> Result<String, LprmError> {
    String::from_utf8(value.to_vec())
        .map_err(|_| LprmError::git_precondition("Changed paths must be valid UTF-8 in LPRM v1"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{Git, GitVersion, MergePreflight, parse_worktree_porcelain};

    fn run_git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(arguments)
            .status()
            .expect("git should start");
        assert!(status.success(), "git command failed: {arguments:?}");
    }

    fn git_stdout(path: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(arguments)
            .output()
            .expect("git should start");
        assert!(output.status.success(), "git command failed: {arguments:?}");
        String::from_utf8(output.stdout)
            .expect("git output should be UTF-8")
            .trim()
            .to_owned()
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

    #[test]
    fn parses_nul_delimited_worktree_porcelain() {
        let records = parse_worktree_porcelain(
            b"worktree /repo\0HEAD 0123456789012345678901234567890123456789\0branch refs/heads/dev\0\0",
        )
        .expect("porcelain should parse");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, Path::new("/repo"));
        assert_eq!(records[0].branch.as_deref(), Some("dev"));
    }

    #[test]
    fn collects_exact_diff_data_for_a_hostile_filename() {
        let repository = tempdir().expect("temporary directory should be created");
        run_git(
            repository.path(),
            &["init", "--quiet", "--initial-branch=dev"],
        );
        run_git(repository.path(), &["config", "user.name", "LPRM Test"]);
        run_git(
            repository.path(),
            &["config", "user.email", "lprm@example.invalid"],
        );
        fs::write(repository.path().join("README.md"), "fixture\n")
            .expect("fixture should be written");
        run_git(repository.path(), &["add", "--", "README.md"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "base"]);

        let git = Git::default();
        let target_sha = git
            .resolve_local_branch(repository.path(), "dev")
            .expect("target should resolve");
        run_git(repository.path(), &["checkout", "--quiet", "-b", "feature"]);
        let hostile_name = "--output\nwith\ttabs.txt";
        fs::write(repository.path().join(hostile_name), "first\nsecond\n")
            .expect("hostile fixture should be written");
        run_git(repository.path(), &["add", "--", hostile_name]);
        run_git(
            repository.path(),
            &["commit", "--quiet", "-m", "hostile filename"],
        );
        let source_sha = git
            .resolve_local_branch(repository.path(), "feature")
            .expect("source should resolve");

        let collected = git
            .collect_diff(repository.path(), &target_sha, &source_sha)
            .expect("diff should be collected");

        assert_eq!(collected.target_sha, target_sha);
        assert_eq!(collected.source_sha, source_sha);
        assert_eq!(collected.commits, [source_sha]);
        assert_eq!(collected.files.len(), 1);
        assert_eq!(collected.files[0].path, hostile_name);
        assert_eq!(collected.files[0].added_lines, Some(2));
        assert_eq!(collected.files[0].deleted_lines, Some(0));
        assert!(collected.patch.contains("+first"));
        assert!(collected.patch.contains("+second"));
    }

    #[test]
    fn rejects_unrelated_commit_histories() {
        let repository = tempdir().expect("temporary directory should be created");
        run_git(
            repository.path(),
            &["init", "--quiet", "--initial-branch=dev"],
        );
        run_git(repository.path(), &["config", "user.name", "LPRM Test"]);
        run_git(
            repository.path(),
            &["config", "user.email", "lprm@example.invalid"],
        );
        fs::write(repository.path().join("README.md"), "fixture\n")
            .expect("fixture should be written");
        run_git(repository.path(), &["add", "--", "README.md"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "base"]);

        let git = Git::default();
        let target_sha = git
            .resolve_local_branch(repository.path(), "dev")
            .expect("target should resolve");
        run_git(
            repository.path(),
            &["checkout", "--quiet", "--orphan", "other"],
        );
        run_git(repository.path(), &["read-tree", "--empty"]);
        fs::remove_file(repository.path().join("README.md"))
            .expect("old fixture should be removed");
        fs::write(repository.path().join("OTHER.md"), "unrelated\n")
            .expect("fixture should be written");
        run_git(repository.path(), &["add", "--", "OTHER.md"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "unrelated"]);
        let source_sha = git
            .resolve_local_branch(repository.path(), "other")
            .expect("source should resolve");

        let error = git
            .collect_diff(repository.path(), &target_sha, &source_sha)
            .expect_err("unrelated histories must be rejected");

        assert!(error.message.contains("unrelated"));
    }

    #[test]
    fn merge_preflight_reports_clean_without_mutating_checkout_or_index() {
        let repository = merge_fixture();
        let target_sha = git_stdout(repository.path(), &["rev-parse", "dev"]);
        run_git(repository.path(), &["checkout", "--quiet", "-b", "feature"]);
        fs::write(repository.path().join("feature.txt"), "feature\n")
            .expect("feature should be written");
        run_git(repository.path(), &["add", "--", "feature.txt"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "feature"]);
        let source_sha = git_stdout(repository.path(), &["rev-parse", "HEAD"]);
        let status_before = git_stdout(repository.path(), &["status", "--porcelain=v1"]);
        let index_before = git_stdout(repository.path(), &["write-tree"]);

        let result = Git::default()
            .merge_preflight(repository.path(), &target_sha, &source_sha)
            .expect("preflight should run");

        assert!(matches!(result, MergePreflight::Clean { .. }));
        assert_eq!(
            git_stdout(repository.path(), &["status", "--porcelain=v1"]),
            status_before
        );
        assert_eq!(git_stdout(repository.path(), &["write-tree"]), index_before);
    }

    #[test]
    fn merge_preflight_reports_conflict_without_mutating_checkout_or_index() {
        let repository = merge_fixture();
        run_git(repository.path(), &["checkout", "--quiet", "-b", "feature"]);
        fs::write(repository.path().join("shared.txt"), "feature\n")
            .expect("feature should be written");
        run_git(repository.path(), &["add", "--", "shared.txt"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "feature"]);
        let source_sha = git_stdout(repository.path(), &["rev-parse", "HEAD"]);
        run_git(repository.path(), &["checkout", "--quiet", "dev"]);
        fs::write(repository.path().join("shared.txt"), "target\n")
            .expect("target should be written");
        run_git(repository.path(), &["add", "--", "shared.txt"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "target"]);
        let target_sha = git_stdout(repository.path(), &["rev-parse", "HEAD"]);
        let status_before = git_stdout(repository.path(), &["status", "--porcelain=v1"]);
        let index_before = git_stdout(repository.path(), &["write-tree"]);

        let result = Git::default()
            .merge_preflight(repository.path(), &target_sha, &source_sha)
            .expect("preflight should run");

        assert_eq!(result, MergePreflight::Conflict);
        assert_eq!(
            git_stdout(repository.path(), &["status", "--porcelain=v1"]),
            status_before
        );
        assert_eq!(git_stdout(repository.path(), &["write-tree"]), index_before);
    }

    fn merge_fixture() -> tempfile::TempDir {
        let repository = tempdir().expect("temporary directory should be created");
        run_git(
            repository.path(),
            &["init", "--quiet", "--initial-branch=dev"],
        );
        run_git(repository.path(), &["config", "user.name", "LPRM Test"]);
        run_git(
            repository.path(),
            &["config", "user.email", "lprm@example.invalid"],
        );
        fs::write(repository.path().join("shared.txt"), "base\n").expect("base should be written");
        run_git(repository.path(), &["add", "--", "shared.txt"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "base"]);
        repository
    }
}
