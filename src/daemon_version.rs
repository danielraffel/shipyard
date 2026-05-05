//! Shared daemon/CLI version comparison helpers.

use std::path::Path;

use serde_json::Value;

use crate::daemon_ipc::read_daemon_status;

/// Relationship between the running daemon and the current CLI binary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DaemonVersionRelation {
    /// The daemon reports the same version as the CLI.
    Match {
        /// Daemon version without a leading `v`.
        daemon_version: String,
        /// CLI version as displayed to the user.
        cli_version: String,
    },
    /// The daemon reports a different version than the CLI.
    Mismatch {
        /// Daemon version without a leading `v`.
        daemon_version: String,
        /// CLI version as displayed to the user.
        cli_version: String,
    },
    /// The daemon is reachable but predates `shipyard_version`.
    UnknownDaemonVersion {
        /// CLI version as displayed to the user.
        cli_version: String,
    },
}

/// Read daemon status and compare its version to `cli_version`.
#[must_use]
pub fn read_daemon_version_relation(
    state_dir: &Path,
    cli_version: &str,
) -> Option<DaemonVersionRelation> {
    let status = read_daemon_status(state_dir)?;
    compare_daemon_version(Some(&status), cli_version)
}

/// Compare a daemon status payload to the current CLI version.
#[must_use]
pub fn compare_daemon_version(
    status: Option<&Value>,
    cli_version: &str,
) -> Option<DaemonVersionRelation> {
    let status = status?;
    let cli_version = cli_version.to_owned();
    let cli_normalized = cli_version.trim_start_matches('v');
    let Some(daemon_version) = status.get("shipyard_version").and_then(Value::as_str) else {
        return Some(DaemonVersionRelation::UnknownDaemonVersion { cli_version });
    };
    if daemon_version == cli_normalized {
        Some(DaemonVersionRelation::Match {
            daemon_version: daemon_version.to_owned(),
            cli_version,
        })
    } else {
        Some(DaemonVersionRelation::Mismatch {
            daemon_version: daemon_version.to_owned(),
            cli_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{DaemonVersionRelation, compare_daemon_version};

    #[test]
    fn compares_match_mismatch_unknown_and_absent_status() {
        assert_eq!(
            compare_daemon_version(
                Some(&serde_json::json!({"shipyard_version": "0.1.0"})),
                "v0.1.0"
            ),
            Some(DaemonVersionRelation::Match {
                daemon_version: "0.1.0".to_owned(),
                cli_version: "v0.1.0".to_owned(),
            })
        );
        assert_eq!(
            compare_daemon_version(
                Some(&serde_json::json!({"shipyard_version": "0.0.9"})),
                "v0.1.0"
            ),
            Some(DaemonVersionRelation::Mismatch {
                daemon_version: "0.0.9".to_owned(),
                cli_version: "v0.1.0".to_owned(),
            })
        );
        assert_eq!(
            compare_daemon_version(Some(&serde_json::json!({})), "v0.1.0"),
            Some(DaemonVersionRelation::UnknownDaemonVersion {
                cli_version: "v0.1.0".to_owned(),
            })
        );
        assert_eq!(compare_daemon_version(None, "v0.1.0"), None);
    }
}
