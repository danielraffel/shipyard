//! Ship execution orchestration helpers.
//!
//! The full `ship` command eventually ties together dispatch, queue,
//! evidence, ship-state, and merge behavior. This module starts with
//! the warm-pool and durable execution logic so executor wiring can
//! reuse it without embedding policy decisions in CLI code.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::evidence::{EvidenceRecord, EvidenceStore};
use crate::executor::dispatch::{DispatchValidationRequest, ExecutorDispatcher, ResolvedTarget};
use crate::executor::streaming::ProgressEvent;
use crate::job::{Job, JobTransitionError, Priority, TargetResult, TargetStatus, ValidationMode};
use crate::queue::{Queue, QueueError};
use crate::ship_state::{DispatchedRun, ShipState, ShipStateStore, compute_policy_signature};
use crate::warm_pool::{
    PoolEntry, WarmPool, compute_expires_at, is_backend_eligible, warm_host_key,
};

const RESUME_ORDER: [&str; 4] = ["setup", "configure", "build", "test"];
const WARM_DEFAULT_RESUME_FROM: &str = "configure";
const DEFAULT_WORKDIR: &str = "~/repo";

/// Resolved inputs for one `ship` execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipExecutionRequest {
    /// Pull request number.
    pub pr: u64,
    /// Repository slug.
    pub repo: String,
    /// Head branch.
    pub branch: String,
    /// Base branch.
    pub base_branch: String,
    /// Head SHA.
    pub sha: String,
    /// Optional commit subject.
    pub commit_subject: String,
    /// Optional PR URL resolved from GitHub.
    pub pr_url: Option<String>,
    /// Optional PR title resolved from GitHub.
    pub pr_title: Option<String>,
    /// Validation mode.
    pub mode: ValidationMode,
    /// Queue priority.
    pub priority: Priority,
    /// Whether warm-pool reuse is disabled for this run.
    pub warm_disabled: bool,
    /// Whether remaining targets should be skipped after the first failure.
    pub fail_fast: bool,
    /// Optional explicit resume stage.
    pub resume_from: Option<String>,
    /// Target names whose failures should not block merge.
    pub advisory_targets: BTreeSet<String>,
    /// Ordered target list.
    pub targets: Vec<ResolvedTarget>,
}

/// Resolved inputs for one `shipyard run` execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunExecutionRequest {
    /// Branch under validation.
    pub branch: String,
    /// Head SHA.
    pub sha: String,
    /// Validation mode.
    pub mode: ValidationMode,
    /// Scheduling priority.
    pub priority: Priority,
    /// Whether warm-pool reuse is disabled for this run.
    pub warm_disabled: bool,
    /// Whether remaining targets should be skipped after the first failure.
    pub fail_fast: bool,
    /// Optional explicit resume stage.
    pub resume_from: Option<String>,
    /// Ordered target list.
    pub targets: Vec<ResolvedTarget>,
}

/// Durable stores needed by ship execution.
pub struct ShipStores<'a> {
    /// Job queue store.
    pub queue: &'a mut Queue,
    /// Evidence store.
    pub evidence: &'a EvidenceStore,
    /// Ship-state store.
    pub ship_state: &'a ShipStateStore,
    /// Warm-pool store.
    pub warm_pool: &'a WarmPool,
    /// State directory used for target logs.
    pub state_dir: &'a Path,
}

/// Durable stores needed by `shipyard run` execution.
pub struct RunStores<'a> {
    /// Job queue store.
    pub queue: &'a mut Queue,
    /// Evidence store.
    pub evidence: &'a EvidenceStore,
    /// Warm-pool store.
    pub warm_pool: &'a WarmPool,
    /// State directory used for target logs.
    pub state_dir: &'a Path,
}

/// Outcome of one ship execution pass.
#[derive(Clone, Debug, PartialEq)]
pub struct ShipExecutionOutcome {
    /// Final job.
    pub job: Job,
    /// Final active ship state.
    pub ship_state: ShipState,
    /// Whether an existing compatible state was reused.
    pub resumed_existing_state: bool,
}

/// Outcome of one `shipyard run` execution.
#[derive(Clone, Debug, PartialEq)]
pub struct RunExecutionOutcome {
    /// Final job.
    pub job: Job,
}

/// Errors from ship execution orchestration.
#[derive(Debug)]
pub enum ShipExecutionError {
    /// Existing state belongs to a different SHA.
    ShaDrift {
        /// State SHA.
        existing: String,
        /// Current SHA.
        current: String,
    },
    /// Existing state was created under a different target/policy set.
    PolicyDrift {
        /// State policy signature.
        existing: String,
        /// Current policy signature.
        current: String,
    },
    /// Job transition failed.
    JobTransition(JobTransitionError),
    /// Queue persistence failed.
    Queue(QueueError),
    /// Evidence persistence failed.
    Evidence(String),
    /// Ship-state persistence failed.
    ShipState(String),
    /// Warm-pool persistence failed.
    WarmPool(std::io::Error),
}

