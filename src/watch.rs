use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::evidence::EvidenceStore;
use crate::output::write_json_envelope;
use crate::ship_state::{DispatchedRun, ShipState, ShipStateStore};

const QUEUED_STATUSES: [&str; 3] = ["queued", "pending", "waiting"];

/// Determine the active PR for the current git branch, if any.
#[must_use]
pub fn active_pr_for_current_branch(store: &ShipStateStore, cwd: &Path) -> Option<u64> {
    let branch = git_branch(cwd)?;
    store
        .list_active()
        .into_iter()
        .filter(|state| state.branch == branch)
        .max_by_key(|state| state.updated_at)
        .map(|state| state.pr)
}

/// Return a stable signature for the visible watch state.
#[must_use]
pub fn watch_signature(state: &ShipState) -> String {
    let now = Utc::now();
    let stuck_threshold = stuck_queued_threshold_secs();
    let runs = state
        .dispatched_runs
        .iter()
        .map(|run| {
            format!(
                "{}:{}:{}:{}:{}:sq={}",
                run.target,
                run.status,
                run.run_id,
                run.phase.as_deref().unwrap_or(""),
                run.last_heartbeat_at
                    .as_ref()
                    .map(DateTime::<Utc>::to_rfc3339)
                    .unwrap_or_default(),
                if is_stuck_queued(run, now, stuck_threshold) {
                    "1"
                } else {
                    "0"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    let evidence = state
        .evidence_snapshot
        .iter()
        .map(|(target, status)| format!("{target}:{status}"))
        .collect::<Vec<_>>()
        .join(",");

    format!(
        "pr={}|sha={}|attempt={}|evidence={}|runs={}",
        state.pr, state.head_sha, state.attempt, evidence, runs
    )
}

/// Return reused evidence provenance for the current ship state keyed by target.
#[must_use]
pub fn reused_evidence_map(
    evidence_store: &EvidenceStore,
    state: &ShipState,
) -> BTreeMap<String, String> {
    evidence_store
        .get_branch(&state.branch)
        .into_iter()
        .filter_map(|(target, record)| {
            if record.sha == state.head_sha {
                record.reused_from.map(|reused_from| (target, reused_from))
            } else {
                None
            }
        })
        .collect()
}

/// Return the full watch-event signature including reuse provenance.
#[must_use]
pub fn watch_event_signature(state: &ShipState, reuse_map: &BTreeMap<String, String>) -> String {
    let reuse = reuse_map
        .iter()
        .map(|(target, reused_from)| format!("{target}:{}", short_sha(reused_from, 12)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{}|reused={reuse}", watch_signature(state))
}

/// Return `Some(true)` for terminal success, `Some(false)` for terminal failure,
/// or `None` while the ship is still in flight.
#[must_use]
pub fn ship_terminal_verdict(state: &ShipState) -> Option<bool> {
    if state.evidence_snapshot.is_empty() {
        return None;
    }

    if state
        .evidence_snapshot
        .values()
        .any(|status| status != "pass" && status != "fail")
    {
        return None;
    }

    let advisory_targets = state
        .dispatched_runs
        .iter()
        .filter(|run| !run.required)
        .map(|run| run.target.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let required_targets = state
        .dispatched_runs
        .iter()
        .filter(|run| run.required)
        .map(|run| run.target.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    let seen_targets = state
        .evidence_snapshot
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if required_targets.difference(&seen_targets).next().is_some() {
        return None;
    }

    Some(
        state
            .evidence_snapshot
            .iter()
            .all(|(target, status)| status == "pass" || advisory_targets.contains(target.as_str())),
    )
}

/// Emit one watch snapshot, either as a JSON update event or human-readable text.
pub fn emit_watch_snapshot<W: Write>(
    state: &ShipState,
    reuse_map: &BTreeMap<String, String>,
    json: bool,
    stdout: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = Utc::now();
    if json {
        return emit_watch_json_snapshot(state, reuse_map, now, stdout);
    }
    emit_watch_human_snapshot(state, reuse_map, now, stdout)
}

fn emit_watch_json_snapshot<W: Write>(
    state: &ShipState,
    reuse_map: &BTreeMap<String, String>,
    now: DateTime<Utc>,
    stdout: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut data = BTreeMap::new();
    data.insert("event".to_owned(), Value::from("update"));
    data.insert("pr".to_owned(), Value::from(state.pr));
    data.insert("head_sha".to_owned(), Value::from(state.head_sha.clone()));
    data.insert("attempt".to_owned(), Value::from(state.attempt));
    data.insert(
        "evidence".to_owned(),
        Value::Object(json_evidence_snapshot(state, reuse_map)),
    );
    let stuck_threshold = stuck_queued_threshold_secs();
    data.insert(
        "dispatched_runs".to_owned(),
        Value::Array(
            state
                .dispatched_runs
                .iter()
                .map(|run| json_run(run, now, stuck_threshold))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    );
    data.insert(
        "updated_at".to_owned(),
        Value::from(state.updated_at.to_rfc3339()),
    );
    write_json_envelope(stdout, "watch", data)?;
    Ok(())
}

fn json_evidence_snapshot(
    state: &ShipState,
    reuse_map: &BTreeMap<String, String>,
) -> serde_json::Map<String, Value> {
    state
        .evidence_snapshot
        .iter()
        .map(|(target, status)| {
            let value = if status == "pass" {
                reuse_map.get(target).map_or_else(
                    || Value::from(status.clone()),
                    |reused_from| {
                        serde_json::json!({
                            "status": "reused",
                            "reused_from": reused_from,
                        })
                    },
                )
            } else {
                Value::from(status.clone())
            };
            (target.clone(), value)
        })
        .collect()
}

fn emit_watch_human_snapshot<W: Write>(
    state: &ShipState,
    reuse_map: &BTreeMap<String, String>,
    now: DateTime<Utc>,
    stdout: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    let age_secs = now
        .signed_duration_since(state.updated_at)
        .num_seconds()
        .max(0);
    writeln!(
        stdout,
        "PR #{}  sha={}  attempt={}  age={}s",
        state.pr,
        abbreviate_sha(&state.head_sha),
        state.attempt,
        age_secs
    )?;

    let (complete, in_flight, total) = progress_summary(state);
    if total > 0 {
        writeln!(
            stdout,
            "  {complete}/{total} targets complete · {in_flight} in flight"
        )?;
    }

    let advisory_targets = state
        .dispatched_runs
        .iter()
        .filter(|run| !run.required)
        .map(|run| run.target.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    for (target, status) in &state.evidence_snapshot {
        let advisory_tag = if advisory_targets.contains(target.as_str()) {
            " (advisory)"
        } else {
            ""
        };
        if status == "pass"
            && let Some(reused_from) = reuse_map.get(target)
        {
            writeln!(
                stdout,
                "  evidence: {target}=reused (from {}){advisory_tag}",
                short_sha(reused_from, 7)
            )?;
        } else {
            writeln!(stdout, "  evidence: {target}={status}{advisory_tag}")?;
        }
    }

    for run in &state.dispatched_runs {
        let phase = run.phase.as_deref().unwrap_or("-");
        let advisory_tag = if run.required { "" } else { " (advisory)" };
        let stuck_queued = queued_for_secs(run, now)
            .filter(|queued_for_secs| *queued_for_secs >= stuck_queued_threshold_secs())
            .map(|queued_for_secs| {
                format!(
                    " stuck-queued {}",
                    format_stuck_queued_duration(queued_for_secs)
                )
            })
            .unwrap_or_default();
        writeln!(
            stdout,
            "  run: {} ({}) id={} status={} phase={} elapsed={}s last_seen={}{}{}",
            run.target,
            run.provider,
            run.run_id,
            run.status,
            phase,
            run_elapsed_secs(run, now),
            heartbeat_age_label(run, now),
            stuck_queued,
            advisory_tag
        )?;
    }

    Ok(())
}

/// Emit a non-update watch event such as `pr-not-found`.
pub fn emit_watch_event<W: Write>(
    event: &str,
    pr: Option<u64>,
    message: Option<&str>,
    json: bool,
    stdout: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        let mut data = BTreeMap::new();
        data.insert("event".to_owned(), Value::from(event));
        if let Some(pr) = pr {
            data.insert("pr".to_owned(), Value::from(pr));
        }
        if let Some(message) = message {
            data.insert("message".to_owned(), Value::from(message));
        }
        write_json_envelope(stdout, "watch", data)?;
        return Ok(());
    }

    if let Some(message) = message {
        writeln!(stdout, "{message}")?;
    }
    Ok(())
}

fn git_branch(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!branch.is_empty()).then_some(branch)
}

fn json_run(
    run: &DispatchedRun,
    now: DateTime<Utc>,
    stuck_threshold_secs: i64,
) -> Result<Value, serde_json::Error> {
    let mut value = serde_json::to_value(run)?;
    let Value::Object(ref mut map) = value else {
        unreachable!("DispatchedRun must serialize as an object");
    };
    map.insert(
        "elapsed_seconds".to_owned(),
        Value::from(run_elapsed_secs(run, now)),
    );
    map.insert(
        "queued_for_secs".to_owned(),
        queued_for_secs(run, now).map_or(Value::Null, Value::from),
    );
    map.insert(
        "stuck_queued".to_owned(),
        Value::from(is_stuck_queued(run, now, stuck_threshold_secs)),
    );
    Ok(value)
}

fn run_elapsed_secs(run: &DispatchedRun, now: DateTime<Utc>) -> i64 {
    now.signed_duration_since(run.started_at)
        .num_seconds()
        .max(0)
}

fn stuck_queued_threshold_secs() -> i64 {
    std::env::var("SHIPYARD_STUCK_QUEUED_THRESHOLD_SECS")
        .ok()
        .and_then(|raw| raw.parse::<i64>().ok())
        .map_or(300, |threshold| threshold.max(0))
}

fn queued_for_secs(run: &DispatchedRun, now: DateTime<Utc>) -> Option<i64> {
    QUEUED_STATUSES
        .contains(&run.status.to_lowercase().as_str())
        .then(|| run_elapsed_secs(run, now))
}

fn is_stuck_queued(run: &DispatchedRun, now: DateTime<Utc>, threshold_secs: i64) -> bool {
    queued_for_secs(run, now).is_some_and(|queued_for_secs| queued_for_secs >= threshold_secs)
}

fn format_stuck_queued_duration(secs: i64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3_600 {
        return format!("{}m", secs / 60);
    }
    let hours = secs / 3_600;
    let minutes = (secs % 3_600) / 60;
    format!("{hours}h{minutes}m")
}

fn heartbeat_age_label(run: &DispatchedRun, now: DateTime<Utc>) -> String {
    let terminal = matches!(
        run.status.as_str(),
        "pass"
            | "success"
            | "completed"
            | "completed_success"
            | "fail"
            | "failed"
            | "failure"
            | "completed_failure"
            | "cancelled"
            | "canceled"
    );
    if terminal {
        return "-".to_owned();
    }
    let Some(last_heartbeat_at) = run.last_heartbeat_at else {
        return "-".to_owned();
    };
    let age = now
        .signed_duration_since(last_heartbeat_at)
        .num_seconds()
        .max(0);
    if age < 60 {
        format!("{age}s_ago")
    } else {
        let minutes = age / 60;
        let seconds = age % 60;
        format!("{minutes}m{seconds:02}s_ago")
    }
}

fn progress_summary(state: &ShipState) -> (usize, usize, usize) {
    let terminal_statuses = [
        "pass",
        "success",
        "completed",
        "completed_success",
        "fail",
        "failed",
        "failure",
        "completed_failure",
        "cancelled",
        "canceled",
    ];

    let complete = state
        .dispatched_runs
        .iter()
        .filter(|run| terminal_statuses.contains(&run.status.as_str()))
        .count();
    let total = state.dispatched_runs.len();
    let in_flight = total.saturating_sub(complete);
    (complete, in_flight, total)
}

fn abbreviate_sha(sha: &str) -> &str {
    short_sha(sha, 12)
}

fn short_sha(sha: &str, width: usize) -> &str {
    let max = sha
        .char_indices()
        .nth(width)
        .map_or_else(|| sha.len(), |(index, _)| index);
    &sha[..max]
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::{
        emit_watch_snapshot, format_stuck_queued_duration, is_stuck_queued, queued_for_secs,
        reused_evidence_map, ship_terminal_verdict, watch_event_signature, watch_signature,
    };
    use crate::evidence::{EvidenceRecord, EvidenceStore};
    use crate::ship_state::{DispatchedRun, ShipState};

    fn state(pr: u64, evidence: &[(&str, &str)], runs: Vec<DispatchedRun>) -> ShipState {
        let mut state = ShipState::new(pr, "owner/repo", "feature/x", "main", "a".repeat(40), "p1");
        state.evidence_snapshot = evidence
            .iter()
            .map(|(target, status)| ((*target).to_owned(), (*status).to_owned()))
            .collect();
        state.dispatched_runs = runs;
        state
    }

    fn run(target: &str, status: &str, run_id: &str, required: bool) -> DispatchedRun {
        let now = Utc::now();
        DispatchedRun {
            target: target.to_owned(),
            provider: "local".to_owned(),
            run_id: run_id.to_owned(),
            status: status.to_owned(),
            started_at: now,
            updated_at: now,
            attempt: 1,
            last_heartbeat_at: None,
            phase: None,
            required,
        }
    }

    fn run_with_age(target: &str, status: &str, age_secs: i64) -> DispatchedRun {
        let mut run = run(target, status, "1", true);
        let started_at = Utc::now() - Duration::seconds(age_secs);
        run.started_at = started_at;
        run.updated_at = started_at;
        run
    }

    #[test]
    fn watch_signature_ignores_updated_at_only_changes() {
        let mut state = state(42, &[("macos", "pass")], vec![]);
        let before = watch_signature(&state);
        state.updated_at += Duration::seconds(5);
        let after = watch_signature(&state);
        assert_eq!(before, after);
    }

    #[test]
    fn watch_event_signature_changes_when_reuse_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let evidence = EvidenceStore::new(temp.path().join("evidence")).expect("store");
        let state = state(
            42,
            &[("macos", "pass")],
            vec![run("macos", "completed", "1", true)],
        );
        let base = watch_event_signature(&state, &reused_evidence_map(&evidence, &state));

        evidence
            .record(&EvidenceRecord {
                sha: state.head_sha.clone(),
                branch: state.branch.clone(),
                target_name: "macos".to_owned(),
                platform: "macos-arm64".to_owned(),
                status: "pass".to_owned(),
                backend: "reused".to_owned(),
                completed_at: Utc::now(),
                duration_secs: None,
                host: None,
                primary_backend: None,
                failover_reason: None,
                provider: None,
                runner_profile: None,
                failure_class: None,
                reused_from: Some("b".repeat(40)),
                contract_digest: None,
                stages_signature: None,
            })
            .expect("record");

        let updated = watch_event_signature(&state, &reused_evidence_map(&evidence, &state));
        assert_ne!(base, updated);
        assert!(updated.contains("reused=macos:bbbbbbbbbbbb"));
    }

    #[test]
    fn queued_for_secs_only_applies_to_queued_family_statuses() {
        let now = Utc::now();
        for status in ["queued", "pending", "waiting"] {
            assert!(queued_for_secs(&run_with_age("macos", status, 120), now).is_some());
        }
        for status in ["in_progress", "completed", "failed"] {
            assert!(queued_for_secs(&run_with_age("macos", status, 120), now).is_none());
        }
    }

    #[test]
    fn stuck_queued_uses_threshold_and_human_duration_shape() {
        let now = Utc::now();
        let run = run_with_age("macos", "queued", 400);

        assert!(is_stuck_queued(&run, now, 300));
        assert!(!is_stuck_queued(&run, now, 600));
        assert_eq!(format_stuck_queued_duration(45), "45s");
        assert_eq!(format_stuck_queued_duration(125), "2m");
        assert_eq!(format_stuck_queued_duration(3_900), "1h5m");
    }

    #[test]
    fn watch_signature_flips_when_run_crosses_default_stuck_threshold() {
        let before = state(42, &[], vec![run_with_age("macos", "queued", 100)]);
        let after = state(42, &[], vec![run_with_age("macos", "queued", 400)]);

        let sig_before = watch_signature(&before);
        let sig_after = watch_signature(&after);

        assert_ne!(sig_before, sig_after);
        assert!(sig_before.contains("sq=0"));
        assert!(sig_after.contains("sq=1"));
    }

    #[test]
    fn watch_json_includes_stuck_queued_fields() {
        let state = state(42, &[], vec![run_with_age("macos", "queued", 400)]);
        let mut stdout = Vec::new();

        emit_watch_snapshot(
            &state,
            &std::collections::BTreeMap::new(),
            true,
            &mut stdout,
        )
        .expect("watch snapshot");
        let value: serde_json::Value = serde_json::from_slice(&stdout).expect("json");
        let run = &value["dispatched_runs"][0];

        assert!(run["queued_for_secs"].as_i64().expect("queued secs") >= 300);
        assert_eq!(run["stuck_queued"], serde_json::Value::Bool(true));
    }

    #[test]
    fn verdict_returns_success_when_all_required_targets_pass() {
        let state = state(
            42,
            &[("macos", "pass"), ("linux", "pass")],
            vec![
                run("macos", "completed", "1", true),
                run("linux", "completed", "2", true),
            ],
        );
        assert_eq!(ship_terminal_verdict(&state), Some(true));
    }

    #[test]
    fn verdict_returns_failure_when_required_target_fails() {
        let state = state(
            42,
            &[("macos", "fail")],
            vec![run("macos", "failed", "1", true)],
        );
        assert_eq!(ship_terminal_verdict(&state), Some(false));
    }

    #[test]
    fn verdict_ignores_advisory_failures() {
        let state = state(
            42,
            &[("mac", "pass"), ("windows", "fail")],
            vec![
                run("mac", "completed", "1", true),
                run("windows", "failed", "2", false),
            ],
        );
        assert_eq!(ship_terminal_verdict(&state), Some(true));
    }

    #[test]
    fn verdict_requires_terminal_evidence_for_all_required_targets() {
        let state = state(
            42,
            &[("mac", "pass")],
            vec![
                run("mac", "completed", "1", true),
                run("linux", "completed", "2", true),
            ],
        );
        assert_eq!(ship_terminal_verdict(&state), None);
    }
}
