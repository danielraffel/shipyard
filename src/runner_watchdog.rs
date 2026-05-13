//! Self-hosted runner watchdog: pure detection logic shared by the CLI handler.
//!
//! Ports the Pulp planning prototype at
//! `pulp-planning/scripts/runner-watchdog.sh` (commit c719482) into typed,
//! shell-free Rust helpers. The CLI side (`src/app/runner_cmd.rs`) is the only
//! place that shells out to `gh` and inspects the local process table; this
//! module just classifies inputs and produces report shapes.
//!
//! ## Symptoms tracked
//!
//! 1. **Runner reachability** — runner ID returns a payload with `status =
//!    "online"`. Otherwise `OFFLINE`.
//! 2. **Orphaned busy state** — runner API reports `busy = true` but no
//!    `Runner.Worker` process owned by this runner is visible in `ps`. The
//!    status commonly clears in 1-5 min; auto-fix is opt-in.
//! 3. **Hung Worker** — a `Runner.Worker` process has been running longer than
//!    `max_job_min` minutes.
//! 4. **Stale queued runs** — queued GitHub Actions runs older than
//!    `max_queue_age_hours`. With `--fix` the CLI cancels them.
//!
//! ## Why no auto-actions by default
//!
//! Killing a Worker process can corrupt in-flight artifacts; cancelling a
//! queued run can lose CI evidence the user is waiting on. `cleanup --fix`
//! and `--force-kill` are explicit opt-ins, matching the prototype's
//! convention.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::cloud::QueuedRun;

/// Defaults for the watchdog, mirroring `[runner.watchdog]` in
/// `.shipyard/config.toml`.
pub const DEFAULT_MAX_JOB_MIN: i64 = 90;
/// Default stale-queue age before a queued run is flagged.
pub const DEFAULT_MAX_QUEUE_AGE_HOURS: i64 = 2;
/// Default watch-mode polling interval in seconds.
pub const DEFAULT_WATCH_INTERVAL_SECONDS: u64 = 300;

/// Resolved runner-watchdog thresholds for a single invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchdogThresholds {
    /// Warn when a `Runner.Worker` has been running longer than this many
    /// minutes.
    pub max_job_min: i64,
    /// Flag any queued workflow run created more than this many hours ago.
    pub max_queue_age_hours: i64,
    /// Polling interval for `runner watch`.
    pub watch_interval_seconds: u64,
    /// Whether `--fix` defaults to on.
    pub auto_fix: bool,
}

impl Default for WatchdogThresholds {
    fn default() -> Self {
        Self {
            max_job_min: DEFAULT_MAX_JOB_MIN,
            max_queue_age_hours: DEFAULT_MAX_QUEUE_AGE_HOURS,
            watch_interval_seconds: DEFAULT_WATCH_INTERVAL_SECONDS,
            auto_fix: false,
        }
    }
}

/// Overall runner-health classification.
///
/// Maps directly to the CLI exit codes: 0 healthy, 1 stuck, 2 unreachable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerHealth {
    /// Runner is online, no symptoms detected.
    Healthy,
    /// At least one symptom (orphaned busy / hung worker / stale queue)
    /// detected; runner is still reachable.
    Stuck,
    /// Runner is offline or the API call failed.
    Offline,
}

impl RunnerHealth {
    /// Exit code that should accompany this health verdict.
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Healthy => 0,
            Self::Stuck => 1,
            Self::Offline => 2,
        }
    }

    /// Snake-case string form used in JSON and human output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Stuck => "stuck",
            Self::Offline => "offline",
        }
    }
}

/// One symptom found during a watchdog scan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Symptom {
    /// API reports busy but no `Runner.Worker` process is visible.
    OrphanedBusy,
    /// A `Runner.Worker` has been running longer than `max_job_min` minutes.
    HungWorker {
        /// Observed runtime in whole minutes.
        worker_age_min: i64,
        /// Threshold that was crossed.
        threshold_min: i64,
    },
    /// One or more queued runs are older than `max_queue_age_hours`.
    StaleQueuedRuns {
        /// Number of stale runs.
        count: usize,
    },
}

impl Symptom {
    /// Short tag used in human output.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            Self::OrphanedBusy => "orphaned_busy",
            Self::HungWorker { .. } => "hung_worker",
            Self::StaleQueuedRuns { .. } => "stale_queued_runs",
        }
    }
}

/// Single stale queued-run row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StaleQueuedRun {
    /// GitHub Actions run ID.
    pub run_id: u64,
    /// Workflow display name.
    pub workflow: String,
    /// Head branch.
    pub branch: String,
    /// How long the run has been queued, in seconds.
    pub queued_for_secs: i64,
    /// Browser URL, when GitHub returned one.
    pub url: Option<String>,
}

