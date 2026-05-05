//! Pull request shell boundary used by `ship`.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// GitHub pull request metadata needed by ship orchestration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrInfo {
    /// Pull request number.
    pub number: u64,
    /// Pull request URL.
    pub url: String,
    /// Pull request title.
    pub title: String,
    /// Pull request state.
    pub state: String,
    /// Head branch name.
    pub branch: String,
    /// Base branch name.
    pub base: String,
}

/// Error returned by PR shell helpers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrError {
    message: String,
}

impl PrError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for PrError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PrError {}

/// Push the current branch before PR lookup/create.
pub fn push_branch(cwd: &Path, branch: &str) -> Result<(), PrError> {
    let output = Command::new("git")
        .args(["push", "-u", "origin", branch])
        .current_dir(cwd)
        .output()
        .map_err(|error| PrError::new(format!("git push failed to start: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(PrError::new(format!(
        "git push failed: {}",
        stderr_or_stdout(&output)
    )))
}

/// Find the first open PR for `branch`.
pub fn find_pr_for_branch(
    cwd: &Path,
    gh_command: Option<&Path>,
    branch: &str,
) -> Result<Option<PrInfo>, PrError> {
    let output = gh(gh_command)
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "open",
            "--limit",
            "1",
            "--json",
            PR_JSON_FIELDS,
        ])
        .current_dir(cwd)
        .output()
        .map_err(|error| PrError::new(format!("gh pr list failed to start: {error}")))?;
    if !output.status.success() {
        return Err(PrError::new(format!(
            "gh pr list failed: {}",
            stderr_or_stdout(&output)
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_pr_list(&text)
}

/// Create a PR and normalize its metadata through `gh pr view`.
pub fn create_pr(
    cwd: &Path,
    gh_command: Option<&Path>,
    branch: &str,
    base: &str,
    title: &str,
    body: &str,
) -> Result<PrInfo, PrError> {
    let output = gh(gh_command)
        .args([
            "pr", "create", "--head", branch, "--base", base, "--title", title, "--body", body,
        ])
        .current_dir(cwd)
        .output()
        .map_err(|error| PrError::new(format!("gh pr create failed to start: {error}")))?;
    if !output.status.success() {
        return Err(PrError::new(format!(
            "gh pr create failed: {}",
            stderr_or_stdout(&output)
        )));
    }
    let selector = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if selector.is_empty() {
        return Err(PrError::new("gh pr create did not print a PR URL"));
    }
    get_pr_status(cwd, gh_command, &selector)
}

/// Return normalized PR metadata for a PR selector.
pub fn get_pr_status(
    cwd: &Path,
    gh_command: Option<&Path>,
    selector: &str,
) -> Result<PrInfo, PrError> {
    let output = gh(gh_command)
        .args(["pr", "view", selector, "--json", PR_JSON_FIELDS])
        .current_dir(cwd)
        .output()
        .map_err(|error| PrError::new(format!("gh pr view failed to start: {error}")))?;
    if !output.status.success() {
        return Err(PrError::new(format!(
            "gh pr view failed: {}",
            stderr_or_stdout(&output)
        )));
    }
    parse_pr_info(&String::from_utf8_lossy(&output.stdout))
}

const PR_JSON_FIELDS: &str = "number,url,title,state,headRefName,baseRefName";

fn gh(gh_command: Option<&Path>) -> Command {
    gh_command.map_or_else(|| Command::new("gh"), Command::new)
}

fn stderr_or_stdout(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn parse_pr_list(text: &str) -> Result<Option<PrInfo>, PrError> {
    let value = serde_json::from_str::<Value>(text)
        .map_err(|error| PrError::new(format!("failed to parse gh pr list JSON: {error}")))?;
    let Some(items) = value.as_array() else {
        return Err(PrError::new("gh pr list JSON was not an array"));
    };
    items.first().map(parse_pr_value).transpose()
}

fn parse_pr_info(text: &str) -> Result<PrInfo, PrError> {
    let value = serde_json::from_str::<Value>(text)
        .map_err(|error| PrError::new(format!("failed to parse gh pr view JSON: {error}")))?;
    parse_pr_value(&value)
}

fn parse_pr_value(value: &Value) -> Result<PrInfo, PrError> {
    Ok(PrInfo {
        number: value
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| PrError::new("PR JSON missing number"))?,
        url: string_field(value, "url")?,
        title: string_field(value, "title")?,
        state: string_field(value, "state")?,
        branch: string_field(value, "headRefName")?,
        base: string_field(value, "baseRefName")?,
    })
}

fn string_field(value: &Value, field: &str) -> Result<String, PrError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| PrError::new(format!("PR JSON missing {field}")))
}

#[cfg(test)]
mod tests {
    use super::{PrInfo, parse_pr_info, parse_pr_list};

    #[test]
    fn parses_pr_view_payload() {
        let info = parse_pr_info(
            r#"{
                "number": 42,
                "url": "https://github.com/o/r/pull/42",
                "title": "Fix thing",
                "state": "OPEN",
                "headRefName": "feature/test",
                "baseRefName": "main"
            }"#,
        )
        .expect("pr info");

        assert_eq!(
            info,
            PrInfo {
                number: 42,
                url: "https://github.com/o/r/pull/42".to_owned(),
                title: "Fix thing".to_owned(),
                state: "OPEN".to_owned(),
                branch: "feature/test".to_owned(),
                base: "main".to_owned(),
            }
        );
    }

    #[test]
    fn parses_empty_and_non_empty_pr_lists() {
        assert_eq!(parse_pr_list("[]").expect("empty"), None);
        assert_eq!(
            parse_pr_list(
                r#"[{
                    "number": 7,
                    "url": "https://github.com/o/r/pull/7",
                    "title": "Ship it",
                    "state": "OPEN",
                    "headRefName": "feature/a",
                    "baseRefName": "develop"
                }]"#,
            )
            .expect("list")
            .expect("first")
            .number,
            7
        );
    }
}
