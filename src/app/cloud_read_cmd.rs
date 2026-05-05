use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitCode};

use chrono::Utc;
use serde_json::Value;

use super::CliFailure;
use crate::cloud::{
    CloudDispatchPlan, GitHubActions, default_workflow_key, discover_workflows,
    resolve_cloud_dispatch_plan,
};
use crate::cloud_records::{CloudRecordStore, CloudRunRecord};
use crate::config::LoadedConfig;
use crate::output::write_json_envelope;

pub(super) fn cloud_workflows<W: Write>(
    config: &LoadedConfig,
    cwd: &Path,
    json: bool,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    let workflows = discover_workflows(cwd);
    let default = default_workflow_key(config, &workflows);
    let mut data = BTreeMap::new();
    data.insert(
        "workflows".to_owned(),
        serde_json::to_value(&workflows).expect("workflows serialize"),
    );
    data.insert(
        "default".to_owned(),
        default.map_or(Value::Null, Value::from),
    );
    if json {
        write_json_envelope(stdout, "cloud.workflows", data)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(ExitCode::SUCCESS);
    }
    if workflows.is_empty() {
        writeln!(stdout, "No GitHub workflows discovered.")
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(ExitCode::SUCCESS);
    }
    writeln!(stdout, "Discovered workflows:")
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    for (key, workflow) in workflows {
        writeln!(stdout, "  {key}: {}", workflow.description)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
    }
    Ok(ExitCode::SUCCESS)
}

pub(super) fn cloud_defaults<W: Write>(
    config: &LoadedConfig,
    cwd: &Path,
    json: bool,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    let workflows = discover_workflows(cwd);
    let default_key = default_workflow_key(config, &workflows);
    let provider = config
        .get_str("cloud.provider")
        .unwrap_or("github-hosted")
        .to_owned();
    let ref_name = current_git_branch(cwd).unwrap_or_else(|| "main".to_owned());
    let mut resolved = BTreeMap::new();
    for key in workflows.keys() {
        if let Ok(plan) = resolve_cloud_dispatch_plan(config, &workflows, key, &ref_name, None) {
            resolved.insert(key.clone(), plan_to_value(&plan));
        }
    }
    let mut data = BTreeMap::new();
    data.insert(
        "repository".to_owned(),
        config
            .get_str("cloud.repository")
            .map_or(Value::Null, |repo| Value::String(repo.to_owned())),
    );
    data.insert(
        "default_workflow".to_owned(),
        default_key.clone().map_or(Value::Null, Value::from),
    );
    data.insert(
        "default_provider".to_owned(),
        Value::String(provider.clone()),
    );
    data.insert(
        "workflows".to_owned(),
        serde_json::to_value(&workflows).expect("workflows serialize"),
    );
    data.insert(
        "resolved".to_owned(),
        serde_json::to_value(&resolved).expect("resolved plans serialize"),
    );
    if json {
        write_json_envelope(stdout, "cloud.defaults", data)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(ExitCode::SUCCESS);
    }
    writeln!(
        stdout,
        "repository: {}",
        config.get_str("cloud.repository").unwrap_or("current repo")
    )
    .map_err(|error| CliFailure::new(1, error.to_string()))?;
    writeln!(
        stdout,
        "default workflow: {}",
        default_key.as_deref().unwrap_or("none")
    )
    .map_err(|error| CliFailure::new(1, error.to_string()))?;
    writeln!(stdout, "default provider: {provider}")
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    if !workflows.is_empty() {
        writeln!(stdout, "workflows:").map_err(|error| CliFailure::new(1, error.to_string()))?;
        for (key, workflow) in workflows {
            let fields = resolved
                .get(&key)
                .and_then(|value| value.get("dispatch_fields"))
                .and_then(Value::as_object)
                .map(|fields| {
                    fields
                        .iter()
                        .map(|(name, value)| {
                            format!("{}={}", name, value.as_str().unwrap_or_default())
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|summary| !summary.is_empty())
                .unwrap_or_else(|| "no dispatch fields".to_owned());
            writeln!(stdout, "  {key}: {} ({fields})", workflow.file)
                .map_err(|error| CliFailure::new(1, error.to_string()))?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub(super) fn cloud_status<W: Write>(
    records: &CloudRecordStore,
    actions: &GitHubActions,
    identifier: Option<&str>,
    limit: usize,
    refresh: bool,
    json: bool,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    let mut selected = select_records(records, identifier, limit);
    let mut refreshed = Vec::new();
    for record in selected.drain(..) {
        refreshed.push(refresh_record(records, actions, record, refresh)?);
    }

    if json {
        let mut data = BTreeMap::new();
        data.insert(
            "records".to_owned(),
            serde_json::to_value(&refreshed).expect("cloud records serialize"),
        );
        write_json_envelope(stdout, "cloud.status", data)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(ExitCode::SUCCESS);
    }

    if refreshed.is_empty() {
        writeln!(stdout, "No tracked cloud runs yet.")
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(ExitCode::SUCCESS);
    }
    for record in refreshed {
        writeln!(
            stdout,
            "{}: {} ref={} provider={} status={} conclusion={}",
            record.dispatch_id,
            record.workflow_key,
            record.requested_ref,
            record.provider,
            record.status,
            record.conclusion.as_deref().unwrap_or("-"),
        )
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    }
    Ok(ExitCode::SUCCESS)
}

fn plan_to_value(plan: &CloudDispatchPlan) -> Value {
    let mut data = serde_json::Map::new();
    data.insert(
        "workflow".to_owned(),
        serde_json::to_value(&plan.workflow).expect("workflow serializes"),
    );
    data.insert(
        "repository".to_owned(),
        plan.repository.clone().map_or(Value::Null, Value::String),
    );
    data.insert("ref".to_owned(), Value::String(plan.ref_name.clone()));
    data.insert("provider".to_owned(), Value::String(plan.provider.clone()));
    data.insert(
        "dispatch_fields".to_owned(),
        serde_json::to_value(&plan.dispatch_fields).expect("dispatch fields serialize"),
    );
    data.insert(
        "sources".to_owned(),
        serde_json::to_value(&plan.sources).expect("sources serialize"),
    );
    Value::Object(data)
}

fn current_git_branch(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

fn select_records(
    records: &CloudRecordStore,
    identifier: Option<&str>,
    limit: usize,
) -> Vec<CloudRunRecord> {
    match identifier {
        Some("latest" | "") => records.list(limit).into_iter().take(1).collect(),
        Some(dispatch_id) => records.get(dispatch_id).into_iter().collect(),
        None => records.list(limit),
    }
}

fn refresh_record(
    records: &CloudRecordStore,
    actions: &GitHubActions,
    mut record: CloudRunRecord,
    refresh: bool,
) -> Result<CloudRunRecord, CliFailure> {
    if !refresh {
        return Ok(record);
    }
    let Some(run_id) = record
        .run_id
        .as_deref()
        .and_then(|value| value.parse().ok())
    else {
        return Ok(record);
    };
    let Ok(view) = actions.workflow_run_status(record.repository.as_deref(), run_id) else {
        return Ok(record);
    };
    record.status = view.status;
    record.conclusion = view.conclusion;
    record.url = view.url.or(record.url);
    record.updated_at = Some(Utc::now());
    if record.status == "completed" {
        record.completed_at = Some(Utc::now());
    }
    records
        .save(&record)
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    Ok(record)
}
