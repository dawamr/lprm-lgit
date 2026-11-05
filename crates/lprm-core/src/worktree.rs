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
    use super::{WorktreeSpec, slugify};

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
}
