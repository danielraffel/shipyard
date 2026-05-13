//! `shipyard update` — self-update from the CLI.
//!
//! Phase 1 (this module):
//! - `--check` / `--json` query GitHub's REST `releases/latest` (no GraphQL,
//!   matches the policy from #289) and report installed-vs-available.
//! - `--to vX.Y.Z` pins to a specific tag.
//! - Apply path delegates to `install.sh` so we don't reimplement
//!   platform-specific dmg-mount / atomic-rename / Windows .cmd shimming.
//!   That's the canonical bootstrap path; Phase 2 will move it native.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use serde_json::Value;

use super::{CliFailure, cli::UpdateArgs};
use crate::output::write_json_envelope;

const UPDATE_EVENT: &str = "update";
const DEFAULT_RELEASES_API_BASE: &str =
    "https://api.github.com/repos/danielraffel/Shipyard/releases";
const DEFAULT_INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/danielraffel/Shipyard/main/install.sh";

/// CLI dispatch entry.
pub(super) fn update_command<W: Write>(
    args: &UpdateArgs,
    json: bool,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    let installed = installed_version();
    let target = resolve_target_tag(args).map_err(|message| CliFailure::new(1, message))?;
    let update_available = target_is_newer(&installed, &target);

    if args.check {
        return render_check(
            &installed,
            &target,
            update_available,
            args.dry_run,
            json,
            stdout,
        );
    }
    if args.dry_run {
        return render_plan(&installed, &target, update_available, json, stdout);
    }

    apply_update(args, &installed, &target, update_available, json, stdout)
}

fn installed_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

fn resolve_target_tag(args: &UpdateArgs) -> Result<String, String> {
    let api_base = args
        .releases_api_base
        .as_deref()
        .unwrap_or(DEFAULT_RELEASES_API_BASE);
    let curl_bin = args
        .curl_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("curl"));
    if let Some(raw) = args.to.as_deref().filter(|value| !value.trim().is_empty()) {
        return Ok(normalize_tag(raw));
    }
    fetch_latest_tag(api_base, &curl_bin)
}

fn normalize_tag(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with('v') {
        trimmed.to_owned()
    } else {
        format!("v{trimmed}")
    }
}

fn fetch_latest_tag(api_base: &str, curl_bin: &Path) -> Result<String, String> {
    let url = format!("{api_base}/latest");
    let output = Command::new(curl_bin)
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: shipyard-update",
            &url,
        ])
        .output()
        .map_err(|error| format!("failed to invoke curl: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!(
            "GitHub releases/latest request failed: {}",
            if stderr.is_empty() {
                "curl exited non-zero".to_owned()
            } else {
                stderr
            }
        ));
    }
    parse_tag_name(&String::from_utf8_lossy(&output.stdout))
}

fn parse_tag_name(body: &str) -> Result<String, String> {
    let value = serde_json::from_str::<Value>(body)
        .map_err(|error| format!("failed to parse releases JSON: {error}"))?;
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| "releases JSON missing `tag_name`".to_owned())?;
    Ok(tag.to_owned())
}

fn target_is_newer(installed: &str, target_tag: &str) -> bool {
    let target = target_tag.strip_prefix('v').unwrap_or(target_tag);
    compare_semver(installed, target).is_lt()
}

fn compare_semver(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |raw: &str| -> [u64; 3] {
        let mut parts = [0u64; 3];
        for (idx, segment) in raw.split('.').take(3).enumerate() {
            let cleaned: String = segment.chars().take_while(char::is_ascii_digit).collect();
            parts[idx] = cleaned.parse::<u64>().unwrap_or(0);
        }
        parts
    };
    parse(a).cmp(&parse(b))
}

fn render_check<W: Write>(
    installed: &str,
    target_tag: &str,
    update_available: bool,
    dry_run: bool,
    json: bool,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    let mut data = BTreeMap::new();
    data.insert("event".to_owned(), Value::from("check"));
    data.insert("installed".to_owned(), Value::from(installed.to_owned()));
    data.insert("target".to_owned(), Value::from(target_tag.to_owned()));
    data.insert("update_available".to_owned(), Value::Bool(update_available));
    data.insert("dry_run".to_owned(), Value::Bool(dry_run));
    render(stdout, json, data, || {
        if update_available {
            format!(
                "installed={installed} available={target_tag} → update available (run `shipyard update` to apply)."
            )
        } else {
            format!("installed={installed} target={target_tag} → already up to date.")
        }
    })?;
    Ok(ExitCode::SUCCESS)
}

