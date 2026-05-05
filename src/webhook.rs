//! GitHub webhook signature validation and event decoding.
//!
//! This is the pure domain layer for the daemon HTTP endpoint. The runtime
//! owns sockets; this module owns the Python-compatible wire event shape.

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Normalized GitHub `workflow_run` payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkflowRunPayload {
    /// GitHub webhook action.
    pub action: String,
    /// Workflow run database id.
    pub run_id: u64,
    /// `owner/repo` slug.
    pub repo: String,
    /// Head branch.
    pub head_branch: String,
    /// Head SHA.
    pub head_sha: String,
    /// GitHub run status.
    pub status: String,
    /// GitHub run conclusion.
    pub conclusion: Option<String>,
    /// Workflow name.
    pub workflow_name: String,
    /// GitHub HTML URL.
    pub html_url: Option<String>,
}

/// Normalized GitHub `workflow_job` payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkflowJobPayload {
    /// GitHub webhook action.
    pub action: String,
    /// Parent workflow run id.
    pub run_id: u64,
    /// Workflow job id.
    pub job_id: u64,
    /// `owner/repo` slug.
    pub repo: String,
    /// Job name.
    pub name: String,
    /// GitHub job status.
    pub status: String,
    /// GitHub job conclusion.
    pub conclusion: Option<String>,
    /// Runner name, when assigned.
    pub runner_name: Option<String>,
    /// Job labels.
    pub labels: Vec<String>,
}

/// Normalized GitHub `pull_request` payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PullRequestPayload {
    /// GitHub webhook action.
    pub action: String,
    /// Pull request number.
    pub number: u64,
    /// `owner/repo` slug.
    pub repo: String,
    /// Pull request state.
    pub state: String,
    /// Whether GitHub reports the PR merged.
    pub merged: bool,
    /// Merge timestamp.
    pub merged_at: Option<String>,
    /// Close timestamp.
    pub closed_at: Option<String>,
}

/// Normalized GitHub `check_run` payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckRunPayload {
    /// GitHub webhook action.
    pub action: String,
    /// `owner/repo` slug.
    pub repo: String,
    /// Check name.
    pub name: String,
    /// GitHub check status.
    pub status: String,
    /// GitHub check conclusion.
    pub conclusion: Option<String>,
    /// Head SHA.
    pub head_sha: String,
    /// Pull request numbers linked to this check.
    pub pull_request_numbers: Vec<u64>,
}

/// Normalized GitHub `check_suite` payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckSuitePayload {
    /// GitHub webhook action.
    pub action: String,
    /// `owner/repo` slug.
    pub repo: String,
    /// GitHub suite status.
    pub status: String,
    /// GitHub suite conclusion.
    pub conclusion: Option<String>,
    /// Head SHA.
    pub head_sha: String,
    /// Pull request numbers linked to this suite.
    pub pull_request_numbers: Vec<u64>,
}

/// Release asset summary included in `release` webhook payloads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReleaseAssetInfo {
    /// Asset filename.
    pub name: String,
    /// GitHub asset state.
    pub state: String,
    /// Asset size in bytes.
    pub size: u64,
}

/// Normalized GitHub `release` payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReleasePayload {
    /// GitHub webhook action.
    pub action: String,
    /// `owner/repo` slug.
    pub repo: String,
    /// Release tag.
    pub tag_name: String,
    /// Whether the release is draft.
    pub draft: bool,
    /// Whether the release is a prerelease.
    pub prerelease: bool,
    /// Release assets.
    pub assets: Vec<ReleaseAssetInfo>,
}

/// Tagged webhook event delivered over daemon IPC.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebhookEvent {
    /// `workflow_run` event.
    WorkflowRun(WorkflowRunPayload),
    /// `workflow_job` event.
    WorkflowJob(WorkflowJobPayload),
    /// `pull_request` event.
    PullRequest(PullRequestPayload),
    /// `check_run` event.
    CheckRun(CheckRunPayload),
    /// `check_suite` event.
    CheckSuite(CheckSuitePayload),
    /// `release` event.
    Release(ReleasePayload),
    /// Unknown but syntactically valid GitHub event.
    Unhandled {
        /// Raw GitHub event type.
        event_type: String,
    },
}

