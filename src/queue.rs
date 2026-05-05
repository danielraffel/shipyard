//! Durable machine-global job queue.

use std::cmp::Ordering;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde_json::{Value, json};

use crate::job::{Job, JobStatus, TargetResult, TargetStatus};

/// Number of completed jobs retained in the durable queue.
pub const KEEP_COMPLETED: usize = 25;
/// Default number of Windows replace attempts after PR `#214`.
pub const WINDOWS_REPLACE_ATTEMPTS: usize = 18;
/// Base backoff delay. Attempt `n` sleeps in `[0.5*base*n, 1.5*base*n]`.
pub const WINDOWS_REPLACE_BASE_DELAY: Duration = Duration::from_millis(50);
const STALE_RECOVERY_MESSAGE: &str = "Process died mid-validation; job recovered on startup";

/// Fallible queue operation result.
pub type QueueResult<T> = Result<T, QueueError>;

/// Durable queue operation error.
#[derive(Debug)]
pub enum QueueError {
    /// Filesystem operation failed.
    Io(io::Error),
    /// Queue JSON serialization failed.
    Json(serde_json::Error),
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "queue I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "queue JSON failed: {error}"),
        }
    }
}

impl std::error::Error for QueueError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
        }
    }
}

impl From<io::Error> for QueueError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for QueueError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Persistent, file-locked job queue.
#[derive(Debug)]
pub struct Queue {
    state_dir: PathBuf,
    jobs: Vec<Job>,
    loaded: bool,
}

impl Queue {
    /// Open a queue rooted at `state_dir`.
    pub fn new(state_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let state_dir = state_dir.into();
        fs::create_dir_all(&state_dir)?;
        Ok(Self {
            state_dir,
            jobs: Vec::new(),
            loaded: false,
        })
    }

    /// Queue state directory.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Durable queue file path.
    #[must_use]
    pub fn queue_file(&self) -> PathBuf {
        self.state_dir.join("queue.json")
    }