impl Display for ShipExecutionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShaDrift { existing, current } => {
                write!(
                    formatter,
                    "ship state SHA drift: existing {existing}, current {current}"
                )
            }
            Self::PolicyDrift { existing, current } => write!(
                formatter,
                "ship state policy drift: existing {existing}, current {current}"
            ),
            Self::JobTransition(error) => write!(formatter, "{error}"),
            Self::Queue(error) => write!(formatter, "{error}"),
            Self::Evidence(error) => write!(formatter, "evidence write failed: {error}"),
            Self::ShipState(error) => write!(formatter, "ship-state write failed: {error}"),
            Self::WarmPool(error) => write!(formatter, "warm-pool write failed: {error}"),
        }
    }
}

impl Error for ShipExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::JobTransition(error) => Some(error),
            Self::Queue(error) => Some(error),
            Self::WarmPool(error) => Some(error),
            Self::ShaDrift { .. }
            | Self::PolicyDrift { .. }
            | Self::Evidence(_)
            | Self::ShipState(_) => None,
        }
    }
}

impl From<JobTransitionError> for ShipExecutionError {
    fn from(error: JobTransitionError) -> Self {
        Self::JobTransition(error)
    }
}

impl From<QueueError> for ShipExecutionError {
    fn from(error: QueueError) -> Self {
        Self::Queue(error)
    }
}

/// Validation backend boundary used by ship orchestration.
pub trait ShipTargetDispatcher {
    /// Validate one resolved target.
    fn validate(&self, request: DispatchValidationRequest<'_, '_>) -> TargetResult;
}

impl ShipTargetDispatcher for ExecutorDispatcher {
    fn validate(&self, request: DispatchValidationRequest<'_, '_>) -> TargetResult {
        ExecutorDispatcher::validate(self, request)
    }
}

/// Execute all targets for a ship request and persist terminal state.
pub fn execute_ship<D: ShipTargetDispatcher>(
    request: &ShipExecutionRequest,
    stores: ShipStores<'_>,
    dispatcher: &D,
) -> Result<ShipExecutionOutcome, ShipExecutionError> {
    let ShipStores {
        queue,
        evidence,
        ship_state,
        warm_pool,
        state_dir,
    } = stores;
    let target_names = target_names(&request.targets);
    let resumed_existing_state = ship_state.get(request.pr).is_some();
    let mut state = load_or_create_state(request, &target_names, ship_state)?;
    ship_state
        .save(&state)
        .map_err(|error| ShipExecutionError::ShipState(error.to_string()))?;

    let mut job = Job::create(
        request.sha.clone(),
        request.branch.clone(),
        target_names,
        request.mode,
        request.priority,
    );
    queue.enqueue(job.clone())?;
    job = job.start()?;
    queue.update(&job)?;

    job = execute_targets(request, state_dir, queue, warm_pool, dispatcher, job)?;
    job = job.complete()?;
    queue.update(&job)?;
    record_evidence(evidence, request, &job)?;
    update_ship_state_from_job(&mut state, request, &job);
    ship_state
        .save(&state)
        .map_err(|error| ShipExecutionError::ShipState(error.to_string()))?;

    Ok(ShipExecutionOutcome {
        job,
        ship_state: state,
        resumed_existing_state,
    })
}

/// Execute configured targets for `shipyard run` without PR/ship-state mutation.
pub fn execute_run<D: ShipTargetDispatcher>(
    request: &RunExecutionRequest,
    stores: RunStores<'_>,
    dispatcher: &D,
) -> Result<RunExecutionOutcome, ShipExecutionError> {
    let RunStores {
        queue,
        evidence,
        warm_pool,
        state_dir,
    } = stores;
    let target_names = target_names(&request.targets);
    let shim = ShipExecutionRequest {
        pr: 0,
        repo: String::new(),
        branch: request.branch.clone(),
        base_branch: String::new(),
        sha: request.sha.clone(),
        commit_subject: String::new(),
        pr_url: None,
        pr_title: None,
        mode: request.mode,
        priority: request.priority,
        warm_disabled: request.warm_disabled,
        fail_fast: request.fail_fast,
        resume_from: request.resume_from.clone(),
        advisory_targets: BTreeSet::new(),
        targets: request.targets.clone(),
    };
    let mut job = Job::create(
        request.sha.clone(),
        request.branch.clone(),
        target_names,
        request.mode,
        request.priority,
    );
    queue.enqueue(job.clone())?;
    job = job.start()?;
    queue.update(&job)?;
    job = execute_targets(&shim, state_dir, queue, warm_pool, dispatcher, job)?;
    job = job.complete()?;
    queue.update(&job)?;
    record_evidence(evidence, &shim, &job)?;
    Ok(RunExecutionOutcome { job })
}