impl WebhookEvent {
    /// Return the IPC kind label.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::WorkflowRun(_) => "workflow_run",
            Self::WorkflowJob(_) => "workflow_job",
            Self::PullRequest(_) => "pull_request",
            Self::CheckRun(_) => "check_run",
            Self::CheckSuite(_) => "check_suite",
            Self::Release(_) => "release",
            Self::Unhandled { .. } => "unhandled",
        }
    }

    /// Render the Python-compatible IPC event wire shape.
    #[must_use]
    pub fn to_wire(&self) -> Value {
        match self {
            Self::WorkflowRun(payload) => event_value("workflow_run", payload),
            Self::WorkflowJob(payload) => event_value("workflow_job", payload),
            Self::PullRequest(payload) => event_value("pull_request", payload),
            Self::CheckRun(payload) => event_value("check_run", payload),
            Self::CheckSuite(payload) => event_value("check_suite", payload),
            Self::Release(payload) => event_value("release", payload),
            Self::Unhandled { event_type } => json!({
                "kind": "unhandled",
                "type": event_type,
            }),
        }
    }
}

/// Compute the GitHub HMAC-SHA256 hex digest for a webhook body.
#[must_use]
pub fn hmac_sha256_hex(body: &[u8], secret: &str) -> String {
    const BLOCK_SIZE: usize = 64;
    let mut key = secret.as_bytes().to_vec();
    if key.len() > BLOCK_SIZE {
        key = Sha256::digest(&key).to_vec();
    }
    key.resize(BLOCK_SIZE, 0);

    let mut outer = [0x5c_u8; BLOCK_SIZE];
    let mut inner = [0x36_u8; BLOCK_SIZE];
    for (index, byte) in key.iter().enumerate() {
        outer[index] ^= byte;
        inner[index] ^= byte;
    }

    let mut inner_hash = Sha256::new();
    inner_hash.update(inner);
    inner_hash.update(body);
    let inner_result = inner_hash.finalize();

    let mut outer_hash = Sha256::new();
    outer_hash.update(outer);
    outer_hash.update(inner_result);
    hex::encode(outer_hash.finalize())
}

/// Validate GitHub's `X-Hub-Signature-256` header.
#[must_use]
pub fn is_valid_signature(body: &[u8], secret: &str, header: Option<&str>) -> bool {
    let Some(header) = header else {
        return false;
    };
    let provided = header.strip_prefix("sha256=").unwrap_or(header);
    constant_time_eq(
        hmac_sha256_hex(body, secret).as_bytes(),
        provided.as_bytes(),
    )
}

/// Decode a raw GitHub webhook delivery into a normalized IPC event.
#[must_use]
pub fn decode_webhook_event(event_header: Option<&str>, body: &[u8]) -> Option<WebhookEvent> {
    let event_header = event_header?;
    let value = serde_json::from_slice::<Value>(body).ok()?;
    let object = value.as_object()?;
    match event_header {
        "workflow_run" => decode_workflow_run(object),
        "workflow_job" => decode_workflow_job(object),
        "pull_request" => decode_pull_request(object),
        "check_run" => decode_check_run(object),
        "check_suite" => decode_check_suite(object),
        "release" => decode_release(object),
        other => Some(WebhookEvent::Unhandled {
            event_type: other.to_owned(),
        }),
    }
}

fn event_value<T: Serialize>(kind: &str, payload: &T) -> Value {
    json!({
        "kind": kind,
        "payload": serde_json::to_value(payload).expect("webhook payload serializes"),
    })
}

fn decode_workflow_run(object: &serde_json::Map<String, Value>) -> Option<WebhookEvent> {
    let run = object.get("workflow_run")?.as_object()?;
    let repo = repo_full_name(object)?;
    Some(WebhookEvent::WorkflowRun(WorkflowRunPayload {
        action: string_field(object.get("action")),
        run_id: as_u64(run.get("id")?)?,
        repo,
        head_branch: string_field(run.get("head_branch")),
        head_sha: string_field(run.get("head_sha")),
        status: string_field(run.get("status")),
        conclusion: optional_string(run.get("conclusion")),
        workflow_name: string_field(run.get("name")),
        html_url: optional_string(run.get("html_url")),
    }))
}

fn decode_workflow_job(object: &serde_json::Map<String, Value>) -> Option<WebhookEvent> {
    let job = object.get("workflow_job")?.as_object()?;
    let repo = repo_full_name(object)?;
    let labels = job
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .map(|label| string_field(Some(label)))
                .collect()
        })
        .unwrap_or_default();
    Some(WebhookEvent::WorkflowJob(WorkflowJobPayload {
        action: string_field(object.get("action")),
        run_id: as_u64(job.get("run_id")?)?,
        job_id: as_u64(job.get("id")?)?,
        repo,
        name: string_field(job.get("name")),
        status: string_field(job.get("status")),
        conclusion: optional_string(job.get("conclusion")),
        runner_name: optional_string(job.get("runner_name")),
        labels,
    }))
}