fn render_plan<W: Write>(
    installed: &str,
    target_tag: &str,
    update_available: bool,
    json: bool,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    let mut data = BTreeMap::new();
    data.insert("event".to_owned(), Value::from("plan"));
    data.insert("installed".to_owned(), Value::from(installed.to_owned()));
    data.insert("target".to_owned(), Value::from(target_tag.to_owned()));
    data.insert("update_available".to_owned(), Value::Bool(update_available));
    data.insert("dry_run".to_owned(), Value::Bool(true));
    render(stdout, json, data, || {
        if update_available {
            format!(
                "Dry-run: would install {target_tag} (current: {installed}) via install.sh. Re-run without --dry-run to apply."
            )
        } else {
            format!("Dry-run: installed={installed} matches target={target_tag}; nothing to do.")
        }
    })?;
    Ok(ExitCode::SUCCESS)
}

fn apply_update<W: Write>(
    args: &UpdateArgs,
    installed: &str,
    target_tag: &str,
    update_available: bool,
    json: bool,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    if !update_available && args.to.is_none() {
        // No-op fast path; --to forces install even if equal/older.
        let mut data = BTreeMap::new();
        data.insert("event".to_owned(), Value::from("noop"));
        data.insert("installed".to_owned(), Value::from(installed.to_owned()));
        data.insert("target".to_owned(), Value::from(target_tag.to_owned()));
        render(stdout, json, data, || {
            format!("installed={installed} already matches target={target_tag}; no update applied.")
        })?;
        return Ok(ExitCode::SUCCESS);
    }

    let install_script_url = args
        .install_script_url
        .as_deref()
        .unwrap_or(DEFAULT_INSTALL_SCRIPT_URL);
    let curl_bin = args
        .curl_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("curl"));
    let shell_bin = args
        .shell_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("sh"));

    let mut data = BTreeMap::new();
    data.insert("event".to_owned(), Value::from("apply"));
    data.insert("installed".to_owned(), Value::from(installed.to_owned()));
    data.insert("target".to_owned(), Value::from(target_tag.to_owned()));
    data.insert(
        "install_script".to_owned(),
        Value::from(install_script_url.to_owned()),
    );
    render(stdout, json, data, || {
        format!("Updating from {installed} to {target_tag} via {install_script_url} …")
    })?;

    invoke_install_script(&curl_bin, &shell_bin, install_script_url, target_tag, json)?;

    let mut data = BTreeMap::new();
    data.insert("event".to_owned(), Value::from("applied"));
    data.insert("installed".to_owned(), Value::from(installed.to_owned()));
    data.insert("target".to_owned(), Value::from(target_tag.to_owned()));
    render(stdout, json, data, || {
        format!(
            "Update to {target_tag} applied. Run `shipyard --version` to confirm the new binary is on PATH."
        )
    })?;
    Ok(ExitCode::SUCCESS)
}

fn invoke_install_script(
    curl_bin: &Path,
    shell_bin: &Path,
    install_script_url: &str,
    target_tag: &str,
    json: bool,
) -> Result<(), CliFailure> {
    let mut curl = Command::new(curl_bin)
        .args(["-fsSL", install_script_url])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| CliFailure::new(1, format!("failed to spawn curl: {error}")))?;

    let curl_stdout = curl
        .stdout
        .take()
        .ok_or_else(|| CliFailure::new(1, "curl stdout pipe missing"))?;

    // Under `--json`, route installer progress to stderr so the stdout
    // stream stays a clean sequence of JSON envelopes for downstream
    // automation. In human mode, keep the installer's stdout visible so
    // the user sees the progress bar.
    let install_stdout = if json {
        // Inherit our own stderr — install.sh's stdout becomes our stderr.
        // `Stdio::from(std::io::stderr())` would consume our stderr handle;
        // duplicate the parent stderr fd instead.
        Stdio::from(
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/stderr")
                .map_err(|error| {
                    CliFailure::new(1, format!("failed to open /dev/stderr: {error}"))
                })?,
        )
    } else {
        Stdio::inherit()
    };

    let env_tag = target_tag.strip_prefix('v').unwrap_or(target_tag);
    let mut sh = Command::new(shell_bin)
        .env("SHIPYARD_VERSION", env_tag)
        .stdin(curl_stdout)
        .stdout(install_stdout)
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| CliFailure::new(1, format!("failed to spawn shell: {error}")))?;

    let curl_status = curl
        .wait()
        .map_err(|error| CliFailure::new(1, format!("curl wait failed: {error}")))?;
    let sh_status = sh
        .wait()
        .map_err(|error| CliFailure::new(1, format!("install.sh wait failed: {error}")))?;
    if !curl_status.success() {
        return Err(CliFailure::new(
            1,
            format!(
                "curl exited {} while fetching {install_script_url}",
                curl_status.code().unwrap_or(-1)
            ),
        ));
    }
    if !sh_status.success() {
        return Err(CliFailure::new(
            1,
            format!(
                "install.sh exited {}; binary may not have been replaced",
                sh_status.code().unwrap_or(-1)
            ),
        ));
    }
    Ok(())
}

