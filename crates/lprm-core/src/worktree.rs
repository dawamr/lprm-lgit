use std::fs;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::LprmError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeSpec {
    id: String,
    short_id: String,
    slug: String,
    branch: String,
    directory_name: String,
}

impl WorktreeSpec {
    /// Generates a unique managed worktree identity and safe Git branch name.
    ///
    /// # Errors
    ///
    /// Returns an invalid-argument error when `task` is blank or has no slug characters.
    pub fn new(task: &str) -> Result<Self, LprmError> {
        if task.trim().is_empty() {
            return Err(LprmError::invalid_argument(
                "Worktree task must not be blank",
            ));
        }
        let slug = slugify(task);
        if slug.is_empty() {
            return Err(LprmError::invalid_argument(
                "Worktree task must contain an ASCII letter or number",
            ));
        }

        let id = Uuid::now_v7().simple().to_string();
        let short_id = id[..8].to_owned();
        let directory_name = format!("{short_id}-{slug}");
        let branch = format!("lprm/{directory_name}");
        Ok(Self {
            id,
            short_id,
            slug,
            branch,
            directory_name,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn short_id(&self) -> &str {
        &self.short_id
    }

    pub fn slug(&self) -> &str {
        &self.slug
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub fn directory_name(&self) -> &str {
        &self.directory_name
    }
}

/// Canonicalizes a managed worktree and proves it is a direct, non-symlink child of its root.
///
/// # Errors
///
/// Returns a Git precondition error for missing, symlinked, nested, or outside-root paths.
pub fn validate_managed_worktree_path(
    configured_root: &Path,
    registered_path: &Path,
) -> Result<PathBuf, LprmError> {
    let root = fs::canonicalize(configured_root).map_err(|_| {
        LprmError::git_precondition("Configured worktree root is missing or inaccessible")
    })?;
    let metadata = fs::symlink_metadata(registered_path).map_err(|_| {
        LprmError::git_precondition("Registered worktree path is missing or inaccessible")
    })?;
    if metadata.file_type().is_symlink() {
        return Err(LprmError::git_precondition(
            "Registered worktree path must not be a symlink",
        ));
    }
    let path = fs::canonicalize(registered_path).map_err(|_| {
        LprmError::git_precondition("Registered worktree path cannot be canonicalized")
    })?;
    if path.parent() != Some(root.as_path()) {
        return Err(LprmError::git_precondition(
            "Registered worktree path is outside the configured root",
        ));
    }
    Ok(path)
}

fn slugify(task: &str) -> String {
    let mut slug = String::with_capacity(task.len().min(40));
    let mut pending_separator = false;
    for character in task.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !slug.is_empty() && slug.len() < 40 {
                slug.push('-');
            }
            pending_separator = false;
            if slug.len() < 40 {
                slug.push(character.to_ascii_lowercase());
            }
        } else if !slug.is_empty() {
            pending_separator = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{WorktreeSpec, slugify, validate_managed_worktree_path};

    #[test]
    fn slug_is_bounded_and_collapses_separators() {
        assert_eq!(slugify("  Add USER---roles  "), "add-user-roles");
        assert!(slugify(&"a".repeat(100)).len() <= 40);
    }

    #[test]
    fn generated_names_are_unique_and_ref_safe() {
        let first = WorktreeSpec::new("Add user roles").expect("task should be valid");
        let second = WorktreeSpec::new("Add user roles").expect("task should be valid");

        assert_ne!(first.id(), second.id());
        assert!(first.branch().starts_with("lprm/"));
        assert!(first.branch().ends_with("-add-user-roles"));
        assert!(!first.directory_name().contains('/'));
    }

    #[test]
    fn managed_path_must_be_a_direct_child_of_the_root() {
        let fixture = tempdir().expect("temporary directory should be created");
        let root = fixture.path().join("root");
        let managed = root.join("managed");
        let outside = fixture.path().join("outside");
        fs::create_dir_all(&managed).expect("managed path should be created");
        fs::create_dir(&outside).expect("outside path should be created");

        assert_eq!(
            validate_managed_worktree_path(&root, &managed).expect("managed path should validate"),
            fs::canonicalize(&managed).expect("managed path should canonicalize")
        );
        assert!(validate_managed_worktree_path(&root, &outside).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn managed_path_rejects_a_symlink() {
        use std::os::unix::fs::symlink;

        let fixture = tempdir().expect("temporary directory should be created");
        let root = fixture.path().join("root");
        let target = fixture.path().join("target");
        let linked = root.join("linked");
        fs::create_dir(&root).expect("root should be created");
        fs::create_dir(&target).expect("target should be created");
        symlink(&target, &linked).expect("symlink should be created");

        assert!(validate_managed_worktree_path(&root, &linked).is_err());
    }
}