fn execute_targets<D: ShipTargetDispatcher>(
    request: &ShipExecutionRequest,
    state_dir: &Path,
    queue: &mut Queue,
    warm_pool: &WarmPool,
    dispatcher: &D,
    mut job: Job,
) -> Result<Job, ShipExecutionError> {
    let mut had_failure = false;
    for target in &request.targets {
        if had_failure && request.fail_fast {
            job = job.with_result(cancelled_result(target, job.started_at));
            queue.update(&job)?;
            continue;
        }
        let log_path = target_log_path(state_dir, &job.id, &target.name);
        let decision = apply_warm_reuse(
            warm_pool,
            target,
            &request.sha,
            request.resume_from.as_deref(),
            request.warm_disabled,
            crate::warm_pool::now_epoch_secs(),
        );
        job = job.with_result(running_result(&decision.target, &log_path, job.started_at));
        queue.update(&job)?;

        let progress_log_path = log_path.clone();
        let mut progress_error = None;
        let result = {
            let mut progress_callback = |event: ProgressEvent| {
                if progress_error.is_some() {
                    return;
                }
                apply_progress_event(&mut job, &decision.target, &progress_log_path, event);
                if let Err(error) = queue.update(&job) {
                    progress_error = Some(error);
                }
            };
            dispatcher.validate(DispatchValidationRequest {
                sha: request.sha.clone(),
                branch: request.branch.clone(),
                target: &decision.target,
                log_path,
                resume_from: decision.resume_from.clone(),
                mode: request.mode,
                progress_callback: Some(&mut progress_callback),
            })
        };
        if let Some(error) = progress_error {
            return Err(ShipExecutionError::Queue(error));
        }
        job = job.with_result(result.clone());
        queue.update(&job)?;
        if !result.passed() {
            had_failure = true;
        }
        update_warm_pool_after_run(
            warm_pool,
            &decision.target,
            &request.sha,
            &result,
            decision.warm_hit,
            request.warm_disabled,
            crate::warm_pool::now_epoch_secs(),
        )
        .map_err(ShipExecutionError::WarmPool)?;
    }
    Ok(job)
}

fn apply_progress_event(
    job: &mut Job,
    target: &ResolvedTarget,
    log_path: &Path,
    event: ProgressEvent,
) {
    let mut current = job
        .results
        .get(&target.name)
        .cloned()
        .unwrap_or_else(|| running_result(target, log_path, job.started_at));
    current.status = TargetStatus::Running;
    if let Some(phase) = event.phase {
        current.phase = Some(phase);
    }
    if let Some(last_output_at) = event.last_output_at {
        current.last_output_at = Some(last_output_at);
    }
    current.last_heartbeat_at = Some(event.last_heartbeat_at);
    current.quiet_for_secs = Some(event.quiet_for_secs);
    current.liveness = Some(event.liveness);
    current.log_path = current
        .log_path
        .or_else(|| Some(log_path.to_string_lossy().into_owned()));
    *job = job.with_result(current);
}

fn load_or_create_state(
    request: &ShipExecutionRequest,
    target_names: &[String],
    store: &ShipStateStore,
) -> Result<ShipState, ShipExecutionError> {
    let policy = policy_signature(&request.targets, target_names, request.mode);
    if let Some(mut existing) = store.get(request.pr) {
        validate_existing_state(&existing, &request.sha, &policy)?;
        existing.commit_subject.clone_from(&request.commit_subject);
        refresh_pr_metadata(&mut existing, request);
        existing.touch();
        return Ok(existing);
    }

    let mut state = ShipState::new(
        request.pr,
        request.repo.clone(),
        request.branch.clone(),
        request.base_branch.clone(),
        request.sha.clone(),
        policy,
    );
    refresh_pr_metadata(&mut state, request);
    if state.pr_url.is_empty() && !request.repo.is_empty() {
        state.pr_url = format!("https://github.com/{}/pull/{}", request.repo, request.pr);
    }
    state.commit_subject.clone_from(&request.commit_subject);
    Ok(state)
}

fn refresh_pr_metadata(state: &mut ShipState, request: &ShipExecutionRequest) {
    if let Some(pr_url) = request.pr_url.as_deref().filter(|value| !value.is_empty()) {
        pr_url.clone_into(&mut state.pr_url);
    }
    if let Some(pr_title) = request
        .pr_title
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        pr_title.clone_into(&mut state.pr_title);
    }
}

fn validate_existing_state(
    state: &ShipState,
    sha: &str,
    policy: &str,
) -> Result<(), ShipExecutionError> {
    if state.is_sha_drift(sha) {
        return Err(ShipExecutionError::ShaDrift {
            existing: state.head_sha.clone(),
            current: sha.to_owned(),
        });
    }
    if state.policy_signature != policy {
        return Err(ShipExecutionError::PolicyDrift {
            existing: state.policy_signature.clone(),
            current: policy.to_owned(),
        });
    }
    Ok(())
}

fn policy_signature(
    targets: &[ResolvedTarget],
    target_names: &[String],
    mode: ValidationMode,
) -> String {
    let platforms = targets
        .iter()
        .map(|target| target.platform.clone())
        .collect::<Vec<_>>();
    compute_policy_signature(&platforms, target_names, policy_mode_label(mode))
}