fn render<W: Write>(
    stdout: &mut W,
    json: bool,
    data: BTreeMap<String, Value>,
    human: impl FnOnce() -> String,
) -> Result<(), CliFailure> {
    if json {
        write_json_envelope(stdout, UPDATE_EVENT, data)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(());
    }
    writeln!(stdout, "{}", human()).map_err(|error| CliFailure::new(1, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_tag_prepends_v() {
        assert_eq!(normalize_tag("0.53.0"), "v0.53.0");
        assert_eq!(normalize_tag("v0.53.0"), "v0.53.0");
        assert_eq!(normalize_tag("  v1.2.3 "), "v1.2.3");
    }

    #[test]
    fn parse_tag_name_extracts_tag_name() {
        let body = r#"{"tag_name":"v0.54.0","draft":false}"#;
        assert_eq!(parse_tag_name(body).unwrap(), "v0.54.0");
    }

    #[test]
    fn parse_tag_name_errors_when_missing() {
        let body = r#"{"draft":false}"#;
        let err = parse_tag_name(body).expect_err("missing tag_name");
        assert!(err.contains("tag_name"));
    }

    #[test]
    fn target_is_newer_compares_semver_correctly() {
        assert!(target_is_newer("0.53.0", "v0.54.0"));
        assert!(!target_is_newer("0.54.0", "v0.54.0"));
        assert!(!target_is_newer("0.54.0", "v0.53.0"));
        assert!(target_is_newer("0.53.0", "v0.53.1"));
        assert!(target_is_newer("0.53.0", "v1.0.0"));
    }

    #[test]
    fn compare_semver_handles_prerelease_suffix() {
        // Pre-release suffixes are ignored conservatively.
        assert_eq!(
            compare_semver("0.54.0-rc.1", "0.54.0"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn render_check_human_mentions_action() {
        let mut buf = Vec::new();
        render_check("0.53.0", "v0.54.0", true, false, false, &mut buf).expect("render");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("update available"));
        assert!(text.contains("v0.54.0"));
        assert!(text.contains("0.53.0"));
    }

    #[test]
    fn render_check_human_handles_already_up_to_date() {
        let mut buf = Vec::new();
        render_check("0.54.0", "v0.54.0", false, false, false, &mut buf).expect("render");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("already up to date"));
    }

    #[test]
    fn render_plan_human_describes_dry_run() {
        let mut buf = Vec::new();
        render_plan("0.53.0", "v0.54.0", true, false, &mut buf).expect("render");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("Dry-run"));
        assert!(text.contains("install.sh"));
    }

    #[test]
    fn render_check_json_carries_full_envelope() {
        let mut buf = Vec::new();
        render_check("0.53.0", "v0.54.0", true, false, true, &mut buf).expect("render");
        let json: Value = serde_json::from_slice(&buf).expect("json");
        assert_eq!(json["installed"], Value::from("0.53.0"));
        assert_eq!(json["target"], Value::from("v0.54.0"));
        assert_eq!(json["update_available"], Value::Bool(true));
        assert_eq!(json["event"], Value::from("check"));
        assert_eq!(json["command"], Value::from(UPDATE_EVENT));
    }
}