/// Summary returned by [`assess_runner`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RunnerReport {
    /// Final verdict for exit code.
    pub health: RunnerHealth,
    /// Concrete symptoms observed.
    pub symptoms: Vec<Symptom>,
    /// Stale queued runs (always present, even when not the reason for STUCK).
    pub stale_queued_runs: Vec<StaleQueuedRun>,
    /// Number of `Runner.Worker` processes observed locally.
    pub worker_count: usize,
    /// Whether the API reports the runner as busy.
    pub busy: bool,
    /// Reported runner status string (e.g. "online", "offline").
    pub status: String,
}

impl RunnerReport {
    /// Convenience: did the scan find anything actionable?
    #[must_use]
    pub fn is_clean(&self) -> bool {
        matches!(self.health, RunnerHealth::Healthy)
    }
}

/// Snapshot of the runner-side state, gathered by the caller (CLI shells out
/// to `gh` and `ps`). Kept as a plain struct so the pure analysis stays
/// trivially testable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunnerSnapshot {
    /// `status` field from `repos/<owner>/<repo>/actions/runners/<id>`.
    pub status: String,
    /// `busy` field from the same API.
    pub busy: bool,
    /// Number of locally-visible `Runner.Worker` processes.
    pub worker_count: usize,
    /// Age of the oldest `Runner.Worker`, in minutes.
    pub oldest_worker_age_min: Option<i64>,
}

/// Build a [`RunnerReport`] from a runner snapshot and a list of queued runs.
///
/// `now` is taken as a parameter so tests can pin the clock without touching
/// the system time.
#[must_use]
pub fn assess_runner(
    snapshot: &RunnerSnapshot,
    queued_runs: &[QueuedRun],
    thresholds: WatchdogThresholds,
    now: DateTime<Utc>,
) -> RunnerReport {
    if snapshot.status != "online" {
        return RunnerReport {
            health: RunnerHealth::Offline,
            symptoms: Vec::new(),
            stale_queued_runs: Vec::new(),
            worker_count: snapshot.worker_count,
            busy: snapshot.busy,
            status: snapshot.status.clone(),
        };
    }

    let mut symptoms = Vec::new();

    if snapshot.busy && snapshot.worker_count == 0 {
        symptoms.push(Symptom::OrphanedBusy);
    }

    if let Some(age) = snapshot.oldest_worker_age_min
        && age > thresholds.max_job_min
    {
        symptoms.push(Symptom::HungWorker {
            worker_age_min: age,
            threshold_min: thresholds.max_job_min,
        });
    }

    let stale_queued_runs =
        compute_stale_queued_runs(queued_runs, thresholds.max_queue_age_hours * 3_600, now);
    if !stale_queued_runs.is_empty() {
        symptoms.push(Symptom::StaleQueuedRuns {
            count: stale_queued_runs.len(),
        });
    }

    let health = if symptoms.is_empty() {
        RunnerHealth::Healthy
    } else {
        RunnerHealth::Stuck
    };

    RunnerReport {
        health,
        symptoms,
        stale_queued_runs,
        worker_count: snapshot.worker_count,
        busy: snapshot.busy,
        status: snapshot.status.clone(),
    }
}

/// Filter `queued_runs` down to entries older than `threshold_secs` relative
/// to `now`. Runs with unparseable timestamps are ignored, matching the
/// prototype's silent-skip behaviour.
#[must_use]
pub fn compute_stale_queued_runs(
    queued_runs: &[QueuedRun],
    threshold_secs: i64,
    now: DateTime<Utc>,
) -> Vec<StaleQueuedRun> {
    let mut out = Vec::new();
    for run in queued_runs {
        let Ok(created) = DateTime::parse_from_rfc3339(&run.created_at) else {
            continue;
        };
        let age_secs = (now - created.with_timezone(&Utc)).num_seconds();
        if age_secs < threshold_secs {
            continue;
        }
        out.push(StaleQueuedRun {
            run_id: run.database_id,
            workflow: run.workflow_name.clone(),
            branch: run.head_branch.clone(),
            queued_for_secs: age_secs,
            url: run.url.clone(),
        });
    }
    out
}