    /// Drain lock file path.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join("queue.lock")
    }

    /// Add a job, superseding pending jobs for the same branch, target list, and mode.
    pub fn enqueue(&mut self, job: Job) -> QueueResult<Job> {
        self.ensure_loaded()?;
        self.jobs.retain(|queued| {
            queued.branch != job.branch
                || queued.status != JobStatus::Pending
                || queued.target_names != job.target_names
                || queued.mode != job.mode
        });
        self.jobs.push(job.clone());
        self.save()?;
        Ok(job)
    }

    /// Return the highest-priority pending job, preserving FIFO within each priority.
    pub fn next_pending(&mut self) -> QueueResult<Option<Job>> {
        self.ensure_loaded()?;
        Ok(self.pending_jobs_sorted().into_iter().next())
    }

    /// Replace a queued job matched by id, then trim old completed jobs.
    pub fn update(&mut self, job: &Job) -> QueueResult<()> {
        self.ensure_loaded()?;
        for queued in &mut self.jobs {
            if queued.id == job.id {
                *queued = job.clone();
            }
        }
        self.trim_completed();
        self.save()
    }

    /// Look up a job by id.
    pub fn get(&mut self, job_id: &str) -> QueueResult<Option<Job>> {
        self.ensure_loaded()?;
        Ok(self.jobs.iter().find(|job| job.id == job_id).cloned())
    }

    /// Return the currently running job, if any.
    pub fn get_active(&mut self) -> QueueResult<Option<Job>> {
        self.ensure_loaded()?;
        Ok(self
            .jobs
            .iter()
            .find(|job| job.status == JobStatus::Running)
            .cloned())
    }

    /// Return completed jobs newest first.
    pub fn get_recent(&mut self, limit: usize) -> QueueResult<Vec<Job>> {
        self.ensure_loaded()?;
        let mut completed = self
            .jobs
            .iter()
            .filter(|job| job.status == JobStatus::Completed)
            .cloned()
            .collect::<Vec<_>>();
        sort_recent_completed(&mut completed);
        completed.truncate(limit);
        Ok(completed)
    }

    /// Return pending jobs sorted by priority descending, then FIFO.
    pub fn get_pending(&mut self) -> QueueResult<Vec<Job>> {
        self.ensure_loaded()?;
        Ok(self.pending_jobs_sorted())
    }

    /// Count pending jobs.
    pub fn pending_count(&mut self) -> QueueResult<usize> {
        self.ensure_loaded()?;
        Ok(self
            .jobs
            .iter()
            .filter(|job| job.status == JobStatus::Pending)
            .count())
    }

    /// Count running jobs.
    pub fn running_count(&mut self) -> QueueResult<usize> {
        self.ensure_loaded()?;
        Ok(self
            .jobs
            .iter()
            .filter(|job| job.status == JobStatus::Running)
            .count())
    }

    /// Try to acquire exclusive drain ownership.
    pub fn acquire_drain_lock(&self) -> QueueResult<Option<DrainLock>> {
        DrainLock::acquire(self.lock_file()).map_err(QueueError::Io)
    }

    fn ensure_loaded(&mut self) -> QueueResult<()> {
        if self.loaded {
            return Ok(());
        }
        self.load()
    }

    fn load(&mut self) -> QueueResult<()> {
        self.jobs = self.read_jobs_from_disk()?;
        if self.jobs.iter().any(|job| job.status == JobStatus::Running) && !self.is_drain_active() {
            self.recover_stale_running_jobs();
            self.save()?;
        }
        self.loaded = true;
        Ok(())
    }

    fn read_jobs_from_disk(&self) -> QueueResult<Vec<Job>> {
        let queue_file = self.queue_file();
        let raw = match fs::read_to_string(&queue_file) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        Ok(parse_jobs_payload(&raw))
    }

    fn save(&self) -> QueueResult<()> {
        fs::create_dir_all(&self.state_dir)?;
        self.sweep_legacy_tmp();

        let payload = json!({
            "jobs": self.jobs.iter().map(Job::to_json_value).collect::<Vec<_>>(),
        });
        let payload = format!("{}\n", serde_json::to_string_pretty(&payload)?);
        let (temp_path, mut temp_file) = create_unique_temp_file(&self.state_dir)?;

        let result = (|| -> QueueResult<()> {
            temp_file.write_all(payload.as_bytes())?;
            temp_file.flush()?;
            temp_file.sync_all()?;
            drop(temp_file);
            replace_file_with_windows_retry(&temp_path, &self.queue_file())?;
            sync_directory_best_effort(&self.state_dir);
            Ok(())
        })();

        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    fn sweep_legacy_tmp(&self) {
        let mut legacy_tmp = self.queue_file();
        legacy_tmp.set_extension("json.tmp");
        let _ = fs::remove_file(legacy_tmp);
    }

    fn is_drain_active(&self) -> bool {
        let lock_file = self.lock_file();
        let Ok(file) = OpenOptions::new().read(true).write(true).open(lock_file) else {
            return false;
        };
        match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = file.unlock();
                false
            }
            Err(_) => true,
        }
    }

    fn recover_stale_running_jobs(&mut self) {
        for job in self
            .jobs
            .iter_mut()
            .filter(|job| job.status == JobStatus::Running)
        {
            for target_name in &job.target_names {
                job.results
                    .entry(target_name.clone())
                    .or_insert_with(|| stale_recovery_result(target_name));
            }
            job.status = JobStatus::Completed;
            job.completed_at = Some(Utc::now());
        }
    }

    fn pending_jobs_sorted(&self) -> Vec<Job> {
        let mut pending = self
            .jobs
            .iter()
            .filter(|job| job.status == JobStatus::Pending)
            .cloned()
            .collect::<Vec<_>>();
        pending.sort_by(compare_pending_jobs);
        pending
    }

    fn trim_completed(&mut self) {
        let mut completed = self
            .jobs
            .iter()
            .filter(|job| job.status == JobStatus::Completed)
            .cloned()
            .collect::<Vec<_>>();
        sort_recent_completed(&mut completed);
        completed.truncate(KEEP_COMPLETED);

        let mut retained = self
            .jobs
            .iter()
            .filter(|job| job.status != JobStatus::Completed)
            .cloned()
            .collect::<Vec<_>>();
        retained.extend(completed);
        self.jobs = retained;
    }
}