fn decode_pull_request(object: &serde_json::Map<String, Value>) -> Option<WebhookEvent> {
    let pull_request = object.get("pull_request")?.as_object()?;
    let repo = repo_full_name(object)?;
    Some(WebhookEvent::PullRequest(PullRequestPayload {
        action: string_field(object.get("action")),
        number: as_u64(pull_request.get("number")?)?,
        repo,
        state: string_field(pull_request.get("state")),
        merged: pull_request
            .get("merged")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        merged_at: optional_string(pull_request.get("merged_at")),
        closed_at: optional_string(pull_request.get("closed_at")),
    }))
}

fn decode_check_run(object: &serde_json::Map<String, Value>) -> Option<WebhookEvent> {
    let check = object.get("check_run")?.as_object()?;
    let repo = repo_full_name(object)?;
    Some(WebhookEvent::CheckRun(CheckRunPayload {
        action: string_field(object.get("action")),
        repo,
        name: string_field(check.get("name")),
        status: string_field(check.get("status")),
        conclusion: optional_string(check.get("conclusion")),
        head_sha: string_field(check.get("head_sha")),
        pull_request_numbers: pull_request_numbers(check.get("pull_requests")),
    }))
}

fn decode_check_suite(object: &serde_json::Map<String, Value>) -> Option<WebhookEvent> {
    let suite = object.get("check_suite")?.as_object()?;
    let repo = repo_full_name(object)?;
    Some(WebhookEvent::CheckSuite(CheckSuitePayload {
        action: string_field(object.get("action")),
        repo,
        status: string_field(suite.get("status")),
        conclusion: optional_string(suite.get("conclusion")),
        head_sha: string_field(suite.get("head_sha")),
        pull_request_numbers: pull_request_numbers(suite.get("pull_requests")),
    }))
}

