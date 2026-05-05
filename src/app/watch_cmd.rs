use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use super::CliFailure;
use crate::evidence::EvidenceStore;
use crate::ship_state::ShipStateStore;
use crate::watch::{
    active_pr_for_current_branch, emit_watch_event, emit_watch_snapshot, reused_evidence_map,
    ship_terminal_verdict, watch_event_signature,
};

#[derive(Clone, Copy)]
pub(super) struct WatchCommandContext<'a> {
    pub(super) store: &'a ShipStateStore,
    pub(super) evidence_store: &'a EvidenceStore,
    pub(super) cwd: &'a Path,
}

#[derive(Clone, Copy)]
pub(super) struct WatchCommandOptions {
    pub(super) pr: Option<u64>,
    pub(super) follow: bool,
    pub(super) interval: f64,
    pub(super) json: bool,
}

pub(super) fn watch<W: Write>(
    context: WatchCommandContext<'_>,
    options: WatchCommandOptions,
    stdout: &mut W,
) -> Result<ExitCode, CliFailure> {
    let target_pr = options
        .pr
        .or_else(|| active_pr_for_current_branch(context.store, context.cwd));
    let Some(target_pr) = target_pr else {
        let message = "No active ship state for current branch.";
        emit_watch_event("no-active-ship", None, Some(message), options.json, stdout)
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
        return Ok(ExitCode::from(2));
    };

    let mut last_signature = None;
    let mut observed_any_state = false;

    loop {
        let state = context.store.get(target_pr);
        let Some(state) = state else {
            if !observed_any_state {
                let message = format!(
                    "PR #{target_pr}: no ship state found (typo, wrong repo, or never shipped)."
                );
                emit_watch_event(
                    "pr-not-found",
                    Some(target_pr),
                    Some(&message),
                    options.json,
                    stdout,
                )
                .map_err(|error| CliFailure::new(1, error.to_string()))?;
                return Ok(ExitCode::from(2));
            }
            let message =
                format!("PR #{target_pr}: ship state archived (merged, discarded, or pruned).");
            emit_watch_event(
                "state-archived",
                Some(target_pr),
                Some(&message),
                options.json,
                stdout,
            )
            .map_err(|error| CliFailure::new(1, error.to_string()))?;
            return Ok(ExitCode::SUCCESS);
        };

        observed_any_state = true;
        let reuse_map = reused_evidence_map(context.evidence_store, &state);
        let signature = watch_event_signature(&state, &reuse_map);
        if last_signature.as_deref() != Some(signature.as_str()) {
            emit_watch_snapshot(&state, &reuse_map, options.json, stdout)
                .map_err(|error| CliFailure::new(1, error.to_string()))?;
            last_signature = Some(signature);
        }

        if let Some(verdict) = ship_terminal_verdict(&state) {
            return Ok(if verdict {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            });
        }

        if !options.follow {
            return Ok(ExitCode::from(3));
        }

        thread::sleep(Duration::from_secs_f64(options.interval.max(1.0)));
    }
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    use chrono::Utc;
    use serde_json::Value;

    use super::{WatchCommandContext, WatchCommandOptions, watch};
    use crate::evidence::EvidenceStore;
    use crate::ship_state::{DispatchedRun, ShipState, ShipStateStore};

    fn stores(temp: &tempfile::TempDir) -> (ShipStateStore, EvidenceStore) {
        let ship = ShipStateStore::new(temp.path().join("ship-state")).expect("ship store");
        let evidence = EvidenceStore::new(temp.path().join("evidence")).expect("evidence store");
        (ship, evidence)
    }

    fn context<'a>(
        store: &'a ShipStateStore,
        evidence_store: &'a EvidenceStore,
        cwd: &'a std::path::Path,
    ) -> WatchCommandContext<'a> {
        WatchCommandContext {
            store,
            evidence_store,
            cwd,
        }
    }

    fn options(pr: Option<u64>, follow: bool, json: bool) -> WatchCommandOptions {
        WatchCommandOptions {
            pr,
            follow,
            interval: 0.01,
            json,
        }
    }

    fn sample_state(pr: u64) -> ShipState {
        let mut state = ShipState::new(
            pr,
            "danielraffel/pulp",
            "feature/test",
            "main",
            "abcdef0123456789abcdef0123456789abcdef01",
            "policy",
        );
        state.pr_title = "Test PR".to_owned();
        state.dispatched_runs.push(sample_run("linux", true));
        state
    }

    fn sample_run(target: &str, required: bool) -> DispatchedRun {
        let now = Utc::now();
        DispatchedRun {
            target: target.to_owned(),
            provider: "local".to_owned(),
            run_id: format!("run-{target}"),
            status: "in_progress".to_owned(),
            started_at: now,
            updated_at: now,
            attempt: 1,
            last_heartbeat_at: None,
            phase: Some("test".to_owned()),
            required,
        }
    }

    #[test]
    fn watch_reports_no_active_state_for_current_branch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (store, evidence) = stores(&temp);
        let mut out = Vec::new();

        let code = watch(
            context(&store, &evidence, temp.path()),
            options(None, false, true),
            &mut out,
        )
        .expect("watch");

        let payload: Value = serde_json::from_slice(&out).expect("json payload");
        assert_eq!(code, ExitCode::from(2));
        assert_eq!(payload["command"], "watch");
        assert_eq!(payload["event"], "no-active-ship");
        assert_eq!(
            payload["message"],
            "No active ship state for current branch."
        );
    }

    #[test]
    fn watch_reports_explicit_pr_not_found() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (store, evidence) = stores(&temp);
        let mut out = Vec::new();

        let code = watch(
            context(&store, &evidence, temp.path()),
            options(Some(404), false, false),
            &mut out,
        )
        .expect("watch");

        let text = String::from_utf8(out).expect("utf8");
        assert_eq!(code, ExitCode::from(2));
        assert!(text.contains("PR #404: no ship state found"));
    }

    #[test]
    fn watch_no_follow_emits_snapshot_and_exits_in_flight_code() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (store, evidence) = stores(&temp);
        store.save(&sample_state(42)).expect("state");
        let mut out = Vec::new();

        let code = watch(
            context(&store, &evidence, temp.path()),
            options(Some(42), false, true),
            &mut out,
        )
        .expect("watch");

        let payload: Value = serde_json::from_slice(&out).expect("json payload");
        assert_eq!(code, ExitCode::from(3));
        assert_eq!(payload["command"], "watch");
        assert_eq!(payload["event"], "update");
        assert_eq!(payload["pr"], 42);
        assert_eq!(payload["dispatched_runs"][0]["target"], "linux");
    }

    #[test]
    fn watch_terminal_pass_returns_success_after_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (store, evidence) = stores(&temp);
        let mut state = sample_state(42);
        state
            .evidence_snapshot
            .insert("linux".to_owned(), "pass".to_owned());
        store.save(&state).expect("state");
        let mut out = Vec::new();

        let code = watch(
            context(&store, &evidence, temp.path()),
            options(Some(42), false, true),
            &mut out,
        )
        .expect("watch");

        let payload: Value = serde_json::from_slice(&out).expect("json payload");
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(payload["evidence"]["linux"], "pass");
    }

    #[test]
    fn watch_terminal_fail_returns_failure_after_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (store, evidence) = stores(&temp);
        let mut state = sample_state(42);
        state
            .evidence_snapshot
            .insert("linux".to_owned(), "fail".to_owned());
        store.save(&state).expect("state");
        let mut out = Vec::new();

        let code = watch(
            context(&store, &evidence, temp.path()),
            options(Some(42), false, false),
            &mut out,
        )
        .expect("watch");

        let text = String::from_utf8(out).expect("utf8");
        assert_eq!(code, ExitCode::from(1));
        assert!(text.contains("PR #42"));
        assert!(text.contains("linux"));
        assert!(text.contains("fail"));
    }
}