fn policy_mode_label(mode: ValidationMode) -> &'static str {
    match mode {
        ValidationMode::Full => "FULL",
        ValidationMode::Smoke => "SMOKE",
    }
}

fn target_names(targets: &[ResolvedTarget]) -> Vec<String> {
    targets
        .iter()
        .map(|target| target.name.clone())
        .collect::<Vec<_>>()
}

fn target_log_path(state_dir: &Path, job_id: &str, target: &str) -> PathBuf {
    state_dir
        .join("logs")
        .join(job_id)
        .join(format!("{target}.log"))
}

fn running_result(
    target: &ResolvedTarget,
    log_path: &Path,
    started_at: Option<chrono::DateTime<Utc>>,
) -> TargetResult {
    let mut result = TargetResult::new(
        target.name.clone(),
        target.platform.clone(),
        TargetStatus::Running,
        target.backend_name.clone(),
    );
    result.started_at = started_at;
    result.log_path = Some(log_path.to_string_lossy().into_owned());
    result
}

fn cancelled_result(
    target: &ResolvedTarget,
    started_at: Option<chrono::DateTime<Utc>>,
) -> TargetResult {
    let mut result = TargetResult::new(
        target.name.clone(),
        target.platform.clone(),
        TargetStatus::Cancelled,
        "skipped",
    );
    result.started_at = started_at;
    result.completed_at = Some(Utc::now());
    result.error_message = Some("Skipped (earlier target failed, --fail-fast)".to_owned());
    result
}

fn record_evidence(
    evidence: &EvidenceStore,
    request: &ShipExecutionRequest,
    job: &Job,
) -> Result<(), ShipExecutionError> {
    let targets = request
        .targets
        .iter()
        .map(|target| (target.name.as_str(), target))
        .collect::<BTreeMap<_, _>>();
    for result in job.results.values() {
        let target = targets.get(result.target_name.as_str()).copied();
        evidence
            .record(&evidence_record(request, result, target))
            .map_err(|error| ShipExecutionError::Evidence(error.to_string()))?;
    }
    Ok(())
}

fn evidence_record(
    request: &ShipExecutionRequest,
    result: &TargetResult,
    target: Option<&ResolvedTarget>,
) -> EvidenceRecord {
    EvidenceRecord {
        sha: request.sha.clone(),
        branch: request.branch.clone(),
        target_name: result.target_name.clone(),
        platform: result.platform.clone(),
        status: evidence_status(result).to_owned(),
        backend: result.backend.clone(),
        completed_at: result.completed_at.unwrap_or_else(Utc::now),
        duration_secs: result.duration_secs,
        host: target.and_then(|target| target.host.clone()),
        primary_backend: result.primary_backend.clone(),
        failover_reason: result.failover_reason.clone(),
        provider: result.provider.clone(),
        runner_profile: result.runner_profile.clone(),
        failure_class: result.failure_class.clone(),
        reused_from: result.reused_from.clone(),
        contract_digest: None,
        stages_signature: None,
    }
}

fn update_ship_state_from_job(state: &mut ShipState, request: &ShipExecutionRequest, job: &Job) {
    let targets = request
        .targets
        .iter()
        .map(|target| (target.name.as_str(), target))
        .collect::<BTreeMap<_, _>>();
    for result in job.results.values() {
        state.update_evidence(&result.target_name, evidence_status(result));
        let run = dispatched_run(
            state,
            job,
            result,
            targets.get(result.target_name.as_str()).copied(),
            !request.advisory_targets.contains(&result.target_name),
        );
        state.upsert_run(run);
    }
}

fn dispatched_run(
    state: &ShipState,
    job: &Job,
    result: &TargetResult,
    target: Option<&ResolvedTarget>,
    required: bool,
) -> DispatchedRun {
    let now = Utc::now();
    DispatchedRun {
        target: result.target_name.clone(),
        provider: result
            .provider
            .clone()
            .or_else(|| result.primary_backend.clone())
            .unwrap_or_else(|| {
                target.map_or_else(
                    || result.backend.clone(),
                    |target| target.backend_name.clone(),
                )
            }),
        // Issue #303: prefer the cloud (GHA) workflow run id when present so
        // the dispatched-run record actually points at the workflow run a user
        // can open in the browser. Fall back to the internal Shipyard job id
        // for local/SSH/Windows backends that don't yield a GHA run.
        run_id: result
            .cloud_run_id
            .map_or_else(|| job.id.clone(), |id| id.to_string()),
        status: if result.passed() {
            "completed".to_owned()
        } else {
            "failed".to_owned()
        },
        started_at: result.started_at.unwrap_or(now),
        updated_at: result.completed_at.unwrap_or(now),
        attempt: state.attempt,
        last_heartbeat_at: result.last_heartbeat_at,
        phase: result.phase.clone(),
        required,
    }
}

fn evidence_status(result: &TargetResult) -> &'static str {
    if result.passed() { "pass" } else { "fail" }
}

