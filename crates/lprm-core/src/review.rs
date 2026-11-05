//! Review findings, protected-path risk classification, and admission policy evaluation.

use serde::{Deserialize, Serialize};

use crate::LprmError;
use crate::config::{Config, ReviewMode, RiskLevel};

pub const REVIEW_SCHEMA_VERSION: u32 = 1;
const MAX_FINDINGS: usize = 500;
const MAX_TEXT_BYTES: usize = 4096;

/// Finding severity shares the ordered risk scale used by policy.
pub type Severity = RiskLevel;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Pass,
    ChangesRequested,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerIdentity {
    /// `rules` for the built-in reviewer, `external` for an adapter subprocess.
    pub kind: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub severity: Severity,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub resolved: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewReport {
    pub schema_version: u32,
    pub reviewer: ReviewerIdentity,
    pub verdict: ReviewVerdict,
    pub findings: Vec<Finding>,
}

impl ReviewReport {
    /// Parses and validates a versioned report, such as an external reviewer's stdout.
    ///
    /// # Errors
    ///
    /// Returns a validation error for malformed JSON, an unsupported schema, or invalid fields.
    pub fn from_json(bytes: &[u8]) -> Result<Self, LprmError> {
        let report: Self = serde_json::from_slice(bytes).map_err(|error| {
            LprmError::validation_failed(format!("Malformed review report: {error}"))
        })?;
        report.validate()?;
        Ok(report)
    }

    /// Checks schema version, identity, and per-finding invariants.
    ///
    /// # Errors
    ///
    /// Returns a validation error describing the first invalid field.
    pub fn validate(&self) -> Result<(), LprmError> {
        let invalid = |message: &str| Err(LprmError::validation_failed(message.to_owned()));
        if self.schema_version != REVIEW_SCHEMA_VERSION {
            return invalid("Unsupported review schema version");
        }
        if self.reviewer.kind.trim().is_empty() || self.reviewer.name.trim().is_empty() {
            return invalid("Reviewer identity must not be empty");
        }
        if self.findings.len() > MAX_FINDINGS {
            return invalid("Review report has too many findings");
        }
        for finding in &self.findings {
            if finding.code.trim().is_empty() || finding.message.trim().is_empty() {
                return invalid("Finding code and message must not be empty");
            }
            if finding.line == Some(0) {
                return invalid("Finding line numbers start at 1");
            }
            let oversized = |text: &str| text.len() > MAX_TEXT_BYTES;
            if oversized(&finding.code)
                || oversized(&finding.message)
                || finding.file.as_deref().is_some_and(oversized)
                || finding.suggestion.as_deref().is_some_and(oversized)
            {
                return invalid("Finding text exceeds the size limit");
            }
        }
        Ok(())
    }

    /// Highest severity among findings that are still unresolved.
    #[must_use]
    pub fn unresolved_severity(&self) -> Option<Severity> {
        self.findings
            .iter()
            .filter(|finding| !finding.resolved)
            .map(|finding| finding.severity)
            .max()
    }
}

/// Matches `pattern` against a slash-separated path. `**` crosses directories, `*` and `?` do not.
#[must_use]
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn walk(pattern: &[u8], path: &[u8]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((b'*', rest)) if rest.first() == Some(&b'*') => {
                let rest = &rest[1..];
                let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                (0..=path.len()).any(|skip| walk(rest, &path[skip..]))
            }
            Some((b'*', rest)) => {
                let limit = path
                    .iter()
                    .position(|byte| *byte == b'/')
                    .unwrap_or(path.len());
                (0..=limit).any(|skip| walk(rest, &path[skip..]))
            }
            Some((b'?', rest)) => {
                path.first().is_some_and(|byte| *byte != b'/') && walk(rest, &path[1..])
            }
            Some((byte, rest)) => path.first() == Some(byte) && walk(rest, &path[1..]),
        }
    }
    walk(pattern.as_bytes(), path.as_bytes())
}

/// Built-in deterministic reviewer: protected paths are high-severity findings.
#[must_use]
pub fn rules_review(paths: &[String], protected_paths: &[String]) -> ReviewReport {
    let findings: Vec<Finding> = paths
        .iter()
        .filter(|path| protected_paths.iter().any(|glob| glob_match(glob, path)))
        .map(|path| Finding {
            severity: Severity::High,
            file: Some(path.clone()),
            line: None,
            code: "PROTECTED_PATH".to_owned(),
            message: format!("{path} matches a protected path policy"),
            suggestion: Some("Obtain an eligible approval before queueing".to_owned()),
            resolved: false,
        })
        .collect();
    ReviewReport {
        schema_version: REVIEW_SCHEMA_VERSION,
        reviewer: ReviewerIdentity {
            kind: "rules".to_owned(),
            name: "lprm-rules".to_owned(),
        },
        verdict: if findings.is_empty() {
            ReviewVerdict::Pass
        } else {
            ReviewVerdict::ChangesRequested
        },
        findings,
    }
}

/// An approval recorded against one exact revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApprovalEvidence {
    pub actor: String,
    pub kind: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PolicyAssessment {
    pub risk: RiskLevel,
    pub approval_required: bool,
    pub required_reviews_passed: bool,
    pub required_approvals_present: bool,
    pub blockers: Vec<String>,
}