/// Render a [`RunnerReport`] as a flat `BTreeMap<String, Value>` ready to be
/// dropped into `write_json_envelope`.
#[must_use]
pub fn report_to_json(report: &RunnerReport) -> BTreeMap<String, Value> {
    let mut data = BTreeMap::new();
    data.insert("health".to_owned(), Value::from(report.health.as_str()));
    data.insert("status".to_owned(), Value::from(report.status.clone()));
    data.insert("busy".to_owned(), Value::Bool(report.busy));
    data.insert("worker_count".to_owned(), Value::from(report.worker_count));
    data.insert(
        "symptoms".to_owned(),
        serde_json::to_value(&report.symptoms).expect("symptom serialization"),
    );
    data.insert(
        "stale_queued_runs".to_owned(),
        serde_json::to_value(&report.stale_queued_runs).expect("stale run serialization"),
    );
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn queued_run(id: u64, workflow: &str, branch: &str, created_at: &str) -> QueuedRun {
        QueuedRun {
            database_id: id,
            name: workflow.to_owned(),
            head_branch: branch.to_owned(),
            created_at: created_at.to_owned(),
            workflow_name: workflow.to_owned(),
            url: Some(format!("https://github.com/owner/repo/actions/runs/{id}")),
            path: ".github/workflows/ci.yml".to_owned(),
        }
    }

    #[test]
    fn offline_runner_short_circuits_to_offline() {
        let snapshot = RunnerSnapshot {
            status: "offline".to_owned(),
            busy: false,
            worker_count: 0,
            oldest_worker_age_min: None,
        };
        let report = assess_runner(&snapshot, &[], WatchdogThresholds::default(), Utc::now());
        assert_eq!(report.health, RunnerHealth::Offline);
        assert_eq!(report.health.exit_code(), 2);
        assert!(report.symptoms.is_empty());
    }

    #[test]
    fn healthy_runner_has_no_symptoms() {
        let snapshot = RunnerSnapshot {
            status: "online".to_owned(),
            busy: false,
            worker_count: 0,
            oldest_worker_age_min: None,
        };
        let report = assess_runner(&snapshot, &[], WatchdogThresholds::default(), Utc::now());
        assert_eq!(report.health, RunnerHealth::Healthy);
        assert_eq!(report.health.exit_code(), 0);
    }

    #[test]
    fn busy_with_no_worker_flags_orphaned_busy() {
        let snapshot = RunnerSnapshot {
            status: "online".to_owned(),
            busy: true,
            worker_count: 0,
            oldest_worker_age_min: None,
        };
        let report = assess_runner(&snapshot, &[], WatchdogThresholds::default(), Utc::now());
        assert_eq!(report.health, RunnerHealth::Stuck);
        assert!(matches!(report.symptoms[0], Symptom::OrphanedBusy));
        assert_eq!(report.health.exit_code(), 1);
    }

    #[test]
    fn worker_running_longer_than_threshold_is_hung() {
        let snapshot = RunnerSnapshot {
            status: "online".to_owned(),
            busy: true,
            worker_count: 1,
            oldest_worker_age_min: Some(120),
        };
        let report = assess_runner(&snapshot, &[], WatchdogThresholds::default(), Utc::now());
        assert!(report.symptoms.iter().any(|s| matches!(
            s,
            Symptom::HungWorker {
                worker_age_min: 120,
                threshold_min: 90
            }
        )));
    }

    #[test]
    fn worker_at_threshold_is_not_flagged() {
        let snapshot = RunnerSnapshot {
            status: "online".to_owned(),
            busy: true,
            worker_count: 1,
            oldest_worker_age_min: Some(90),
        };
        let report = assess_runner(&snapshot, &[], WatchdogThresholds::default(), Utc::now());
        assert!(
            report
                .symptoms
                .iter()
                .all(|s| !matches!(s, Symptom::HungWorker { .. }))
        );
    }

    #[test]
    fn stale_queue_filter_uses_age_threshold() {
        let now = Utc::now();
        let runs = vec![
            queued_run(
                101,
                "CI",
                "agentB/81-window-only-capture",
                &(now - ChronoDuration::hours(5)).to_rfc3339(),
            ),
            queued_run(
                102,
                "CI",
                "feature/recent",
                &(now - ChronoDuration::minutes(15)).to_rfc3339(),
            ),
            queued_run(103, "CI", "feature/bad-ts", "not-a-timestamp"),
        ];

        let stale = compute_stale_queued_runs(&runs, 2 * 3_600, now);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].run_id, 101);
        assert!(stale[0].queued_for_secs >= 5 * 3_600);
        assert_eq!(stale[0].branch, "agentB/81-window-only-capture");
    }

    #[test]
    fn stale_queue_drives_stuck_verdict() {
        let now = Utc::now();
        let snapshot = RunnerSnapshot {
            status: "online".to_owned(),
            busy: false,
            worker_count: 0,
            oldest_worker_age_min: None,
        };
        let runs = vec![queued_run(
            101,
            "CI",
            "main",
            &(now - ChronoDuration::hours(3)).to_rfc3339(),
        )];
        let report = assess_runner(&snapshot, &runs, WatchdogThresholds::default(), now);
        assert_eq!(report.health, RunnerHealth::Stuck);
        assert!(matches!(
            report.symptoms[0],
            Symptom::StaleQueuedRuns { count: 1 }
        ));
        assert_eq!(report.stale_queued_runs.len(), 1);
    }

    #[test]
    fn report_to_json_includes_top_level_fields() {
        let now = Utc::now();
        let snapshot = RunnerSnapshot {
            status: "online".to_owned(),
            busy: true,
            worker_count: 0,
            oldest_worker_age_min: None,
        };
        let report = assess_runner(&snapshot, &[], WatchdogThresholds::default(), now);
        let data = report_to_json(&report);
        assert_eq!(data["health"], Value::from("stuck"));
        assert_eq!(data["status"], Value::from("online"));
        assert_eq!(data["busy"], Value::Bool(true));
        assert_eq!(data["worker_count"], Value::from(0));
    }
}
