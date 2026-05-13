use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::Value;

use super::{
    CliFailure,
    cli::{MergeMethod, MergeResult},
};
use crate::output::write_json_envelope;
use crate::ship_state::{ShipState, ShipStateStore};
use crate::watch::ship_terminal_verdict;

pub(super) struct AutoMergeRequest {
    pub(super) pr: u64,
    pub(super) merge_method: MergeMethod,
    pub(super) delete_branch: bool,
    pub(super) admin: bool,
    pub(super) pr_snapshot_file: Option<PathBuf>,
    pub(super) merge_command: Option<PathBuf>,
    pub(super) merge_result: Option<MergeResult>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum AutoMergeOutcome {
    AlreadyMerged,
    PrNotFound,
    InFlight {
        evidence: BTreeMap<String, String>,
    },
    TargetFailed {
        failing_targets: Vec<String>,
        evidence: BTreeMap<String, String>,
    },
    MergeFailed {
        error: String,
    },
    Merged {
        cleanup_warning: Option<String>,
    },
}

#[derive(Debug)]
pub(super) enum AutoMergeOperationError {
    Store(std::io::Error),
}

impl std::fmt::Display for AutoMergeOperationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for AutoMergeOperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
        }
    }
}

pub(super) fn execute_auto_merge(
    store: &ShipStateStore,
    cwd: &Path,
    request: &AutoMergeRequest,
) -> Result<AutoMergeOutcome, AutoMergeOperationError> {
    let Some(state) = store.get(request.pr) else {
        return Ok(
            if pr_is_merged(request.pr, cwd, request.pr_snapshot_file.as_deref()) {
                AutoMergeOutcome::AlreadyMerged
            } else {
                AutoMergeOutcome::PrNotFound
            },
        );
    };

    match ship_terminal_verdict(&state) {
        None => Ok(AutoMergeOutcome::InFlight {
            evidence: state.evidence_snapshot,
        }),
        Some(false) => Ok(AutoMergeOutcome::TargetFailed {
            failing_targets: failing_required_targets(&state),
            evidence: state.evidence_snapshot,
        }),
        Some(true) => {
            if let Err(error) = merge_pr(
                request.pr,
                cwd,
                request.merge_method,
                request.delete_branch,
                request.admin,
                request.merge_command.as_deref(),
                request.merge_result,
            ) {
                if merge_error_confirms_merged(&error)
                    || pr_is_merged(request.pr, cwd, request.pr_snapshot_file.as_deref())
                {
                    store
                        .archive(request.pr)
                        .map_err(AutoMergeOperationError::Store)?;
                    return Ok(AutoMergeOutcome::Merged {
                        cleanup_warning: Some(error),
                    });
                }
                return Ok(AutoMergeOutcome::MergeFailed { error });
            }
            store
                .archive(request.pr)
                .map_err(AutoMergeOperationError::Store)?;
            Ok(AutoMergeOutcome::Merged {
                cleanup_warning: None,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn auto_merge<W: Write>(
    store: &ShipStateStore,
    cwd: &Path,
    pr: u64,
    merge_method: MergeMethod,
    delete_branch: bool,
    admin: bool,
    pr_snapshot_file: Option<PathBuf>,
    merge_command: Option<PathBuf>,
    merge_result: Option<MergeResult>,
    json: bool,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    let request = AutoMergeRequest {
        pr,
        merge_method,
        delete_branch,
        admin,
        pr_snapshot_file,
        merge_command,
        merge_result,
    };
    match execute_auto_merge(store, cwd, &request)
        .map_err(|error| CliFailure::new(1, error.to_string()))?
    {
        AutoMergeOutcome::AlreadyMerged => {
            render_event(
                stdout,
                json,
                "already-merged",
                fields([("pr", Value::from(pr))]),
            )?;
            Ok(ExitCode::SUCCESS)
        }
        AutoMergeOutcome::PrNotFound => {
            render_event(
                stdout,
                json,
                "pr-not-found",
                fields([("pr", Value::from(pr))]),
            )?;
            Ok(ExitCode::from(2))
        }
        AutoMergeOutcome::InFlight { evidence } => {
            render_event(
                stdout,
                json,
                "in-flight",
                fields([
                    ("pr", Value::from(pr)),
                    (
                        "evidence",
                        serde_json::to_value(&evidence)
                            .map_err(|error| CliFailure::new(1, error.to_string()))?,
                    ),
                ]),
            )?;
            Ok(ExitCode::from(3))
        }
        AutoMergeOutcome::TargetFailed {
            failing_targets,
            evidence,
        } => {
            render_event(
                stdout,
                json,
                "target-failed",
                fields([
                    ("pr", Value::from(pr)),
                    (
                        "failing_targets",
                        serde_json::to_value(&failing_targets)
                            .map_err(|error| CliFailure::new(1, error.to_string()))?,
                    ),
                    (
                        "evidence",
                        serde_json::to_value(&evidence)
                            .map_err(|error| CliFailure::new(1, error.to_string()))?,
                    ),
                ]),
            )?;
            Ok(ExitCode::from(1))
        }
        AutoMergeOutcome::MergeFailed { error } => {
            render_event(
                stdout,
                json,
                "merge-failed",
                fields([("pr", Value::from(pr)), ("error", Value::from(error))]),
            )?;
            Ok(ExitCode::from(1))
        }
        AutoMergeOutcome::Merged { cleanup_warning } => {
            let mut data = fields([("pr", Value::from(pr))]);
            if let Some(warning) = cleanup_warning {
                data.insert("cleanup_warning".to_owned(), Value::from(warning));
            }
            render_event(stdout, json, "merged", data)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn pr_is_merged(pr: u64, cwd: &Path, snapshot_file: Option<&Path>) -> bool {
    let payload = if let Some(path) = snapshot_file {
        std::fs::read_to_string(path).ok()
    } else {
        let output = Command::new("gh")
            .args(["pr", "view", &pr.to_string(), "--json", "state"])
            .current_dir(cwd)
            .output()
            .ok();
        let Some(output) = output else {
            return false;
        };
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    payload
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| {
            value
                .get("state")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|state| state.eq_ignore_ascii_case("merged"))
}

fn merge_pr(
    pr: u64,
    cwd: &Path,
    merge_method: MergeMethod,
    delete_branch: bool,
    admin: bool,
    merge_command: Option<&Path>,
    merge_result: Option<MergeResult>,
) -> Result<(), String> {
    match merge_result {
        Some(MergeResult::Success) => return Ok(()),
        Some(MergeResult::Failure) => return Err("simulated merge failure".to_owned()),
        None => {}
    }

    let custom_command = merge_command.is_some();
    let mut command =
        Command::new(merge_command.map_or_else(|| PathBuf::from("gh"), Path::to_path_buf));
    if !custom_command {
        command.args(["pr", "merge", &pr.to_string()]);
    }
    command.arg(merge_method.gh_flag());
    if delete_branch {
        command.arg("--delete-branch");
    }
    if admin {
        command.arg("--admin");
    }
    let output = command
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("failed to run merge command: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let message = if stderr.is_empty() { stdout } else { stderr };

    // GraphQL exhausted but REST still has budget? `gh pr merge` uses GraphQL
    // for the merge state probe; the actual merge atom is REST
    // (PUT /repos/:r/pulls/:n/merge), so fall back to a direct REST call
    // rather than failing the ship. Matches src/pr.rs's pattern for
    // gh pr list / create / view.
    if !custom_command && crate::pr::is_graphql_rate_limited(&message) {
        return merge_pr_rest(pr, cwd, merge_method, delete_branch);
    }
    Err(message)
}

/// REST fallback for `gh pr merge` when GraphQL is rate-limited.
///
/// `gh pr merge` queries the PR's mergeable state via GraphQL before issuing
/// the actual merge POST. When GraphQL is at 0/5000 the call fails, but
/// REST is independent (`PUT /repos/:repo/pulls/:n/merge`) and usually has
/// budget left. This function bypasses the GraphQL probe and calls REST
/// directly through `gh api`, then optionally deletes the head branch the
/// same way `gh pr merge --delete-branch` would.
fn merge_pr_rest(
    pr: u64,
    cwd: &Path,
    merge_method: MergeMethod,
    delete_branch: bool,
) -> Result<(), String> {
    let repo = repo_slug_for_rest(cwd)?;
    let endpoint = format!("repos/{repo}/pulls/{pr}/merge");
    let output = Command::new("gh")
        .args([
            "api",
            "-X",
            "PUT",
            &endpoint,
            "-f",
            &format!("merge_method={}", merge_method.rest_value()),
        ])
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("REST fallback: failed to invoke gh api: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return Err(format!(
            "REST fallback: gh api PUT {endpoint} failed: {}",
            if stderr.is_empty() { stdout } else { stderr }
        ));
    }
    if delete_branch && let Ok(branch) = pr_head_branch_rest(&repo, pr, cwd) {
        // Best-effort delete; mirrors `gh pr merge --delete-branch` which
        // also tolerates a missing branch silently.
        let _ = Command::new("gh")
            .args([
                "api",
                "-X",
                "DELETE",
                &format!("repos/{repo}/git/refs/heads/{branch}"),
            ])
            .current_dir(cwd)
            .status();
    }
    Ok(())
}

fn repo_slug_for_rest(cwd: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("REST fallback: git remote probe failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "REST fallback: git remote probe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let remote = String::from_utf8_lossy(&output.stdout);
    parse_github_remote_slug(remote.trim()).ok_or_else(|| {
        format!(
            "REST fallback: remote.origin.url is not a supported GitHub remote: {}",
            remote.trim()
        )
    })
}

fn parse_github_remote_slug(remote: &str) -> Option<String> {
    let mut slug = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("https://github.com/"))
        .or_else(|| remote.strip_prefix("http://github.com/"))?
        .trim_end_matches('/')
        .to_owned();
    if let Some(stripped) = slug.strip_suffix(".git") {
        slug = stripped.to_owned();
    }
    if slug.split('/').count() != 2 {
        return None;
    }
    Some(slug)
}

fn pr_head_branch_rest(repo: &str, pr: u64, cwd: &Path) -> Result<String, String> {
    let output = Command::new("gh")
        .args(["api", &format!("repos/{repo}/pulls/{pr}")])
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("REST fallback: gh api PR fetch failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "REST fallback: gh api repos/{repo}/pulls/{pr} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("REST fallback: failed to parse PR JSON: {error}"))?;
    value
        .get("head")
        .and_then(|head| head.get("ref"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| "REST fallback: PR JSON missing head.ref".to_owned())
}

fn merge_error_confirms_merged(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("pull request") && lower.contains("already merged")
}

fn failing_required_targets(state: &ShipState) -> Vec<String> {
    let advisory_targets = state
        .dispatched_runs
        .iter()
        .filter(|run| !run.required)
        .map(|run| run.target.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    state
        .evidence_snapshot
        .iter()
        .filter(|(target, status)| *status != "pass" && !advisory_targets.contains(target.as_str()))
        .map(|(target, _)| target.clone())
        .collect()
}

fn render_event<W: Write>(
    stdout: &mut W,
    json: bool,
    event: &str,
    mut data: BTreeMap<String, Value>,
) -> Result<(), CliFailure> {
    if json {
        data.insert("event".to_owned(), Value::from(event.to_owned()));
        write_json_envelope(stdout, "auto-merge", data)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(());
    }

    let pr = data.get("pr").and_then(Value::as_u64).unwrap_or_default();
    match event {
        "already-merged" => writeln!(stdout, "PR #{pr}: already merged - idempotent no-op."),
        "pr-not-found" => writeln!(
            stdout,
            "PR #{pr}: no ship state found (typo / never shipped)."
        ),
        "in-flight" => writeln!(
            stdout,
            "PR #{pr}: ship still in flight - evidence {}.",
            data.get("evidence").unwrap_or(&Value::Null)
        ),
        "target-failed" => {
            let targets = data
                .get("failing_targets")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            writeln!(
                stdout,
                "PR #{pr}: refusing to merge - targets failed: {targets}"
            )
        }
        "merge-failed" => writeln!(
            stdout,
            "PR #{pr}: merge attempt failed - {}",
            data.get("error").and_then(Value::as_str).unwrap_or("")
        ),
        "merged" => {
            if let Some(warning) = data.get("cleanup_warning").and_then(Value::as_str) {
                writeln!(stdout, "PR #{pr}: merged. Cleanup warning: {warning}")
            } else {
                writeln!(stdout, "PR #{pr}: merged.")
            }
        }
        _ => Ok(()),
    }
    .map_err(|error| CliFailure::new(1, error.to_string()))
}

fn fields(items: impl IntoIterator<Item = (&'static str, Value)>) -> BTreeMap<String, Value> {
    items
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}
