//! Validation contract evaluation.
//!
//! Projects can declare marker strings that must appear in validation
//! output. A command that exits 0 but emits none of the expected markers
//! can still be treated as a failure because it likely ran the wrong
//! code path or bypassed validation.

/// Parsed validation contract settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractConfig {
    /// Declared output markers.
    pub markers: Vec<String>,
    /// Whether at least one marker is enough. When false, every marker
    /// must appear.
    pub require_at_least_one: bool,
    /// Whether violation should force the result to fail.
    pub enforce: bool,
}

impl ContractConfig {
    /// Construct a contract with Python-compatible defaults.
    #[must_use]
    pub fn new(markers: Vec<String>) -> Self {
        Self {
            markers,
            require_at_least_one: true,
            enforce: true,
        }
    }
}

/// Outcome of contract evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractEvaluation {
    /// Markers observed by the streaming layer.
    pub seen: Vec<String>,
    /// Required markers that were not observed.
    pub missing: Vec<String>,
    /// Whether the declared contract was violated.
    pub violated: bool,
    /// Whether violation should force a failure.
    pub enforce: bool,
    /// Human-readable violation message.
    pub message: Option<String>,
}

impl ContractEvaluation {
    /// Return whether this evaluation should override a passing exit.
    #[must_use]
    pub fn should_force_fail(&self) -> bool {
        self.violated && self.enforce
    }
}

/// Compare observed markers against an optional contract.
#[must_use]
pub fn evaluate_contract(
    contract: Option<&ContractConfig>,
    seen_markers: &[String],
) -> ContractEvaluation {
    let seen = seen_markers.to_vec();
    let Some(contract) = contract else {
        return no_contract(seen);
    };
    if contract.markers.is_empty() {
        return no_contract(seen);
    }

    let mut missing = contract
        .markers
        .iter()
        .filter(|marker| !seen_markers.contains(marker))
        .cloned()
        .collect::<Vec<_>>();

    if contract.require_at_least_one {
        let violated = !contract
            .markers
            .iter()
            .any(|marker| seen_markers.contains(marker));
        let message = violated.then(|| {
            format!(
                "Validation contract requires at least one of {:?} to appear in the output. None were observed.",
                contract.markers
            )
        });
        if !violated {
            missing.clear();
        }
        return ContractEvaluation {
            seen,
            missing,
            violated,
            enforce: contract.enforce,
            message,
        };
    }

    let violated = !missing.is_empty();
    let message = violated.then(|| {
        format!(
            "Validation contract requires every declared marker to appear in the output. Missing: {missing:?}."
        )
    });
    ContractEvaluation {
        seen,
        missing,
        violated,
        enforce: contract.enforce,
        message,
    }
}

/// Extract marker list for the streaming layer.
#[must_use]
pub fn required_markers(contract: Option<&ContractConfig>) -> Vec<String> {
    contract.map_or_else(Vec::new, |contract| contract.markers.clone())
}

fn no_contract(seen: Vec<String>) -> ContractEvaluation {
    ContractEvaluation {
        seen,
        missing: Vec::new(),
        violated: false,
        enforce: false,
        message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ContractConfig, evaluate_contract, required_markers};

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn no_contract_is_noop() {
        let result = evaluate_contract(None, &strings(&["anything"]));
        assert_eq!(result.seen, strings(&["anything"]));
        assert!(result.missing.is_empty());
        assert!(!result.violated);
        assert!(!result.enforce);
        assert!(!result.should_force_fail());
        assert_eq!(result.message, None);
    }

    #[test]
    fn empty_marker_contract_is_noop() {
        let config = ContractConfig::new(Vec::new());
        let result = evaluate_contract(Some(&config), &strings(&["anything"]));
        assert!(!result.violated);
        assert!(!result.enforce);
    }

    #[test]
    fn at_least_one_contract_passes_when_one_marker_seen() {
        let config = ContractConfig::new(strings(&["smoke", "full"]));
        let result = evaluate_contract(Some(&config), &strings(&["smoke"]));
        assert!(!result.violated);
        assert!(result.missing.is_empty());
        assert!(!result.should_force_fail());
    }

    #[test]
    fn at_least_one_contract_fails_when_none_seen() {
        let config = ContractConfig::new(strings(&["smoke", "full"]));
        let result = evaluate_contract(Some(&config), &[]);
        assert!(result.violated);
        assert_eq!(result.missing, strings(&["smoke", "full"]));
        assert!(result.should_force_fail());
        assert!(result.message.expect("message").contains("at least one"));
    }

    #[test]
    fn warn_only_contract_does_not_force_fail() {
        let mut config = ContractConfig::new(strings(&["smoke"]));
        config.enforce = false;
        let result = evaluate_contract(Some(&config), &[]);
        assert!(result.violated);
        assert!(!result.enforce);
        assert!(!result.should_force_fail());
    }

    #[test]
    fn require_all_contract_reports_missing_markers() {
        let mut config = ContractConfig::new(strings(&["smoke", "full"]));
        config.require_at_least_one = false;
        let result = evaluate_contract(Some(&config), &strings(&["smoke"]));
        assert!(result.violated);
        assert_eq!(result.missing, strings(&["full"]));
        assert!(result.should_force_fail());
        assert!(result.message.expect("message").contains("every declared"));
    }

    #[test]
    fn require_all_contract_passes_when_all_seen() {
        let mut config = ContractConfig::new(strings(&["smoke", "full"]));
        config.require_at_least_one = false;
        let result = evaluate_contract(Some(&config), &strings(&["full", "smoke"]));
        assert!(!result.violated);
        assert!(result.missing.is_empty());
    }

    #[test]
    fn extra_markers_are_fine() {
        let config = ContractConfig::new(strings(&["smoke"]));
        let result = evaluate_contract(Some(&config), &strings(&["smoke", "extra"]));
        assert!(!result.violated);
        assert_eq!(result.seen, strings(&["smoke", "extra"]));
    }

    #[test]
    fn required_markers_returns_declared_marker_list() {
        let config = ContractConfig::new(strings(&["smoke", "full"]));
        assert_eq!(required_markers(Some(&config)), strings(&["smoke", "full"]));
        assert!(required_markers(None).is_empty());
    }
}