fn decode_release(object: &serde_json::Map<String, Value>) -> Option<WebhookEvent> {
    let release = object.get("release")?.as_object()?;
    let repo = repo_full_name(object)?;
    let assets = release
        .get("assets")
        .and_then(Value::as_array)
        .map(|assets| {
            assets
                .iter()
                .filter_map(Value::as_object)
                .map(|asset| ReleaseAssetInfo {
                    name: string_field(asset.get("name")),
                    state: string_field(asset.get("state")),
                    size: asset.get("size").and_then(as_u64).unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();
    Some(WebhookEvent::Release(ReleasePayload {
        action: string_field(object.get("action")),
        repo,
        tag_name: string_field(release.get("tag_name")),
        draft: release
            .get("draft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        prerelease: release
            .get("prerelease")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        assets,
    }))
}

fn repo_full_name(object: &serde_json::Map<String, Value>) -> Option<String> {
    object
        .get("repository")?
        .get("full_name")?
        .as_str()
        .map(str::to_owned)
}

fn pull_request_numbers(value: Option<&Value>) -> Vec<u64> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_object)
                .filter_map(|pull_request| pull_request.get("number").and_then(as_u64))
                .collect()
        })
        .unwrap_or_default()
}

fn string_field(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn as_u64(value: &Value) -> Option<u64> {
    value.as_u64()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |diff, (left, right)| diff | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{WebhookEvent, decode_webhook_event, hmac_sha256_hex, is_valid_signature};

    #[test]
    fn hmac_signature_matches_github_header_shape() {
        let body = br#"{"zen":"Keep it logically awesome."}"#;
        let digest = hmac_sha256_hex(body, "secret");

        assert_eq!(
            digest,
            "b4d0fd3983e1d5612eaebe005a2092e7176a5e0e6a583899433148eb91c11b4e"
        );
        assert!(is_valid_signature(
            body,
            "secret",
            Some(&format!("sha256={digest}"))
        ));
        assert!(is_valid_signature(body, "secret", Some(&digest)));
        assert!(!is_valid_signature(body, "wrong", Some(&digest)));
        assert!(!is_valid_signature(body, "secret", None));
    }

    #[test]
    fn missing_header_or_invalid_json_returns_none() {
        assert!(decode_webhook_event(None, b"{}").is_none());
        assert!(decode_webhook_event(Some("workflow_run"), b"not-json").is_none());
        assert!(decode_webhook_event(Some("workflow_run"), b"[]").is_none());
    }

    #[test]
    fn workflow_run_decodes_to_python_wire_shape() {
        let body = json!({
            "action": "completed",
            "repository": {"full_name": "owner/repo"},
            "workflow_run": {
                "id": 123,
                "head_branch": "feature/x",
                "head_sha": "abc",
                "status": "completed",
                "conclusion": "success",
                "name": "CI",
                "html_url": "https://example/run/123"
            }
        })
        .to_string();

        let event = decode_webhook_event(Some("workflow_run"), body.as_bytes()).expect("event");
        assert!(matches!(event, WebhookEvent::WorkflowRun(_)));
        assert_eq!(
            event.to_wire(),
            json!({
                "kind": "workflow_run",
                "payload": {
                    "action": "completed",
                    "run_id": 123,
                    "repo": "owner/repo",
                    "head_branch": "feature/x",
                    "head_sha": "abc",
                    "status": "completed",
                    "conclusion": "success",
                    "workflow_name": "CI",
                    "html_url": "https://example/run/123"
                }
            })
        );
    }

    #[test]
    fn workflow_job_decodes_labels_and_runner() {
        let body = json!({
            "action": "queued",
            "repository": {"full_name": "owner/repo"},
            "workflow_job": {
                "id": 456,
                "run_id": 123,
                "name": "Build",
                "status": "queued",
                "conclusion": null,
                "runner_name": "runner-1",
                "labels": ["self-hosted", "macos"]
            }
        })
        .to_string();

        let event = decode_webhook_event(Some("workflow_job"), body.as_bytes()).expect("event");
        assert_eq!(
            event.to_wire()["payload"]["labels"],
            json!(["self-hosted", "macos"])
        );
        assert_eq!(event.to_wire()["payload"]["runner_name"], "runner-1");
    }

    #[test]
    fn pull_request_check_and_suite_events_preserve_pr_numbers() {
        let check = json!({
            "action": "completed",
            "repository": {"full_name": "owner/repo"},
            "check_run": {
                "name": "CI",
                "status": "completed",
                "conclusion": "success",
                "head_sha": "abc",
                "pull_requests": [{"number": 10}, {"number": true}, {"number": 11}]
            }
        })
        .to_string();
        let suite = json!({
            "action": "completed",
            "repository": {"full_name": "owner/repo"},
            "check_suite": {
                "status": "completed",
                "conclusion": "success",
                "head_sha": "abc",
                "pull_requests": [{"number": 10}]
            }
        })
        .to_string();
        let pull_request = json!({
            "action": "closed",
            "repository": {"full_name": "owner/repo"},
            "pull_request": {
                "number": 10,
                "state": "closed",
                "merged": true,
                "merged_at": "2026-04-25T08:00:00Z",
                "closed_at": "2026-04-25T08:00:00Z"
            }
        })
        .to_string();

        assert_eq!(
            decode_webhook_event(Some("check_run"), check.as_bytes())
                .expect("check")
                .to_wire()["payload"]["pull_request_numbers"],
            json!([10, 11])
        );
        assert_eq!(
            decode_webhook_event(Some("check_suite"), suite.as_bytes())
                .expect("suite")
                .to_wire()["payload"]["pull_request_numbers"],
            json!([10])
        );
        assert_eq!(
            decode_webhook_event(Some("pull_request"), pull_request.as_bytes())
                .expect("pr")
                .to_wire()["payload"]["merged"],
            true
        );
    }

    #[test]
    fn release_event_decodes_assets() {
        let body = json!({
            "action": "published",
            "repository": {"full_name": "owner/repo"},
            "release": {
                "tag_name": "v1.2.3",
                "draft": false,
                "prerelease": true,
                "assets": [
                    {"name": "shipyard.dmg", "state": "uploaded", "size": 42},
                    "ignored"
                ]
            }
        })
        .to_string();

        let event = decode_webhook_event(Some("release"), body.as_bytes()).expect("event");
        assert_eq!(event.to_wire()["payload"]["tag_name"], "v1.2.3");
        assert_eq!(event.to_wire()["payload"]["assets"][0]["size"], 42);
    }

    #[test]
    fn unknown_event_is_unhandled_but_preserved() {
        let event = decode_webhook_event(Some("ping"), b"{}").expect("event");

        assert_eq!(
            event.to_wire(),
            json!({"kind": "unhandled", "type": "ping"})
        );
    }
}