/// Evaluates review and approval gates for the current revision only.
///
/// `reviews` and `approvals` must already be filtered to the latest revision, which is what makes
/// a new source, target, or policy revision invalidate them.
#[must_use]
pub fn assess_policy(
    config: &Config,
    contributor: &str,
    paths: &[String],
    reviews: &[ReviewReport],
    approvals: &[ApprovalEvidence],
) -> PolicyAssessment {
    let protected = rules_review(paths, &config.policy.protected_paths);
    let path_risk = protected.unresolved_severity().unwrap_or(RiskLevel::Low);
    let finding_risk = reviews
        .iter()
        .filter_map(ReviewReport::unresolved_severity)
        .max()
        .unwrap_or(RiskLevel::Info);
    let risk = path_risk.max(finding_risk);
    let approval_required = config.review.require_approval_for_risk.contains(&risk);
    let eligible_approval_present = approvals
        .iter()
        .any(|approval| approval.actor != contributor);
    let mut blockers = Vec::new();
    let external_missing = config.review.mode == ReviewMode::External
        && !reviews
            .iter()
            .any(|review| review.reviewer.kind == "external");
    if external_missing {
        blockers.push("External review has not run for this revision".to_owned());
    }
    let unresolved_blocking = reviews.iter().any(|review| {
        review.verdict == ReviewVerdict::ChangesRequested
            || review
                .unresolved_severity()
                .is_some_and(|severity| severity >= RiskLevel::High)
    });
    if unresolved_blocking && !eligible_approval_present {
        blockers.push("Unresolved review findings need an eligible approval".to_owned());
    }
    if approval_required && !eligible_approval_present {
        blockers.push(format!(
            "Risk {risk:?} requires an approval from someone other than {contributor}"
        ));
    }
    PolicyAssessment {
        risk,
        approval_required,
        required_reviews_passed: !external_missing
            && (!unresolved_blocking || eligible_approval_present),
        required_approvals_present: !approval_required || eligible_approval_present,
        blockers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::default()
    }

    fn paths(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn globs_follow_directory_semantics() {
        assert!(glob_match("migrations/**", "migrations/001/up.sql"));
        assert!(glob_match("**/*.rs", "crates/a/src/lib.rs"));
        assert!(glob_match("src/*.rs", "src/lib.rs"));
        assert!(!glob_match("src/*.rs", "src/deep/lib.rs"));
        assert!(!glob_match("auth/**", "author/file"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "a/c"));
    }

    #[test]
    fn protected_paths_raise_risk_and_need_a_separate_approver() {
        let changed = paths(&["auth/login.rs", "README.md"]);
        let reviews = [rules_review(&changed, &config().policy.protected_paths)];

        let blocked = assess_policy(&config(), "codex", &changed, &reviews, &[]);
        assert_eq!(blocked.risk, RiskLevel::High);
        assert!(!blocked.required_approvals_present);
        assert!(!blocked.required_reviews_passed);

        let self_approved = [ApprovalEvidence {
            actor: "codex".to_owned(),
            kind: "manual".to_owned(),
            reason: "me".to_owned(),
        }];
        let still_blocked = assess_policy(&config(), "codex", &changed, &reviews, &self_approved);
        assert!(!still_blocked.required_approvals_present);

        let approved = [ApprovalEvidence {
            actor: "operator".to_owned(),
            kind: "manual".to_owned(),
            reason: "reviewed".to_owned(),
        }];
        let allowed = assess_policy(&config(), "codex", &changed, &reviews, &approved);
        assert!(allowed.required_approvals_present && allowed.required_reviews_passed);
        assert_eq!(allowed.blockers, Vec::<String>::new());
    }

    #[test]
    fn low_risk_changes_need_no_approval() {
        let changed = paths(&["src/lib.rs"]);
        let assessment = assess_policy(&config(), "codex", &changed, &[], &[]);
        assert_eq!(assessment.risk, RiskLevel::Low);
        assert!(assessment.required_approvals_present && assessment.required_reviews_passed);
    }

    #[test]
    fn external_findings_can_block_low_risk_paths() {
        let report = ReviewReport {
            schema_version: 1,
            reviewer: ReviewerIdentity {
                kind: "external".to_owned(),
                name: "bot".to_owned(),
            },
            verdict: ReviewVerdict::ChangesRequested,
            findings: vec![Finding {
                severity: Severity::Critical,
                file: Some("src/lib.rs".to_owned()),
                line: Some(3),
                code: "SECRET".to_owned(),
                message: "hard-coded secret".to_owned(),
                suggestion: None,
                resolved: false,
            }],
        };
        let changed = paths(&["src/lib.rs"]);
        let assessment = assess_policy(&config(), "codex", &changed, &[report], &[]);
        assert_eq!(assessment.risk, RiskLevel::Critical);
        assert!(!assessment.required_reviews_passed);
        assert!(!assessment.required_approvals_present);
    }

    #[test]
    fn report_validation_rejects_bad_input() {
        let valid = r#"{"schema_version":1,"reviewer":{"kind":"external","name":"bot"},"verdict":"pass","findings":[]}"#;
        assert!(ReviewReport::from_json(valid.as_bytes()).is_ok());
        for bad in [
            r#"{"schema_version":2,"reviewer":{"kind":"x","name":"y"},"verdict":"pass","findings":[]}"#,
            r#"{"schema_version":1,"reviewer":{"kind":"","name":"y"},"verdict":"pass","findings":[]}"#,
            r#"{"schema_version":1,"reviewer":{"kind":"x","name":"y"},"verdict":"pass","findings":[],"extra":1}"#,
            r#"{"schema_version":1,"reviewer":{"kind":"x","name":"y"},"verdict":"pass","findings":[{"severity":"low","file":null,"line":0,"code":"C","message":"m"}]}"#,
            "not json",
        ] {
            assert!(ReviewReport::from_json(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn report_round_trips_through_json() {
        let report = rules_review(&paths(&["infra/main.tf"]), &config().policy.protected_paths);
        let json = serde_json::to_vec(&report).unwrap();
        assert_eq!(ReviewReport::from_json(&json).unwrap(), report);
    }
}