/// Result of consulting the warm pool for a target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WarmReuseDecision {
    /// Target after any warm workdir override has been applied.
    pub target: ResolvedTarget,
    /// Stable pool key for this target/host pair.
    pub host_key: String,
    /// Whether a live same-SHA pool entry was consumed.
    pub warm_hit: bool,
    /// Effective resume stage for the executor.
    pub resume_from: Option<String>,
}

/// Mutation performed after a target run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarmPoolUpdate {
    /// No pool mutation was needed.
    Noop,
    /// A passing target refreshed or inserted an entry.
    Upserted,
    /// A failing warm reuse evicted an entry.
    Evicted,
}

/// Apply same-SHA warm-pool reuse to a resolved target when possible.
#[must_use]
pub fn apply_warm_reuse(
    pool: &WarmPool,
    target: &ResolvedTarget,
    sha: &str,
    requested_resume_from: Option<&str>,
    globally_off: bool,
    now: f64,
) -> WarmReuseDecision {
    let host_key = warm_host_key(target.host.as_deref());
    let resume_from = requested_resume_from.map(ToOwned::to_owned);
    if globally_off
        || target.warm_keepalive_seconds == 0
        || !is_backend_eligible(&target.backend_name)
    {
        return miss(target, host_key, resume_from);
    }

    let Some(entry) = pool.get(&target.name, &host_key, now) else {
        return miss(target, host_key, resume_from);
    };
    if entry.sha != sha {
        return miss(target, host_key, resume_from);
    }

    WarmReuseDecision {
        target: target.clone().with_workdir(entry.workdir),
        host_key,
        warm_hit: true,
        resume_from: Some(effective_warm_resume(requested_resume_from).to_owned()),
    }
}

/// Record or evict a warm-pool entry after a target run.
pub fn update_warm_pool_after_run(
    pool: &WarmPool,
    target: &ResolvedTarget,
    sha: &str,
    result: &TargetResult,
    warm_was_applied: bool,
    globally_off: bool,
    now: f64,
) -> Result<WarmPoolUpdate, std::io::Error> {
    let host = warm_host_key(target.host.as_deref());
    if result.passed() {
        if globally_off
            || target.warm_keepalive_seconds == 0
            || !is_backend_eligible(&target.backend_name)
        {
            return Ok(WarmPoolUpdate::Noop);
        }
        pool.upsert(PoolEntry::new(
            target.name.clone(),
            host,
            target.backend_name.clone(),
            target
                .workdir()
                .unwrap_or_else(|| DEFAULT_WORKDIR.to_owned()),
            sha.to_owned(),
            compute_expires_at(target.warm_keepalive_seconds, now),
            now,
        ))?;
        return Ok(WarmPoolUpdate::Upserted);
    }

    if warm_was_applied {
        let _removed = pool.evict(&target.name, &host)?;
        return Ok(WarmPoolUpdate::Evicted);
    }
    Ok(WarmPoolUpdate::Noop)
}

fn miss(
    target: &ResolvedTarget,
    host_key: String,
    resume_from: Option<String>,
) -> WarmReuseDecision {
    WarmReuseDecision {
        target: target.clone(),
        host_key,
        warm_hit: false,
        resume_from,
    }
}

