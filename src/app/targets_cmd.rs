use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use toml::{Table, Value as TomlValue};

use super::{
    CliFailure,
    cli::{TargetBackend, TargetsCommand, TargetsWarmCommand},
};
use crate::config::LoadedConfig;
use crate::executor::dispatch::{
    ExecutorDispatcher, ResolvedBackend, ResolvedTarget, resolve_targets,
    resolve_targets_from_table,
};
use crate::identity::RuntimeMode;
use crate::job::ValidationMode;
use crate::output::write_json_envelope;
use crate::warm_pool::{WarmPool, default_pool_path, now_epoch_secs};

pub(super) fn targets_command<W: Write>(
    command: Option<TargetsCommand>,
    mode: RuntimeMode,
    cwd: &Path,
    state_dir: &Path,
    json_mode: bool,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    let config = LoadedConfig::load_from_cwd(mode, cwd)
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    match command.unwrap_or(TargetsCommand::List) {
        TargetsCommand::List => targets_list(&config, json_mode, stdout)?,
        TargetsCommand::Test { name } => return targets_test(&config, &name, json_mode, stdout),
        TargetsCommand::Add {
            name,
            backend,
            platform,
            host,
            repo_path,
        } => {
            let request = TargetAddRequest {
                name,
                backend,
                config: NewTargetConfig {
                    backend: backend.as_str().to_owned(),
                    platform,
                    host,
                    repo_path,
                },
            };
            targets_add(&config, &request, json_mode, stdout)?;
        }
        TargetsCommand::Remove { name } => targets_remove(&config, &name, json_mode, stdout)?,
        TargetsCommand::Warm { command } => {
            targets_warm(command, state_dir, json_mode, stdout)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn targets_list<W: Write>(
    config: &LoadedConfig,
    json_mode: bool,
    stdout: &mut W,
) -> Result<(), CliFailure> {
    if target_tables(config).is_none_or(Table::is_empty) {
        if json_mode {
            write_targets_list_json(stdout, Vec::new())?;
        } else {
            writeln!(stdout, "No targets configured. Run `shipyard init`.")
                .map_err(|error| CliFailure::new(1, error.to_string()))?;
        }
        return Ok(());
    }

    let targets = resolve_targets(config, ValidationMode::Full)
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    let rows = target_rows(&targets);
    if json_mode {
        write_targets_list_json(stdout, rows)?;
    } else {
        write_targets_list_human(stdout, &rows)?;
    }
    Ok(())
}

fn targets_test<W: Write>(
    config: &LoadedConfig,
    name: &str,
    json_mode: bool,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    if !target_tables(config).is_some_and(|targets| targets.contains_key(name)) {
        return Err(CliFailure::new(
            1,
            format!("Target '{name}' not configured"),
        ));
    }
    let targets = resolve_targets(config, ValidationMode::Full)
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    let target = targets
        .iter()
        .find(|target| target.name == name)
        .ok_or_else(|| CliFailure::new(1, format!("Target '{name}' not configured")))?;
    let dispatcher = ExecutorDispatcher::new(None);
    let (reachable, active_backend) = probe_target(&dispatcher, target);
    if json_mode {
        let mut data = BTreeMap::new();
        data.insert("name".to_owned(), Value::String(name.to_owned()));
        data.insert("reachable".to_owned(), Value::Bool(reachable));
        data.insert(
            "active_backend".to_owned(),
            active_backend
                .as_ref()
                .map_or(Value::Null, |backend| Value::String(backend.clone())),
        );
        write_json_envelope(stdout, "targets.test", data)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(ExitCode::SUCCESS);
    }

    if reachable {
        writeln!(
            stdout,
            "{name}: reachable via {}",
            active_backend.unwrap_or_else(|| target.backend_name.clone())
        )
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
        Ok(ExitCode::SUCCESS)
    } else {
        writeln!(stdout, "{name}: unreachable")
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        Ok(ExitCode::FAILURE)
    }
}

fn targets_add<W: Write>(
    config: &LoadedConfig,
    request: &TargetAddRequest,
    json_mode: bool,
    stdout: &mut W,
) -> Result<(), CliFailure> {
    let name = request.name.as_str();
    let project_dir = config.project_dir.as_ref().ok_or_else(|| {
        CliFailure::new(
            1,
            "No .shipyard/config.toml found. Run `shipyard init` first.",
        )
    })?;
    if target_tables(config).is_some_and(|targets| targets.contains_key(name)) {
        return Err(CliFailure::new(
            1,
            format!("Target '{name}' already exists. Remove it first or pick another name."),
        ));
    }
    if matches!(
        request.backend,
        TargetBackend::Ssh | TargetBackend::SshWindows
    ) && request.config.host.is_none()
    {
        return Err(CliFailure::new(
            1,
            format!(
                "--host is required for backend={}",
                request.backend.as_str()
            ),
        ));
    }

    if matches!(
        request.backend,
        TargetBackend::Ssh | TargetBackend::SshWindows
    ) && !probe_new_target(name, &request.config)?
        && !json_mode
        && let Some(host) = request.config.host.as_deref()
    {
        writeln!(
            stdout,
            "warning: {host} is not reachable right now. Adding anyway."
        )
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    }

    let config_path = project_dir.join("config.toml");
    append_target_section(&config_path, name, &request.config)?;
    if json_mode {
        let mut data = BTreeMap::new();
        data.insert("name".to_owned(), Value::String(name.to_owned()));
        data.insert(
            "config".to_owned(),
            serde_json::to_value(&request.config)
                .map_err(|error| CliFailure::new(1, error.to_string()))?,
        );
        write_json_envelope(stdout, "targets.add", data)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
    } else {
        writeln!(stdout, "Added target '{name}' to {}", config_path.display())
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
    }
    Ok(())
}

fn targets_remove<W: Write>(
    config: &LoadedConfig,
    name: &str,
    json_mode: bool,
    stdout: &mut W,
) -> Result<(), CliFailure> {
    let project_dir = config
        .project_dir
        .as_ref()
        .ok_or_else(|| CliFailure::new(1, "No .shipyard/config.toml found."))?;
    if !target_tables(config).is_some_and(|targets| targets.contains_key(name)) {
        return Err(CliFailure::new(1, format!("Target '{name}' not found")));
    }
    let config_path = project_dir.join("config.toml");
    remove_target_section(&config_path, name)?;
    if json_mode {
        let mut data = BTreeMap::new();
        data.insert("name".to_owned(), Value::String(name.to_owned()));
        write_json_envelope(stdout, "targets.remove", data)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
    } else {
        writeln!(
            stdout,
            "Removed target '{name}' from {}",
            config_path.display()
        )
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    }
    Ok(())
}

fn targets_warm<W: Write>(
    command: Option<TargetsWarmCommand>,
    state_dir: &Path,
    json_mode: bool,
    stdout: &mut W,
) -> Result<(), CliFailure> {
    match command.unwrap_or(TargetsWarmCommand::Status) {
        TargetsWarmCommand::Status => targets_warm_status(state_dir, json_mode, stdout),
        TargetsWarmCommand::Drain { yes } => targets_warm_drain(state_dir, yes, json_mode, stdout),
    }
}

fn targets_warm_status<W: Write>(
    state_dir: &Path,
    json_mode: bool,
    stdout: &mut W,
) -> Result<(), CliFailure> {
    let now = now_epoch_secs();
    let pool = WarmPool::new(default_pool_path(state_dir));
    pool.prune_expired(now)
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    let rows = pool
        .all_entries()
        .into_iter()
        .map(|entry| WarmEntryRow {
            target: entry.target,
            host: entry.host,
            backend: entry.backend,
            workdir: entry.workdir,
            sha: entry.sha,
            ttl_remaining_secs: round_one((entry.expires_at - now).max(0.0)),
            expires_at: isoformat_epoch(entry.expires_at),
            created_at: isoformat_epoch(entry.created_at),
        })
        .collect::<Vec<_>>();

    if json_mode {
        let mut data = BTreeMap::new();
        data.insert(
            "entries".to_owned(),
            serde_json::to_value(&rows).map_err(|error| CliFailure::new(1, error.to_string()))?,
        );
        write_json_envelope(stdout, "targets.warm.status", data)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(());
    }

    if rows.is_empty() {
        writeln!(stdout, "Warm pool is empty.")
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(());
    }

    writeln!(stdout).map_err(|error| CliFailure::new(1, error.to_string()))?;
    writeln!(stdout, "Warm pool").map_err(|error| CliFailure::new(1, error.to_string()))?;
    for row in rows {
        writeln!(
            stdout,
            "  {:<16} {:<20} sha={} ttl={:>6.0}s workdir={}",
            row.target,
            row.host,
            short_sha(&row.sha),
            row.ttl_remaining_secs,
            row.workdir
        )
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    }
    writeln!(stdout).map_err(|error| CliFailure::new(1, error.to_string()))
}

fn targets_warm_drain<W: Write>(
    state_dir: &Path,
    yes: bool,
    json_mode: bool,
    stdout: &mut W,
) -> Result<(), CliFailure> {
    let pool = WarmPool::new(default_pool_path(state_dir));
    let existing = pool.all_entries().len();
    if existing == 0 {
        if json_mode {
            write_warm_drain_json(stdout, 0)?;
        } else {
            writeln!(stdout, "Warm pool already empty.")
                .map_err(|error| CliFailure::new(1, error.to_string()))?;
        }
        return Ok(());
    }
    if !yes && !json_mode {
        writeln!(stdout, "Aborted.").map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(());
    }
    let drained = pool
        .drain()
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    if json_mode {
        write_warm_drain_json(stdout, drained)?;
    } else {
        writeln!(stdout, "Drained {drained} warm-pool entries.")
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct TargetRow {
    name: String,
    backend: String,
    platform: String,
    reachable: bool,
    active_backend: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
struct TargetAddRequest {
    name: String,
    backend: TargetBackend,
    config: NewTargetConfig,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct NewTargetConfig {
    backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_path: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
struct WarmEntryRow {
    target: String,
    host: String,
    backend: String,
    workdir: String,
    sha: String,
    ttl_remaining_secs: f64,
    expires_at: String,
    created_at: String,
}

fn target_tables(config: &LoadedConfig) -> Option<&Table> {
    config.get("targets").and_then(TomlValue::as_table)
}

fn target_rows(targets: &[ResolvedTarget]) -> Vec<TargetRow> {
    let dispatcher = ExecutorDispatcher::new(None);
    targets
        .iter()
        .map(|target| {
            let (reachable, active_backend) = probe_target(&dispatcher, target);
            TargetRow {
                name: target.name.clone(),
                backend: target.backend_name.clone(),
                platform: target.platform.clone(),
                reachable,
                active_backend,
            }
        })
        .collect()
}

fn probe_target(
    dispatcher: &ExecutorDispatcher,
    target: &ResolvedTarget,
) -> (bool, Option<String>) {
    if let ResolvedBackend::Fallback(chain) = &target.backend {
        for backend in &chain.backends {
            if dispatcher.probe(&backend.target) {
                return (true, Some(backend.target.backend_name.clone()));
            }
        }
        return (false, None);
    }
    if dispatcher.probe(target) {
        (true, Some(target.backend_name.clone()))
    } else {
        (false, None)
    }
}

fn probe_new_target(name: &str, target: &NewTargetConfig) -> Result<bool, CliFailure> {
    let mut target_table = Table::new();
    target_table.insert(
        "backend".to_owned(),
        TomlValue::String(target.backend.clone()),
    );
    if let Some(platform) = &target.platform {
        target_table.insert("platform".to_owned(), TomlValue::String(platform.clone()));
    }
    if let Some(host) = &target.host {
        target_table.insert("host".to_owned(), TomlValue::String(host.clone()));
    }
    if let Some(repo_path) = &target.repo_path {
        target_table.insert("repo_path".to_owned(), TomlValue::String(repo_path.clone()));
    }
    let mut targets = Table::new();
    targets.insert(name.to_owned(), TomlValue::Table(target_table));
    let mut root = Table::new();
    root.insert("targets".to_owned(), TomlValue::Table(targets));
    let resolved = resolve_targets_from_table(&root, ValidationMode::Full)
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    Ok(resolved
        .first()
        .is_some_and(|target| ExecutorDispatcher::new(None).probe(target)))
}

fn write_targets_list_json<W: Write>(
    stdout: &mut W,
    rows: Vec<TargetRow>,
) -> Result<(), CliFailure> {
    let mut data = BTreeMap::new();
    data.insert(
        "targets".to_owned(),
        serde_json::to_value(rows).map_err(|error| CliFailure::new(1, error.to_string()))?,
    );
    write_json_envelope(stdout, "targets.list", data)
        .map_err(|error| CliFailure::new(1, error.to_string()))
}

fn write_targets_list_human<W: Write>(
    stdout: &mut W,
    rows: &[TargetRow],
) -> Result<(), CliFailure> {
    writeln!(stdout).map_err(|error| CliFailure::new(1, error.to_string()))?;
    writeln!(stdout, "Targets").map_err(|error| CliFailure::new(1, error.to_string()))?;
    for row in rows {
        let status = if row.reachable {
            "reachable"
        } else {
            "unreachable"
        };
        writeln!(
            stdout,
            "  {:<16} {:<12} {:<16} {status}",
            row.name, row.backend, row.platform
        )
        .map_err(|error| CliFailure::new(1, error.to_string()))?;
    }
    writeln!(stdout).map_err(|error| CliFailure::new(1, error.to_string()))
}

fn append_target_section(
    config_path: &Path,
    name: &str,
    target: &NewTargetConfig,
) -> Result<(), CliFailure> {
    let mut text =
        fs::read_to_string(config_path).map_err(|error| CliFailure::new(1, error.to_string()))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.ends_with("\n\n") {
        text.push('\n');
    }
    text.push_str(&render_target_section(name, target));
    fs::write(config_path, text).map_err(|error| CliFailure::new(1, error.to_string()))
}

fn render_target_section(name: &str, target: &NewTargetConfig) -> String {
    let mut section = format!(
        "[targets.{name}]\nbackend = {}\n",
        toml_quote(&target.backend)
    );
    if let Some(platform) = &target.platform {
        let _ = writeln!(section, "platform = {}", toml_quote(platform));
    }
    if let Some(host) = &target.host {
        let _ = writeln!(section, "host = {}", toml_quote(host));
    }
    if let Some(repo_path) = &target.repo_path {
        let _ = writeln!(section, "repo_path = {}", toml_quote(repo_path));
    }
    section
}

fn remove_target_section(config_path: &Path, name: &str) -> Result<(), CliFailure> {
    let text =
        fs::read_to_string(config_path).map_err(|error| CliFailure::new(1, error.to_string()))?;
    let mut output = String::new();
    let mut skipping = false;
    let section_marker = format!("[targets.{name}]");
    for line in text.split_inclusive('\n') {
        let stripped = line.trim();
        if stripped == section_marker {
            skipping = true;
            continue;
        }
        if skipping && stripped.starts_with('[') && stripped.ends_with(']') {
            skipping = false;
            output.push_str(line);
            continue;
        }
        if !skipping {
            output.push_str(line);
        }
    }
    fs::write(config_path, output).map_err(|error| CliFailure::new(1, error.to_string()))
}

fn write_warm_drain_json<W: Write>(stdout: &mut W, drained: usize) -> Result<(), CliFailure> {
    let mut data = BTreeMap::new();
    data.insert("drained".to_owned(), json!(drained));
    write_json_envelope(stdout, "targets.warm.drain", data)
        .map_err(|error| CliFailure::new(1, error.to_string()))
}

fn toml_quote(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}

fn isoformat_epoch(epoch: f64) -> String {
    let system_time = UNIX_EPOCH + Duration::from_secs_f64(epoch.max(0.0));
    DateTime::<Utc>::from(system_time).to_rfc3339()
}

fn round_one(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn short_sha(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}
