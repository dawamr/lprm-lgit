use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::LprmError;

pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub repository: RepositoryConfig,
    #[serde(default)]
    pub merge: MergeConfig,
    #[serde(default)]
    pub checks: ChecksConfig,
    #[serde(default)]
    pub review: ReviewConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            repository: RepositoryConfig::default(),
            merge: MergeConfig::default(),
            checks: ChecksConfig::default(),
            review: ReviewConfig::default(),
            policy: PolicyConfig::default(),
        }
    }
}

impl Config {
    /// Parses and validates a schema-versioned TOML document.
    ///
    /// # Errors
    ///
    /// Returns an invalid-argument error for malformed TOML or unsupported values.
    pub fn from_toml(source: &str) -> Result<Self, LprmError> {
        let config: Self = toml::from_str(source).map_err(|error| {
            LprmError::invalid_argument(format!("Invalid LPRM config: {error}"))
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Loads `.lprm.toml` when present, otherwise returns validated defaults.
    ///
    /// # Errors
    ///
    /// Returns an invalid-argument error when the file cannot be read or validated.
    pub fn load_for_repository(repository_root: &Path) -> Result<Self, LprmError> {
        let path = repository_root.join(".lprm.toml");
        if !path.exists() {
            let config = Self::default();
            config.validate()?;
            return Ok(config);
        }

        let source = fs::read_to_string(&path).map_err(|error| {
            LprmError::invalid_argument(format!(
                "Unable to read LPRM config at {}: {error}",
                path.display()
            ))
        })?;
        Self::from_toml(&source)
    }

    /// Checks cross-field constraints that Serde cannot express.
    ///
    /// # Errors
    ///
    /// Returns an invalid-argument error describing the first invalid constraint.
    pub fn validate(&self) -> Result<(), LprmError> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(LprmError::invalid_argument(format!(
                "Unsupported config schema version {}; expected {CONFIG_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        if !valid_branch_name_candidate(&self.repository.target_branch) {
            return Err(LprmError::invalid_argument(
                "repository.target_branch is empty or unsafe",
            ));
        }
        if self.repository.worktree_root.as_os_str().is_empty() {
            return Err(LprmError::invalid_argument(
                "repository.worktree_root must not be empty",
            ));
        }
        if self.checks.timeout_seconds == 0 {
            return Err(LprmError::invalid_argument(
                "checks.timeout_seconds must be greater than zero",
            ));
        }

        let mut required_names = BTreeSet::new();
        for name in &self.checks.required {
            if !required_names.insert(name) {
                return Err(LprmError::invalid_argument(format!(
                    "checks.required contains duplicate name {name:?}"
                )));
            }
            if !self.checks.commands.contains_key(name) {
                return Err(LprmError::invalid_argument(format!(
                    "checks.required references missing command {name:?}"
                )));
            }
        }

        for (name, command) in &self.checks.commands {
            if command.is_empty() || command.iter().any(String::is_empty) {
                return Err(LprmError::invalid_argument(format!(
                    "checks.commands.{name} must contain non-empty arguments"
                )));
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    #[serde(default = "default_target_branch")]
    pub target_branch: String,
    #[serde(default = "default_worktree_root")]
    pub worktree_root: PathBuf,
    #[serde(default = "default_true")]
    pub require_clean_target: bool,
}

impl Default for RepositoryConfig {
    fn default() -> Self {
        Self {
            target_branch: default_target_branch(),
            worktree_root: default_worktree_root(),
            require_clean_target: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeStrategy {
    #[default]
    NoFf,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MergeConfig {
    #[serde(default)]
    pub strategy: MergeStrategy,
    #[serde(default = "default_true")]
    pub serialize: bool,
    #[serde(default = "default_true")]
    pub revalidate_on_target_change: bool,
    #[serde(default)]
    pub cleanup_after_merge: bool,
    #[serde(default)]
    pub allow_fast_forward: bool,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            strategy: MergeStrategy::NoFf,
            serialize: true,
            revalidate_on_target_change: true,
            cleanup_after_merge: false,
            allow_fast_forward: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChecksConfig {
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_required_checks")]
    pub required: Vec<String>,
    #[serde(default = "default_check_commands")]
    pub commands: BTreeMap<String, Vec<String>>,
}

impl Default for ChecksConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: default_timeout_seconds(),
            required: default_required_checks(),
            commands: default_check_commands(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewMode {
    #[default]
    Rules,
    External,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    #[serde(default)]
    pub mode: ReviewMode,
    #[serde(default = "default_approval_risks")]
    pub require_approval_for_risk: Vec<RiskLevel>,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            mode: ReviewMode::Rules,
            require_approval_for_risk: default_approval_risks(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    #[serde(default = "default_protected_paths")]
    pub protected_paths: Vec<String>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            protected_paths: default_protected_paths(),
        }
    }
}

fn valid_branch_name_candidate(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && !value.starts_with('-')
        && !value.ends_with('/')
        && !value.contains("..")
        && !value.chars().any(char::is_control)
}

fn default_true() -> bool {
    true
}

fn default_target_branch() -> String {
    "dev".to_owned()
}

fn default_worktree_root() -> PathBuf {
    PathBuf::from("../.lprm-worktrees")
}

const fn default_timeout_seconds() -> u64 {
    900
}

fn default_required_checks() -> Vec<String> {
    vec!["format".to_owned(), "test".to_owned()]
}

fn default_check_commands() -> BTreeMap<String, Vec<String>> {
    BTreeMap::from([
        (
            "format".to_owned(),
            ["cargo", "fmt", "--all", "--", "--check"]
                .map(str::to_owned)
                .to_vec(),
        ),
        (
            "test".to_owned(),
            ["cargo", "test", "--workspace", "--locked"]
                .map(str::to_owned)
                .to_vec(),
        ),
    ])
}

fn default_approval_risks() -> Vec<RiskLevel> {
    vec![RiskLevel::High, RiskLevel::Critical]
}

fn default_protected_paths() -> Vec<String> {
    ["migrations/**", ".github/**", "infra/**", "auth/**"]
        .map(str::to_owned)
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::{CONFIG_SCHEMA_VERSION, Config};

    #[test]
    fn defaults_are_valid_and_match_schema_one() {
        let config = Config::default();

        assert_eq!(config.schema_version, CONFIG_SCHEMA_VERSION);
        config.validate().expect("defaults should be valid");
    }

    #[test]
    fn required_checks_must_have_commands() {
        let error = Config::from_toml(
            r#"
                schema_version = 1
                [checks]
                required = ["missing"]
                commands = {}
            "#,
        )
        .expect_err("missing command should fail");

        assert!(error.message.contains("missing command"));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let error = Config::from_toml("schema_version = 1\nunknown = true\n")
            .expect_err("unknown field should fail");

        assert!(error.message.contains("unknown field"));
    }
}
