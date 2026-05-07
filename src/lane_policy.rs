//! Advisory-vs-required lane policy resolution.
//!
//! Shipyard treats every validation target as merge-blocking by default.
//! A target can opt into advisory mode through `targets.<name>.advisory`
//! or through the active profile's `focus_platforms` /
//! `advisory_platforms`. A `Lane-Policy:` commit trailer can override that
//! decision for one PR without changing tracked config.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use toml::{Table, Value};

use crate::config::LoadedConfig;

/// Resolved advisory/required lane policy for a single ship.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LanePolicy {
    /// Target names whose failures should not block merge.
    pub advisory_targets: BTreeSet<String>,
    /// Advisory targets whose decision came from a `Lane-Policy:` trailer.
    pub overrides_from_trailer: BTreeSet<String>,
}

impl LanePolicy {
    /// Return whether `target` is advisory.
    #[must_use]
    pub fn is_advisory(&self, target: &str) -> bool {
        self.advisory_targets.contains(target)
    }

    /// Return whether `target` is required.
    #[must_use]
    pub fn is_required(&self, target: &str) -> bool {
        !self.is_advisory(target)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LaneRequirement {
    Required,
    Advisory,
}

/// Resolve lane policy from config plus the current HEAD message.
#[must_use]
pub fn resolve_lane_policy(config: &LoadedConfig, cwd: &Path) -> LanePolicy {
    let commit_message = read_tip_commit_message(cwd);
    resolve_lane_policy_from_table(&config.data, Some(&commit_message))
}

/// Resolve lane policy from a TOML table and optional commit message.
#[must_use]
pub fn resolve_lane_policy_from_table(data: &Table, commit_message: Option<&str>) -> LanePolicy {
    let known_targets = target_names(data);
    let mut advisory = advisory_targets_from_table(data);
    advisory.extend(advisory_targets_from_active_profile(data));
    let mut overrides = BTreeSet::new();
    let trailer = parse_lane_policy_trailers(commit_message.unwrap_or_default());

    for (target, requirement) in trailer {
        if !known_targets.contains(&target) {
            continue;
        }
        match requirement {
            LaneRequirement::Required if advisory.remove(&target) => {
                overrides.insert(target);
            }
            LaneRequirement::Advisory if advisory.insert(target.clone()) => {
                overrides.insert(target);
            }
            _ => {}
        }
    }

    LanePolicy {
        advisory_targets: advisory,
        overrides_from_trailer: overrides,
    }
}

fn target_names(data: &Table) -> BTreeSet<String> {
    data.get("targets")
        .and_then(Value::as_table)
        .map(|targets| targets.keys().cloned().collect())
        .unwrap_or_default()
}

fn advisory_targets_from_table(data: &Table) -> BTreeSet<String> {
    data.get("targets")
        .and_then(Value::as_table)
        .map(|targets| {
            targets
                .iter()
                .filter_map(|(name, value)| {
                    let advisory = value
                        .as_table()
                        .and_then(|table| table.get("advisory"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    advisory.then(|| name.clone())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn advisory_targets_from_active_profile(data: &Table) -> BTreeSet<String> {
    let Some(profile) = active_profile_table(data) else {
        return BTreeSet::new();
    };
    let focus_platforms = platform_set(profile.get("focus_platforms"));
    let advisory_platforms = platform_set(profile.get("advisory_platforms"));
    if focus_platforms.is_empty() && advisory_platforms.is_empty() {
        return BTreeSet::new();
    }

    target_platforms(data)
        .into_iter()
        .filter_map(|(name, platform)| {
            let family = normalize_platform_family(&platform)?;
            if advisory_platforms.contains(&family)
                || (!focus_platforms.is_empty() && !focus_platforms.contains(&family))
            {
                Some(name)
            } else {
                None
            }
        })
        .collect()
}

fn active_profile_table(data: &Table) -> Option<&Table> {
    let profile_name = data
        .get("project")
        .and_then(Value::as_table)
        .and_then(|project| project.get("profile"))
        .and_then(Value::as_str)?;
    data.get("profiles")
        .and_then(Value::as_table)
        .and_then(|profiles| profiles.get(profile_name))
        .and_then(Value::as_table)
}

fn target_platforms(data: &Table) -> BTreeMap<String, String> {
    data.get("targets")
        .and_then(Value::as_table)
        .map(|targets| {
            targets
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .as_table()
                        .and_then(|target| target.get("platform"))
                        .and_then(Value::as_str)
                        .map(|platform| (name.clone(), platform.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn platform_set(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter_map(normalize_platform_family)
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_platform_family(value: &str) -> Option<String> {
    let lowered = value.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return None;
    }
    if lowered == "mac" || lowered.starts_with("macos") || lowered.starts_with("darwin") {
        return Some("macos".to_owned());
    }
    if lowered == "win" || lowered.starts_with("windows") {
        return Some("windows".to_owned());
    }
    if lowered.starts_with("linux") || lowered == "ubuntu" {
        return Some("linux".to_owned());
    }
    lowered
        .split_once('-')
        .map(|(family, _)| family.to_owned())
        .or(Some(lowered))
}

fn parse_lane_policy_trailers(message: &str) -> BTreeMap<String, LaneRequirement> {
    let mut parsed = BTreeMap::new();
    for line in message.lines() {
        let Some((key, payload)) = line.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("Lane-Policy") {
            continue;
        }
        for token in payload.replace(',', " ").split_whitespace() {
            let Some((target, policy)) = token.split_once('=') else {
                continue;
            };
            let target = target.trim();
            if target.is_empty() {
                continue;
            }
            let requirement = match policy.trim().to_ascii_lowercase().as_str() {
                "required" => LaneRequirement::Required,
                "advisory" => LaneRequirement::Advisory,
                _ => continue,
            };
            parsed.insert(target.to_owned(), requirement);
        }
    }
    parsed
}

fn read_tip_commit_message(cwd: &Path) -> String {
    let output = Command::new("git")
        .args(["log", "-1", "--format=%B", "HEAD"])
        .current_dir(cwd)
        .output();
    let Ok(output) = output else {
        return String::new();
    };
    if !output.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use toml::Table;

    use super::{LaneRequirement, parse_lane_policy_trailers, resolve_lane_policy_from_table};

    fn table(contents: &str) -> Table {
        contents.parse::<Table>().expect("toml")
    }

    #[test]
    fn parses_multiple_lane_policy_trailers_last_wins() {
        let parsed = parse_lane_policy_trailers(
            "body\nLane-Policy: windows=advisory mac=required\nLane-Policy: windows=required, linux=advisory\n",
        );

        assert_eq!(parsed.get("windows"), Some(&LaneRequirement::Required));
        assert_eq!(parsed.get("mac"), Some(&LaneRequirement::Required));
        assert_eq!(parsed.get("linux"), Some(&LaneRequirement::Advisory));
    }

    #[test]
    fn config_advisory_targets_are_non_blocking_by_default() {
        let config = table(
            r#"
            [targets.linux]
            platform = "linux-x64"

            [targets.windows]
            platform = "windows-x64"
            advisory = true
            "#,
        );

        let policy = resolve_lane_policy_from_table(&config, None);

        assert!(policy.is_required("linux"));
        assert!(policy.is_advisory("windows"));
        assert!(policy.overrides_from_trailer.is_empty());
    }

    #[test]
    fn active_profile_focus_platforms_make_non_focus_targets_advisory() {
        let config = table(
            r#"
            [project]
            profile = "macos-only"

            [profiles.macos-only]
            description = "macOS is required; Linux and Windows are advisory."
            focus_platforms = ["macos"]
            advisory_platforms = ["linux", "windows"]

            [targets.mac]
            platform = "macos-arm64"

            [targets.ubuntu]
            platform = "linux-x64"

            [targets.windows]
            platform = "windows-x64"
            "#,
        );

        let policy = resolve_lane_policy_from_table(&config, None);

        assert!(policy.is_required("mac"));
        assert!(policy.is_advisory("ubuntu"));
        assert!(policy.is_advisory("windows"));
    }

    #[test]
    fn trailer_can_make_profile_advisory_target_required() {
        let config = table(
            r#"
            [project]
            profile = "macos-only"

            [profiles.macos-only]
            focus_platforms = ["macos"]

            [targets.mac]
            platform = "macos-arm64"

            [targets.windows]
            platform = "windows-x64"
            "#,
        );

        let policy = resolve_lane_policy_from_table(&config, Some("Lane-Policy: windows=required"));

        assert!(policy.is_required("mac"));
        assert!(policy.is_required("windows"));
        assert_eq!(
            policy.overrides_from_trailer,
            ["windows".to_owned()].into_iter().collect()
        );
    }

    #[test]
    fn trailer_overrides_config_and_ignores_unknown_targets() {
        let config = table(
            r#"
            [targets.linux]
            platform = "linux-x64"
            advisory = true

            [targets.windows]
            platform = "windows-x64"
            "#,
        );

        let policy = resolve_lane_policy_from_table(
            &config,
            Some("Lane-Policy: linux=required windows=advisory typo=advisory"),
        );

        assert!(policy.is_required("linux"));
        assert!(policy.is_advisory("windows"));
        assert_eq!(
            policy.overrides_from_trailer,
            ["linux".to_owned(), "windows".to_owned()]
                .into_iter()
                .collect()
        );
    }
}