/// Exclusive queue-drain ownership guard.
#[derive(Debug)]
pub struct DrainLock {
    file: Option<File>,
}

impl DrainLock {
    fn acquire(path: PathBuf) -> io::Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                file.set_len(0)?;
                writeln!(file, "{}", process::id())?;
                file.sync_all()?;
                Ok(Some(Self { file: Some(file) }))
            }
            Err(error) if lock_is_contended(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Release the lock early.
    pub fn release(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.take() {
            file.unlock()?;
        }
        Ok(())
    }
}

fn lock_is_contended(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }

    #[cfg(windows)]
    {
        // Windows reports byte-range lock contention as ERROR_LOCK_VIOLATION.
        error.raw_os_error() == Some(33)
    }

    #[cfg(not(windows))]
    {
        false
    }
}

impl Drop for DrainLock {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Retry policy for replacing the durable queue file on Windows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplaceRetryPolicy {
    /// Maximum replace attempts.
    pub attempts: usize,
    /// Linear backoff unit.
    pub base_delay: Duration,
}

impl Default for ReplaceRetryPolicy {
    fn default() -> Self {
        Self {
            attempts: WINDOWS_REPLACE_ATTEMPTS,
            base_delay: WINDOWS_REPLACE_BASE_DELAY,
        }
    }
}

/// Atomically replace `dst` with `src`, using jittered retry on Windows.
///
/// POSIX gets a single rename attempt. Windows retries `PermissionDenied`
/// because `MoveFileEx` can transiently fail when a peer writer is
/// mid-rename or the destination is briefly open.
pub fn replace_file_with_windows_retry(src: &Path, dst: &Path) -> io::Result<()> {
    retry_replace_with_strategy(
        cfg!(windows),
        ReplaceRetryPolicy::default(),
        || fs::rename(src, dst),
        thread::sleep,
        random_jitter,
    )
}

/// Testable retry loop used by `replace_file_with_windows_retry`.
pub fn retry_replace_with_strategy<R, S, J>(
    is_windows: bool,
    policy: ReplaceRetryPolicy,
    mut replace: R,
    mut sleep: S,
    mut jitter: J,
) -> io::Result<()>
where
    R: FnMut() -> io::Result<()>,
    S: FnMut(Duration),
    J: FnMut(Duration) -> Duration,
{
    if !is_windows {
        return replace();
    }

    let attempts = policy.attempts.max(1);
    let mut last_error = None;

    for attempt_index in 0..attempts {
        match replace() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                last_error = Some(error);
                if attempt_index + 1 == attempts {
                    break;
                }
                let base = scaled_delay(policy.base_delay, attempt_index + 1);
                sleep((base / 2) + jitter(base));
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_error.expect("permission error recorded before retry exhaustion"))
}

fn scaled_delay(base: Duration, multiplier: usize) -> Duration {
    let nanos = base.as_nanos().saturating_mul(multiplier as u128);
    let capped = nanos.min(u128::from(u64::MAX));
    Duration::from_nanos(u64::try_from(capped).unwrap_or(u64::MAX))
}

fn random_jitter(max: Duration) -> Duration {
    let max_nanos = max.as_nanos();
    if max_nanos == 0 {
        return Duration::ZERO;
    }
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    Duration::from_nanos(u64::try_from(seed % max_nanos).unwrap_or(u64::MAX))
}

fn parse_jobs_payload(raw: &str) -> Vec<Job> {
    if raw.trim().is_empty() {
        return Vec::new();
    }

    let parsed: Value = match serde_json::from_str(raw) {
        Ok(parsed) => parsed,
        Err(_) => return Vec::new(),
    };
    let Some(jobs) = parsed.get("jobs").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut parsed_jobs = Vec::with_capacity(jobs.len());
    for job in jobs {
        let Ok(parsed_job) = serde_json::from_value::<Job>(job.clone()) else {
            return Vec::new();
        };
        parsed_jobs.push(parsed_job);
    }
    parsed_jobs
}

fn compare_pending_jobs(left: &Job, right: &Job) -> Ordering {
    right
        .priority
        .value()
        .cmp(&left.priority.value())
        .then_with(|| left.created_at.cmp(&right.created_at))
}

fn sort_recent_completed(jobs: &mut [Job]) {
    jobs.sort_by(|left, right| completed_sort_time(right).cmp(completed_sort_time(left)));
}

fn completed_sort_time(job: &Job) -> &DateTime<Utc> {
    job.completed_at.as_ref().unwrap_or(&job.created_at)
}

fn stale_recovery_result(target_name: &str) -> TargetResult {
    let mut result = TargetResult::new(
        target_name.to_owned(),
        "unknown",
        TargetStatus::Error,
        "unknown",
    );
    result.error_message = Some(STALE_RECOVERY_MESSAGE.to_owned());
    result
}

fn create_unique_temp_file(state_dir: &Path) -> io::Result<(PathBuf, File)> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    for attempt in 0..100 {
        let path = state_dir.join(format!(
            ".queue-{}-{stamp}-{attempt}.json.tmp",
            process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create unique queue temp file",
    ))
}

fn sync_directory_best_effort(path: &Path) {
    if let Ok(directory) = File::open(path) {
        let _ = directory.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    use chrono::Utc;
    use serde_json::Value;
    use tempfile::TempDir;

    use crate::job::{Job, JobStatus, Priority, TargetResult, TargetStatus, ValidationMode};

    use super::{
        KEEP_COMPLETED, Queue, ReplaceRetryPolicy, STALE_RECOVERY_MESSAGE,
        WINDOWS_REPLACE_ATTEMPTS, retry_replace_with_strategy, scaled_delay,
    };

    fn queue_dir() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn job(branch: &str, sha: &str, targets: &[&str]) -> Job {
        Job::create(
            sha,
            branch,
            targets.iter().map(|target| (*target).to_owned()).collect(),
            ValidationMode::Full,
            Priority::Normal,
        )
    }

    fn completed_from(mut job: Job, seconds_ago: i64) -> Job {
        job = job.start().expect("start").complete().expect("complete");
        job.completed_at = Some(Utc::now() - chrono::Duration::seconds(seconds_ago));
        job
    }

    fn read_queue_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path.join("queue.json")).expect("queue json"))
            .expect("valid json")
    }

    #[test]
    fn enqueue_and_retrieve_job() {
        let temp = queue_dir();
        let mut queue = Queue::new(temp.path()).expect("queue");
        let job = job("main", "abc", &["mac"]);
        let id = job.id.clone();

        queue.enqueue(job).expect("enqueue");

        assert_eq!(queue.pending_count().expect("pending"), 1);
        assert_eq!(queue.get(&id).expect("get").expect("job").sha, "abc");
    }

    #[test]
    fn next_pending_prefers_priority_then_fifo() {
        let temp = queue_dir();
        let mut queue = Queue::new(temp.path()).expect("queue");
        let low = job("feat/low", "a", &["mac"]).with_priority(Priority::Low);
        let first_high = job("feat/high-a", "b", &["mac"]).with_priority(Priority::High);
        let second_high = job("feat/high-b", "c", &["mac"]).with_priority(Priority::High);
        let first_high_id = first_high.id.clone();

        queue.enqueue(low).expect("low");
        queue.enqueue(first_high).expect("first high");
        queue.enqueue(second_high).expect("second high");

        assert_eq!(
            queue.next_pending().expect("next").expect("job").id,
            first_high_id
        );
    }

    #[test]
    fn supersedence_replaces_pending_same_scope_only() {
        let temp = queue_dir();
        let mut queue = Queue::new(temp.path()).expect("queue");
        let old = job("feat/x", "old", &["mac"]);
        let running = job("feat/x", "running", &["mac"]).start().expect("start");
        let narrow = job("feat/x", "narrow", &["linux"]);
        let smoke = Job::create(
            "smoke",
            "feat/x",
            vec!["mac".to_owned()],
            ValidationMode::Smoke,
            Priority::Normal,
        );
        let new = job("feat/x", "new", &["mac"]);

        queue.enqueue(old).expect("old");
        queue.enqueue(running.clone()).expect("running");
        queue.update(&running).expect("update running");
        queue.enqueue(narrow).expect("narrow");
        queue.enqueue(smoke).expect("smoke");
        queue.enqueue(new).expect("new");

        let pending = queue.get_pending().expect("pending");
        assert_eq!(queue.running_count().expect("running"), 1);
        assert_eq!(pending.len(), 3);
        assert!(pending.iter().any(|job| job.sha == "new"));
        assert!(pending.iter().any(|job| job.sha == "narrow"));
        assert!(pending.iter().any(|job| job.sha == "smoke"));
        assert!(!pending.iter().any(|job| job.sha == "old"));
    }

    #[test]
    fn update_get_active_and_persistence_round_trip() {
        let temp = queue_dir();
        let state_dir = temp.path().to_path_buf();
        let mut queue = Queue::new(&state_dir).expect("queue");
        let job = job("main", "abc", &["mac"]);
        let id = job.id.clone();
        queue.enqueue(job.clone()).expect("enqueue");
        assert!(queue.get_active().expect("active").is_none());

        let started = job.start().expect("start");
        queue.update(&started).expect("update");
        assert_eq!(
            queue.get_active().expect("active").expect("job").status,
            JobStatus::Running
        );

        let mut reopened = Queue::new(&state_dir).expect("reopen");
        assert_eq!(
            reopened.get(&id).expect("get").expect("job").status,
            JobStatus::Completed
        );
    }

    #[test]
    fn held_drain_lock_prevents_stale_running_recovery() {
        let temp = queue_dir();
        let state_dir = temp.path().to_path_buf();
        let mut queue = Queue::new(&state_dir).expect("queue");
        let lock = queue.acquire_drain_lock().expect("lock").expect("held");
        let started = job("main", "abc", &["mac"]).start().expect("start");
        let id = started.id.clone();
        queue.enqueue(started).expect("enqueue");
        drop(queue);

        let mut reopened = Queue::new(&state_dir).expect("reopen");
        assert_eq!(
            reopened.get(&id).expect("get").expect("job").status,
            JobStatus::Running
        );
        drop(lock);
    }

    #[test]
    fn stale_running_jobs_recover_when_no_drain_lock_is_held() {
        let temp = queue_dir();
        let state_dir = temp.path().to_path_buf();
        let mut queue = Queue::new(&state_dir).expect("queue");
        let started = job("main", "abc", &["mac", "linux"])
            .start()
            .expect("start")
            .with_result(TargetResult::new(
                "mac",
                "macos",
                TargetStatus::Pass,
                "local",
            ));
        let id = started.id.clone();
        queue.enqueue(started).expect("enqueue");
        drop(queue);

        let mut reopened = Queue::new(&state_dir).expect("reopen");
        let recovered = reopened.get(&id).expect("get").expect("job");

        assert_eq!(recovered.status, JobStatus::Completed);
        assert_eq!(recovered.results["mac"].status, TargetStatus::Pass);
        assert_eq!(recovered.results["linux"].status, TargetStatus::Error);
        assert_eq!(
            recovered.results["linux"].error_message.as_deref(),
            Some(STALE_RECOVERY_MESSAGE)
        );
    }

    #[test]
    fn recent_completed_jobs_are_newest_first_and_trimmed() {
        let temp = queue_dir();
        let mut queue = Queue::new(temp.path()).expect("queue");

        for index in 0..(KEEP_COMPLETED + 10) {
            let pending = job(&format!("feat/{index}"), &format!("sha{index}"), &["mac"]);
            let completed = completed_from(
                pending.clone(),
                i64::try_from(KEEP_COMPLETED + 10 - index).expect("seconds"),
            );
            queue.enqueue(pending).expect("enqueue");
            queue.update(&completed).expect("update");
        }

        let recent = queue.get_recent(100).expect("recent");
        assert_eq!(recent.len(), KEEP_COMPLETED);
        assert_eq!(recent.first().expect("first").sha, "sha34");
    }

    #[test]
    fn empty_missing_zero_byte_and_corrupt_queue_files_load_as_empty() {
        for contents in [None, Some(""), Some("   "), Some(r#"{"jobs": [{"id":"#)] {
            let temp = queue_dir();
            if let Some(contents) = contents {
                fs::write(temp.path().join("queue.json"), contents).expect("write");
            }
            let mut queue = Queue::new(temp.path()).expect("queue");
            assert_eq!(queue.pending_count().expect("pending"), 0);
        }
    }

    #[test]
    fn save_writes_atomic_json_sweeps_legacy_tmp_and_leaves_no_temp_files() {
        let temp = queue_dir();
        fs::write(temp.path().join("queue.json.tmp"), "legacy").expect("legacy");
        let mut queue = Queue::new(temp.path()).expect("queue");

        queue
            .enqueue(job("main", "abc", &["mac"]))
            .expect("enqueue");

        assert!(read_queue_json(temp.path())["jobs"].as_array().is_some());
        assert!(!temp.path().join("queue.json.tmp").exists());
        let leftovers = fs::read_dir(temp.path())
            .expect("read dir")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".queue-"))
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "orphan queue temp files: {leftovers:?}"
        );
    }

    #[test]
    fn drain_lock_is_exclusive_and_releases_on_drop_or_manual_release() {
        let temp = queue_dir();
        let queue = Queue::new(temp.path()).expect("queue");
        let mut first = queue.acquire_drain_lock().expect("first").expect("lock");

        assert!(queue.acquire_drain_lock().expect("second").is_none());
        first.release().expect("release");

        let second = queue.acquire_drain_lock().expect("third").expect("lock");
        drop(second);
        assert!(queue.acquire_drain_lock().expect("after drop").is_some());
    }

    #[test]
    fn posix_path_attempts_once_without_sleep_or_jitter() {
        let mut replace_calls = 0;
        let mut sleep_calls = 0;
        let mut jitter_calls = 0;

        retry_replace_with_strategy(
            false,
            ReplaceRetryPolicy::default(),
            || {
                replace_calls += 1;
                Ok(())
            },
            |_| sleep_calls += 1,
            |_| {
                jitter_calls += 1;
                Duration::ZERO
            },
        )
        .expect("replace");

        assert_eq!(replace_calls, 1);
        assert_eq!(sleep_calls, 0);
        assert_eq!(jitter_calls, 0);
    }

    #[test]
    fn windows_path_uses_centered_growing_jittered_backoff() {
        let attempts = Cell::new(0);
        let mut sleeps = Vec::new();
        let mut jitter_bounds = Vec::new();

        retry_replace_with_strategy(
            true,
            ReplaceRetryPolicy::default(),
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() <= 3 {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "busy"))
                } else {
                    Ok(())
                }
            },
            |duration| sleeps.push(duration),
            |bound| {
                jitter_bounds.push(bound);
                bound / 2
            },
        )
        .expect("eventual success");

        assert_eq!(attempts.get(), 4);
        assert_eq!(
            jitter_bounds,
            vec![
                Duration::from_millis(50),
                Duration::from_millis(100),
                Duration::from_millis(150),
            ]
        );
        assert_eq!(
            sleeps,
            vec![
                Duration::from_millis(50),
                Duration::from_millis(100),
                Duration::from_millis(150),
            ]
        );
    }

    #[test]
    fn windows_path_surfaces_permission_error_after_budget() {
        let mut replace_calls = 0;
        let error = retry_replace_with_strategy(
            true,
            ReplaceRetryPolicy::default(),
            || {
                replace_calls += 1;
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "access denied",
                ))
            },
            |_| {},
            |_| Duration::ZERO,
        )
        .expect_err("budget exhausted");

        assert_eq!(replace_calls, WINDOWS_REPLACE_ATTEMPTS);
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "access denied");
    }

    #[test]
    fn windows_path_does_not_retry_non_permission_errors() {
        let mut replace_calls = 0;
        let error = retry_replace_with_strategy(
            true,
            ReplaceRetryPolicy::default(),
            || {
                replace_calls += 1;
                Err(io::Error::new(io::ErrorKind::NotFound, "missing tmp"))
            },
            |_| panic!("should not sleep"),
            |_| panic!("should not draw jitter"),
        )
        .expect_err("not found");

        assert_eq!(replace_calls, 1);
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn scaled_delay_saturates() {
        assert_eq!(
            scaled_delay(Duration::from_millis(50), 3),
            Duration::from_millis(150)
        );
        assert_eq!(
            scaled_delay(Duration::from_nanos(u64::MAX), 2),
            Duration::from_nanos(u64::MAX)
        );
    }
}