fn effective_warm_resume(requested_resume_from: Option<&str>) -> &str {
    let Some(requested) = requested_resume_from else {
        return WARM_DEFAULT_RESUME_FROM;
    };
    let Some(requested_index) = RESUME_ORDER.iter().position(|stage| *stage == requested) else {
        return WARM_DEFAULT_RESUME_FROM;
    };
    let default_index = RESUME_ORDER
        .iter()
        .position(|stage| *stage == WARM_DEFAULT_RESUME_FROM)
        .expect("warm default is in order");
    if requested_index > default_index {
        requested
    } else {
        WARM_DEFAULT_RESUME_FROM
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    use chrono::{Duration, Utc};
    use toml::Table;

    use super::{
        ShipExecutionError, ShipExecutionRequest, ShipStores, ShipTargetDispatcher, WarmPoolUpdate,
        apply_warm_reuse, execute_ship, update_warm_pool_after_run,
    };
    use crate::evidence::EvidenceStore;
    use crate::executor::dispatch::{
        DispatchValidationRequest, ResolvedTarget, resolve_targets_from_table,
    };
    use crate::executor::streaming::ProgressEvent;
    use crate::job::{Priority, TargetResult, TargetStatus, ValidationMode};
    use crate::queue::Queue;
    use crate::ship_state::{ShipState, ShipStateStore};
    use crate::warm_pool::{PoolEntry, WarmPool};

    fn table(input: &str) -> Table {
        input.parse::<Table>().expect("valid TOML")
    }

    fn ssh_target() -> ResolvedTarget {
        let config = table(
            r#"
            [targets.ubuntu]
            backend = "ssh"
            platform = "linux-x64"
            host = "vm"
            repo_path = "~/repo"
            warm_keepalive_seconds = 600
            "#,
        );
        resolve_targets_from_table(&config, ValidationMode::Full)
            .expect("targets")
            .remove(0)
    }

    fn ship_request(targets: Vec<ResolvedTarget>) -> ShipExecutionRequest {
        ShipExecutionRequest {
            pr: 42,
            repo: "danielraffel/pulp".to_owned(),
            branch: "feature/test".to_owned(),
            base_branch: "main".to_owned(),
            sha: "abc".to_owned(),
            commit_subject: "test commit".to_owned(),
            pr_url: Some("https://github.com/danielraffel/pulp/pull/42".to_owned()),
            pr_title: Some("Test PR".to_owned()),
            mode: ValidationMode::Full,
            priority: Priority::Normal,
            warm_disabled: false,
            fail_fast: false,
            resume_from: None,
            advisory_targets: BTreeSet::new(),
            targets,
        }
    }

    fn pool_with(entry: PoolEntry) -> (tempfile::TempDir, WarmPool) {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool = WarmPool::new(temp.path().join("warm_pool.json"));
        pool.upsert(entry).expect("upsert");
        (temp, pool)
    }

    fn entry(sha: &str, workdir: &str) -> PoolEntry {
        PoolEntry::new("ubuntu", "vm", "ssh", workdir, sha, 100.0, 10.0)
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "{actual} != {expected}"
        );
    }

    struct FakeDispatcher {
        status: TargetStatus,
        progress_event: Option<ProgressEvent>,
        seen_workdirs: RefCell<Vec<Option<String>>>,
        seen_resume: RefCell<Vec<Option<String>>>,
        seen_durable_progress: RefCell<Vec<TargetResult>>,
    }

    impl FakeDispatcher {
        fn new(status: TargetStatus) -> Self {
            Self {
                status,
                progress_event: None,
                seen_workdirs: RefCell::new(Vec::new()),
                seen_resume: RefCell::new(Vec::new()),
                seen_durable_progress: RefCell::new(Vec::new()),
            }
        }

        fn with_progress_event(mut self, event: ProgressEvent) -> Self {
            self.progress_event = Some(event);
            self
        }
    }

    impl ShipTargetDispatcher for FakeDispatcher {
        fn validate(&self, mut request: DispatchValidationRequest<'_, '_>) -> TargetResult {
            self.seen_workdirs
                .borrow_mut()
                .push(request.target.workdir());
            self.seen_resume
                .borrow_mut()
                .push(request.resume_from.clone());
            if let Some(event) = self.progress_event.clone() {
                if let Some(callback) = request.progress_callback.as_mut() {
                    callback(event);
                }
                self.seen_durable_progress
                    .borrow_mut()
                    .push(read_target_result_from_queue(
                        &request.log_path,
                        &request.target.name,
                    ));
            }
            let now = Utc::now();
            let mut result = TargetResult::new(
                request.target.name.clone(),
                request.target.platform.clone(),
                self.status,
                request.target.backend_name.clone(),
            );
            result.started_at = Some(now);
            result.completed_at = Some(now);
            result.log_path = Some(request.log_path.to_string_lossy().into_owned());
            result
        }
    }

    fn read_target_result_from_queue(log_path: &std::path::Path, target: &str) -> TargetResult {
        let job_dir = log_path.parent().expect("target log parent");
        let logs_dir = job_dir.parent().expect("logs parent");
        let state_dir = logs_dir.parent().expect("state dir");
        let job_id = job_dir
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .expect("job id");
        let mut queue = Queue::new(state_dir).expect("queue");
        queue
            .get(job_id)
            .expect("queue get")
            .expect("job")
            .results
            .get(target)
            .expect("target result")
            .clone()
    }

    #[test]
    fn warm_hit_overrides_workdir_and_defaults_resume_to_configure() {
        let target = ssh_target();
        let (_temp, pool) = pool_with(entry("abc", "/srv/warm"));

        let decision = apply_warm_reuse(&pool, &target, "abc", None, false, 20.0);

        assert!(decision.warm_hit);
        assert_eq!(decision.host_key, "vm");
        assert_eq!(decision.resume_from.as_deref(), Some("configure"));
        assert_eq!(decision.target.workdir().as_deref(), Some("/srv/warm"));
    }

    #[test]
    fn requested_later_resume_wins_over_warm_default() {
        let target = ssh_target();
        let (_temp, pool) = pool_with(entry("abc", "/srv/warm"));

        let decision = apply_warm_reuse(&pool, &target, "abc", Some("test"), false, 20.0);

        assert!(decision.warm_hit);
        assert_eq!(decision.resume_from.as_deref(), Some("test"));
    }

    #[test]
    fn sha_miss_preserves_requested_resume_and_original_workdir() {
        let target = ssh_target();
        let (_temp, pool) = pool_with(entry("old", "/srv/warm"));

        let decision = apply_warm_reuse(&pool, &target, "new", Some("build"), false, 20.0);

        assert!(!decision.warm_hit);
        assert_eq!(decision.resume_from.as_deref(), Some("build"));
        assert_eq!(decision.target.workdir().as_deref(), Some("~/repo"));
    }

    #[test]
    fn pass_upserts_warm_pool_entry() {
        let target = ssh_target();
        let temp = tempfile::tempdir().expect("tempdir");
        let pool = WarmPool::new(temp.path().join("warm_pool.json"));
        let result = TargetResult::new("ubuntu", "linux-x64", TargetStatus::Pass, "ssh");

        let update = update_warm_pool_after_run(&pool, &target, "abc", &result, false, false, 20.0)
            .expect("update");

        assert_eq!(update, WarmPoolUpdate::Upserted);
        let entry = pool.get("ubuntu", "vm", 21.0).expect("entry");
        assert_eq!(entry.sha, "abc");
        assert_eq!(entry.workdir, "~/repo");
        assert_close(entry.expires_at, 620.0);
    }

    #[test]
    fn failing_warm_reuse_evicts_entry() {
        let target = ssh_target();
        let (_temp, pool) = pool_with(entry("abc", "/srv/warm"));
        let result = TargetResult::new("ubuntu", "linux-x64", TargetStatus::Fail, "ssh");

        let update = update_warm_pool_after_run(&pool, &target, "abc", &result, true, false, 20.0)
            .expect("update");

        assert_eq!(update, WarmPoolUpdate::Evicted);
        assert!(pool.get("ubuntu", "vm", 21.0).is_none());
    }

    #[test]
    fn disabled_pool_does_not_mutate_on_pass() {
        let target = ssh_target();
        let temp = tempfile::tempdir().expect("tempdir");
        let pool = WarmPool::new(temp.path().join("warm_pool.json"));
        let result = TargetResult::new("ubuntu", "linux-x64", TargetStatus::Pass, "ssh");

        let update = update_warm_pool_after_run(&pool, &target, "abc", &result, false, true, 20.0)
            .expect("update");

        assert_eq!(update, WarmPoolUpdate::Noop);
        assert!(pool.all_entries().is_empty());
    }

    #[test]
    fn execute_ship_records_queue_evidence_ship_state_and_warm_pool() {
        let target = ssh_target();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut queue = Queue::new(temp.path().join("state")).expect("queue");
        let evidence = EvidenceStore::new(temp.path().join("evidence")).expect("evidence");
        let ship_state = ShipStateStore::new(temp.path().join("ship")).expect("ship");
        let warm_pool = WarmPool::new(temp.path().join("warm_pool.json"));
        let dispatcher = FakeDispatcher::new(TargetStatus::Pass);
        let request = ship_request(vec![target]);

        let outcome = execute_ship(
            &request,
            ShipStores {
                queue: &mut queue,
                evidence: &evidence,
                ship_state: &ship_state,
                warm_pool: &warm_pool,
                state_dir: temp.path(),
            },
            &dispatcher,
        )
        .expect("execute");

        assert!(outcome.job.passed());
        assert!(!outcome.resumed_existing_state);
        assert_eq!(
            queue
                .get(&outcome.job.id)
                .expect("queue")
                .expect("job")
                .status,
            crate::job::JobStatus::Completed
        );
        let evidence_record = evidence
            .get_target("feature/test", "ubuntu")
            .expect("evidence");
        assert_eq!(evidence_record.status, "pass");
        assert_eq!(evidence_record.host.as_deref(), Some("vm"));
        let state = ship_state.get(42).expect("state");
        assert_eq!(state.pr_url, "https://github.com/danielraffel/pulp/pull/42");
        assert_eq!(state.pr_title, "Test PR");
        assert_eq!(state.evidence_snapshot["ubuntu"], "pass");
        let run = state.get_run("ubuntu").expect("run");
        assert_eq!(run.status, "completed");
        assert_eq!(run.provider, "ssh");
        assert_eq!(run.run_id, outcome.job.id);
        assert!(run.required);
        assert!(
            warm_pool
                .get("ubuntu", "vm", crate::warm_pool::now_epoch_secs())
                .is_some()
        );
    }

    #[test]
    fn execute_ship_marks_advisory_targets_non_required() {
        let target = ssh_target();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut queue = Queue::new(temp.path().join("state")).expect("queue");
        let evidence = EvidenceStore::new(temp.path().join("evidence")).expect("evidence");
        let ship_state = ShipStateStore::new(temp.path().join("ship")).expect("ship");
        let warm_pool = WarmPool::new(temp.path().join("warm_pool.json"));
        let dispatcher = FakeDispatcher::new(TargetStatus::Fail);
        let mut request = ship_request(vec![target]);
        request.advisory_targets.insert("ubuntu".to_owned());

        let outcome = execute_ship(
            &request,
            ShipStores {
                queue: &mut queue,
                evidence: &evidence,
                ship_state: &ship_state,
                warm_pool: &warm_pool,
                state_dir: temp.path(),
            },
            &dispatcher,
        )
        .expect("execute");

        assert!(!outcome.job.passed());
        let state = ship_state.get(42).expect("state");
        let run = state.get_run("ubuntu").expect("run");
        assert!(!run.required);
        assert_eq!(state.evidence_snapshot["ubuntu"], "fail");
    }

    #[test]
    fn execute_ship_persists_streaming_progress_before_target_finishes() {
        let target = ssh_target();
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("state");
        let mut queue = Queue::new(&state_dir).expect("queue");
        let evidence = EvidenceStore::new(temp.path().join("evidence")).expect("evidence");
        let ship_state = ShipStateStore::new(temp.path().join("ship")).expect("ship");
        let warm_pool = WarmPool::new(temp.path().join("warm_pool.json"));
        let heartbeat = Utc::now() - Duration::seconds(11);
        let dispatcher =
            FakeDispatcher::new(TargetStatus::Pass).with_progress_event(ProgressEvent {
                phase: Some("build".to_owned()),
                last_output_at: Some(heartbeat),
                last_heartbeat_at: heartbeat,
                quiet_for_secs: 11.0,
                liveness: "quiet".to_owned(),
            });
        let request = ship_request(vec![target]);

        let outcome = execute_ship(
            &request,
            ShipStores {
                queue: &mut queue,
                evidence: &evidence,
                ship_state: &ship_state,
                warm_pool: &warm_pool,
                state_dir: &state_dir,
            },
            &dispatcher,
        )
        .expect("execute");

        assert!(outcome.job.passed());
        let durable_progress = dispatcher.seen_durable_progress.borrow();
        assert_eq!(durable_progress.len(), 1);
        assert_eq!(durable_progress[0].status, TargetStatus::Running);
        assert_eq!(durable_progress[0].phase.as_deref(), Some("build"));
        assert_eq!(durable_progress[0].last_output_at, Some(heartbeat));
        assert_eq!(durable_progress[0].last_heartbeat_at, Some(heartbeat));
        assert_eq!(durable_progress[0].quiet_for_secs, Some(11.0));
        assert_eq!(durable_progress[0].liveness.as_deref(), Some("quiet"));
    }

    #[test]
    fn execute_ship_applies_and_evicts_failed_warm_reuse() {
        let target = ssh_target();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut queue = Queue::new(temp.path().join("state")).expect("queue");
        let evidence = EvidenceStore::new(temp.path().join("evidence")).expect("evidence");
        let ship_state = ShipStateStore::new(temp.path().join("ship")).expect("ship");
        let warm_pool = WarmPool::new(temp.path().join("warm_pool.json"));
        let now = crate::warm_pool::now_epoch_secs();
        warm_pool
            .upsert(PoolEntry::new(
                "ubuntu",
                "vm",
                "ssh",
                "/srv/warm",
                "abc",
                now + 600.0,
                now,
            ))
            .expect("warm entry");
        let dispatcher = FakeDispatcher::new(TargetStatus::Fail);
        let request = ship_request(vec![target]);

        let outcome = execute_ship(
            &request,
            ShipStores {
                queue: &mut queue,
                evidence: &evidence,
                ship_state: &ship_state,
                warm_pool: &warm_pool,
                state_dir: temp.path(),
            },
            &dispatcher,
        )
        .expect("execute");

        assert!(!outcome.job.passed());
        assert_eq!(
            dispatcher.seen_workdirs.borrow()[0].as_deref(),
            Some("/srv/warm")
        );
        assert_eq!(
            dispatcher.seen_resume.borrow()[0].as_deref(),
            Some("configure")
        );
        assert!(
            warm_pool
                .get("ubuntu", "vm", crate::warm_pool::now_epoch_secs())
                .is_none()
        );
        let state = ship_state.get(42).expect("state");
        assert_eq!(state.evidence_snapshot["ubuntu"], "fail");
        assert_eq!(state.get_run("ubuntu").expect("run").status, "failed");
    }

    #[test]
    fn execute_ship_refuses_existing_state_sha_drift() {
        let target = ssh_target();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut queue = Queue::new(temp.path().join("state")).expect("queue");
        let evidence = EvidenceStore::new(temp.path().join("evidence")).expect("evidence");
        let ship_state = ShipStateStore::new(temp.path().join("ship")).expect("ship");
        let warm_pool = WarmPool::new(temp.path().join("warm_pool.json"));
        ship_state
            .save(&ShipState::new(
                42,
                "danielraffel/pulp",
                "feature/test",
                "main",
                "old",
                "policy",
            ))
            .expect("save");
        let dispatcher = FakeDispatcher::new(TargetStatus::Pass);
        let request = ship_request(vec![target]);

        let error = execute_ship(
            &request,
            ShipStores {
                queue: &mut queue,
                evidence: &evidence,
                ship_state: &ship_state,
                warm_pool: &warm_pool,
                state_dir: temp.path(),
            },
            &dispatcher,
        )
        .expect_err("sha drift");

        assert!(matches!(
            error,
            ShipExecutionError::ShaDrift { existing, current }
                if existing == "old" && current == "abc"
        ));
        assert!(queue.get_pending().expect("pending").is_empty());
    }
}
