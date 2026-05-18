//! Demonstrates the new Phase 1 failure-diagnostics renderer using the real
//! captured failure from pulp job 76630095261. Lets reviewers see the
//! before/after side-by-side without spinning up the whole `shipyard pr` path.
//!
//! Run with: `cargo run --release --example diagnostics_demo`.

use shipyard::diagnostics::{
    DiagnosticsError, DiagnosticsFetcher, fetch_failed_job_diagnostics, select_parser,
};

struct StaticFetcher {
    jobs_json: &'static str,
    log: &'static str,
}

impl DiagnosticsFetcher for StaticFetcher {
    fn fetch_jobs_json(&self, _repo: &str, _run_id: u64) -> Result<String, DiagnosticsError> {
        Ok(self.jobs_json.to_owned())
    }

    fn fetch_job_log(&self, _repo: &str, _job_id: u64) -> Result<String, DiagnosticsError> {
        Ok(self.log.to_owned())
    }
}

fn main() {
    let fetcher = StaticFetcher {
        jobs_json: include_str!("../tests/fixtures/job_76630095261.json"),
        log: include_str!("../tests/fixtures/ctest_failed_macos.log"),
    };
    let parser = select_parser(Some("ctest"));
    let diag = fetch_failed_job_diagnostics(
        &fetcher,
        "danielraffel/pulp",
        26_063_806_409,
        "mac",
        parser.as_ref(),
    );

    println!("=== BEFORE (Shipyard <= 0.57.0) ===");
    println!("Validation failed. PR #2271 not merged.");
    println!();

    println!("=== AFTER (Phase 1) ===");
    println!("\u{2717} Validation failed. PR #2271 not merged.");
    println!("    Target:  {} (cloud=namespace)", diag.failed_target);
    if let Some(job) = diag.job.as_ref() {
        println!("    Job:     {}", job.name);
        println!("    URL:     {}", job.html_url);
        if let Some(step) = job.failed_step.as_deref() {
            println!("    Step:    \"{step}\"");
        }
    }
    if !diag.failure_summary.is_empty() {
        println!("    Tests:");
        for line in &diag.failure_summary {
            println!("      {line}");
        }
        if diag.failure_summary_truncated {
            println!("      (truncated; see job log for full list)");
        }
    }
    println!("    Action:  run `shipyard watch --pr 2271` to follow recovery, or push fix.");
}
