//! GitHub webhook registration through the user's existing `gh` auth.
//!
//! The registrar mirrors the Python daemon contract: hook IDs are persisted
//! under `daemon/registrations.json`, restarts patch existing hooks instead of
//! creating duplicates, and shutdown best-effort unregisters known hooks.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use wait_timeout::ChildExt;

/// GitHub webhook events Shipyard subscribes to.
pub const SUBSCRIBED_EVENTS: [&str; 6] = [
    "workflow_run",
    "workflow_job",
    "pull_request",
    "check_run",
    "check_suite",
    "release",
];

const GH_API_TIMEOUT: Duration = Duration::from_secs(15);

/// Durable repo-to-hook mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredHook {
    /// Repository in `owner/name` form.
    pub repo: String,
    /// GitHub webhook ID.
    pub hook_id: u64,
}

/// Non-recoverable registrar failure.
#[derive(Debug)]
pub enum RegistrarError {
    /// Filesystem or process boundary failed.
    Io(std::io::Error),
    /// The configured `gh` binary is missing or not executable.
    GhUnavailable(String),
    /// A `gh api` invocation exceeded the registrar timeout.
    GhTimedOut,
    /// GitHub CLI returned a non-zero status.
    GhFailed {
        /// Registrar operation being attempted.
        action: &'static str,
        /// Combined stdout/stderr from `gh`.
        output: String,
    },
    /// GitHub CLI returned a successful response without a hook ID.
    MissingHookId(String),
    /// Persisted registration state could not be serialized or parsed.
    Json(serde_json::Error),
}

impl std::fmt::Display for RegistrarError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::GhUnavailable(message) => formatter.write_str(message),
            Self::GhTimedOut => formatter.write_str("gh api timed out"),
            Self::GhFailed { action, output } => {
                write!(formatter, "{action} hook failed: {}", output.trim())
            }
            Self::MissingHookId(output) => {
                write!(
                    formatter,
                    "couldn't parse hook ID from gh response: {}",
                    output.trim()
                )
            }
            Self::Json(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for RegistrarError {}

impl From<std::io::Error> for RegistrarError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for RegistrarError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Persistent GitHub webhook registrar.
#[derive(Clone, Debug)]
pub struct Registrar {
    state_path: PathBuf,
    by_repo: BTreeMap<String, u64>,
}

impl Registrar {
    /// Load registrar state from `<state_dir>/daemon/registrations.json`.
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        let state_path = state_dir.join("daemon").join("registrations.json");
        let by_repo = load_registrations(&state_path);
        Self {
            state_path,
            by_repo,
        }
    }

    /// Return the current repo-to-hook map.
    #[must_use]
    pub fn all(&self) -> BTreeMap<String, u64> {
        self.by_repo.clone()
    }

    /// Idempotently create or update a webhook using `gh` from `PATH`.
    pub fn ensure_registered(
        &mut self,
        repo: &str,
        url: &str,
        secret: &str,
    ) -> Result<u64, RegistrarError> {
        let gh_binary = resolve_gh_binary()
            .ok_or_else(|| RegistrarError::GhUnavailable("gh CLI not found on PATH".to_owned()))?;
        self.ensure_registered_with_gh(repo, url, secret, &gh_binary)
    }

    /// Idempotently create or update a webhook with an explicit `gh` binary.
    pub fn ensure_registered_with_gh(
        &mut self,
        repo: &str,
        url: &str,
        secret: &str,
        gh_binary: &Path,
    ) -> Result<u64, RegistrarError> {
        validate_gh_binary(gh_binary)?;
        if let Some(hook_id) = self.by_repo.get(repo).copied() {
            update_hook(gh_binary, repo, hook_id, url, secret)?;
            return Ok(hook_id);
        }

        let hook_id = create_hook(gh_binary, repo, url, secret)?;
        self.by_repo.insert(repo.to_owned(), hook_id);
        self.save()?;
        Ok(hook_id)
    }

    /// Best-effort unregister a repo using `gh` from `PATH` when present.
    pub fn unregister(&mut self, repo: &str) -> Result<(), RegistrarError> {
        let Some(hook_id) = self.by_repo.get(repo).copied() else {
            return Ok(());
        };
        if let Some(gh_binary) = resolve_gh_binary() {
            delete_hook(&gh_binary, repo, hook_id)?;
        }
        self.by_repo.remove(repo);
        self.save()
    }

    /// Unregister a repo with an explicit `gh` binary.
    pub fn unregister_with_gh(
        &mut self,
        repo: &str,
        gh_binary: &Path,
    ) -> Result<(), RegistrarError> {
        let Some(hook_id) = self.by_repo.get(repo).copied() else {
            return Ok(());
        };
        validate_gh_binary(gh_binary)?;
        delete_hook(gh_binary, repo, hook_id)?;
        self.by_repo.remove(repo);
        self.save()
    }

    /// Best-effort unregister every known repo.
    pub fn unregister_all(&mut self) -> Result<(), RegistrarError> {
        for repo in self.by_repo.keys().cloned().collect::<Vec<_>>() {
            self.unregister(&repo)?;
        }
        Ok(())
    }

    fn save(&self) -> Result<(), RegistrarError> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = self
            .by_repo
            .iter()
            .map(|(repo, hook_id)| RegistrationRecord {
                repo: repo.clone(),
                hook_id: *hook_id,
            })
            .collect::<Vec<_>>();
        fs::write(&self.state_path, serde_json::to_string_pretty(&payload)?)?;
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct RegistrationRecord {
    repo: String,
    hook_id: u64,
}

fn load_registrations(state_path: &Path) -> BTreeMap<String, u64> {
    let Ok(raw) = fs::read_to_string(state_path) else {
        return BTreeMap::new();
    };
    let Ok(records) = serde_json::from_str::<Vec<RegistrationRecord>>(&raw) else {
        return BTreeMap::new();
    };
    records
        .into_iter()
        .filter(|record| !record.repo.is_empty())
        .map(|record| (record.repo, record.hook_id))
        .collect()
}

fn create_hook(
    gh_binary: &Path,
    repo: &str,
    url: &str,
    secret: &str,
) -> Result<u64, RegistrarError> {
    let body = json!({
        "name": "web",
        "active": true,
        "events": SUBSCRIBED_EVENTS,
        "config": {
            "url": url,
            "content_type": "json",
            "secret": secret,
            "insecure_ssl": "0",
        },
    });
    let output = run_gh(
        gh_binary,
        &[
            "api",
            "-X",
            "POST",
            "-H",
            "Accept: application/vnd.github+json",
            "--input",
            "-",
            &format!("repos/{repo}/hooks"),
        ],
        Some(&body.to_string()),
    )?;
    if output.status != 0 {
        return Err(RegistrarError::GhFailed {
            action: "create",
            output: output.combined_output(),
        });
    }
    let parsed = serde_json::from_str::<serde_json::Value>(&output.stdout)
        .map_err(|_| RegistrarError::MissingHookId(output.stdout.clone()))?;
    parsed
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .ok_or(RegistrarError::MissingHookId(output.stdout))
}

fn update_hook(
    gh_binary: &Path,
    repo: &str,
    hook_id: u64,
    url: &str,
    secret: &str,
) -> Result<(), RegistrarError> {
    let body = json!({
        "config": {
            "url": url,
            "content_type": "json",
            "secret": secret,
            "insecure_ssl": "0",
        },
        "active": true,
    });
    let output = run_gh(
        gh_binary,
        &[
            "api",
            "-X",
            "PATCH",
            "-H",
            "Accept: application/vnd.github+json",
            "--input",
            "-",
            &format!("repos/{repo}/hooks/{hook_id}"),
        ],
        Some(&body.to_string()),
    )?;
    if output.status == 0 {
        return Ok(());
    }
    Err(RegistrarError::GhFailed {
        action: "patch",
        output: output.combined_output(),
    })
}

fn delete_hook(gh_binary: &Path, repo: &str, hook_id: u64) -> Result<(), RegistrarError> {
    let output = run_gh(
        gh_binary,
        &[
            "api",
            "-X",
            "DELETE",
            &format!("repos/{repo}/hooks/{hook_id}"),
        ],
        None,
    )?;
    if output.status == 0 {
        return Ok(());
    }
    let combined = output.combined_output();
    let lowered = combined.to_ascii_lowercase();
    if lowered.contains("404") || lowered.contains("not found") {
        return Ok(());
    }
    Err(RegistrarError::GhFailed {
        action: "delete",
        output: combined,
    })
}

#[derive(Debug)]
struct GhOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

impl GhOutput {
    fn combined_output(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            format!("{}\n{}", self.stdout, self.stderr)
        }
    }
}

fn run_gh(
    gh_binary: &Path,
    args: &[&str],
    stdin: Option<&str>,
) -> Result<GhOutput, RegistrarError> {
    let mut command = Command::new(gh_binary);
    command
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn()?;
    if let Some(stdin) = stdin
        && let Some(mut writer) = child.stdin.take()
    {
        writer.write_all(stdin.as_bytes())?;
    }

    let Some(status) = child.wait_timeout(GH_API_TIMEOUT)? else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RegistrarError::GhTimedOut);
    };
    let output = child.wait_with_output()?;
    Ok(GhOutput {
        status: status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn resolve_gh_binary() -> Option<PathBuf> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .flat_map(|dir| {
                gh_candidate_names()
                    .into_iter()
                    .map(move |name| dir.join(name))
            })
            .find(|candidate| validate_gh_binary(candidate).is_ok())
    }
}

#[cfg(not(test))]
fn gh_candidate_names() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["gh.exe", "gh"]
    } else {
        vec!["gh"]
    }
}

fn validate_gh_binary(path: &Path) -> Result<(), RegistrarError> {
    let metadata = fs::metadata(path).map_err(|_| {
        RegistrarError::GhUnavailable(format!("gh CLI not executable: {}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(RegistrarError::GhUnavailable(format!(
            "gh CLI not executable: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(RegistrarError::GhUnavailable(format!(
                "gh CLI not executable: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::path::{Path, PathBuf};

    #[cfg(unix)]
    use serde_json::Value;

    use super::Registrar;
    #[cfg(unix)]
    use super::{RegistrarError, SUBSCRIBED_EVENTS};

    #[test]
    fn corrupt_state_loads_as_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_path = temp.path().join("daemon").join("registrations.json");
        fs::create_dir_all(state_path.parent().expect("parent")).expect("mkdir");
        fs::write(&state_path, "not json").expect("write");

        let registrar = Registrar::new(temp.path());

        assert!(registrar.all().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn creates_updates_deletes_and_persists_hooks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let gh = write_gh_stub(temp.path(), GhStubMode::Ok);
        let mut registrar = Registrar::new(temp.path());

        let hook_id = registrar
            .ensure_registered_with_gh(
                "owner/repo",
                "https://shipyard.example/webhook",
                "secret-one",
                &gh,
            )
            .expect("create");
        assert_eq!(hook_id, 4242);

        let persisted = fs::read_to_string(temp.path().join("daemon").join("registrations.json"))
            .expect("registrations");
        let records = serde_json::from_str::<Vec<Value>>(&persisted).expect("records");
        assert_eq!(records[0]["repo"], "owner/repo");
        assert_eq!(records[0]["hook_id"], 4242);

        let mut reloaded = Registrar::new(temp.path());
        let hook_id = reloaded
            .ensure_registered_with_gh(
                "owner/repo",
                "https://shipyard.example/rotated/webhook",
                "secret-two",
                &gh,
            )
            .expect("patch");
        assert_eq!(hook_id, 4242);

        reloaded
            .unregister_with_gh("owner/repo", &gh)
            .expect("delete");
        assert!(reloaded.all().is_empty());

        let first_args = read_log(temp.path(), "args-1");
        let first_body = read_json_log(temp.path(), "stdin-1");
        let second_args = read_log(temp.path(), "args-2");
        let second_body = read_json_log(temp.path(), "stdin-2");
        let third_args = read_log(temp.path(), "args-3");

        assert!(first_args.contains("-X POST"));
        assert!(first_args.contains("repos/owner/repo/hooks"));
        assert_eq!(first_body["name"], "web");
        assert_eq!(first_body["active"], true);
        assert_eq!(
            first_body["config"]["url"],
            "https://shipyard.example/webhook"
        );
        assert_eq!(first_body["config"]["content_type"], "json");
        assert_eq!(first_body["config"]["secret"], "secret-one");
        assert_eq!(first_body["config"]["insecure_ssl"], "0");
        let events = first_body["events"].as_array().expect("events");
        assert_eq!(events.len(), SUBSCRIBED_EVENTS.len());
        for event in SUBSCRIBED_EVENTS {
            assert!(events.iter().any(|value| value.as_str() == Some(event)));
        }

        assert!(second_args.contains("-X PATCH"));
        assert!(second_args.contains("repos/owner/repo/hooks/4242"));
        assert_eq!(
            second_body["config"]["url"],
            "https://shipyard.example/rotated/webhook"
        );
        assert_eq!(second_body["config"]["secret"], "secret-two");

        assert!(third_args.contains("-X DELETE"));
        assert!(third_args.contains("repos/owner/repo/hooks/4242"));
    }

    #[cfg(unix)]
    #[test]
    fn delete_404_is_treated_as_success() {
        let temp = tempfile::tempdir().expect("tempdir");
        let gh = write_gh_stub(temp.path(), GhStubMode::Delete404);
        let mut registrar = Registrar::new(temp.path());
        registrar
            .ensure_registered_with_gh("owner/repo", "https://example.test/webhook", "secret", &gh)
            .expect("create");

        registrar
            .unregister_with_gh("owner/repo", &gh)
            .expect("delete");

        assert!(registrar.all().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn create_requires_parseable_hook_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let gh = write_gh_stub(temp.path(), GhStubMode::MissingId);
        let mut registrar = Registrar::new(temp.path());

        let error = registrar
            .ensure_registered_with_gh("owner/repo", "https://example.test/webhook", "secret", &gh)
            .expect_err("missing hook id");

        assert!(matches!(error, RegistrarError::MissingHookId(_)));
        assert!(registrar.all().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn unregister_without_gh_removes_local_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let gh = write_gh_stub(temp.path(), GhStubMode::Ok);
        let mut registrar = Registrar::new(temp.path());
        registrar
            .ensure_registered_with_gh("owner/repo", "https://example.test/webhook", "secret", &gh)
            .expect("create");
        fs::remove_file(&gh).expect("remove gh");

        registrar.unregister("owner/repo").expect("unregister");

        assert!(registrar.all().is_empty());
    }

    #[cfg(unix)]
    #[derive(Clone, Copy)]
    enum GhStubMode {
        Ok,
        Delete404,
        MissingId,
    }

    #[cfg(unix)]
    fn write_gh_stub(temp: &Path, mode: GhStubMode) -> PathBuf {
        let gh = temp.join("gh");
        let create_response = match mode {
            GhStubMode::MissingId => "{}",
            GhStubMode::Ok | GhStubMode::Delete404 => "{\"id\":4242}",
        };
        let delete_branch = match mode {
            GhStubMode::Delete404 => {
                "  *\" -X DELETE \"*) printf '404 not found\\n' >&2; exit 1 ;;"
            }
            GhStubMode::Ok | GhStubMode::MissingId => "  *\" -X DELETE \"*) exit 0 ;;",
        };
        let script = format!(
            r#"#!/bin/sh
set -eu
LOG_DIR={log_dir}
COUNT_FILE="$LOG_DIR/counter"
COUNT="$(cat "$COUNT_FILE" 2>/dev/null || printf 0)"
COUNT="$((COUNT + 1))"
printf '%s' "$COUNT" > "$COUNT_FILE"
printf '%s\n' "$*" > "$LOG_DIR/args-$COUNT"
cat > "$LOG_DIR/stdin-$COUNT" || true
case " $* " in
  *" -X POST "*) printf '%s\n' '{create_response}' ;;
  *" -X PATCH "*) printf '%s\n' '{{}}' ;;
{delete_branch}
  *) printf 'unexpected gh args: %s\n' "$*" >&2; exit 2 ;;
esac
"#,
            log_dir = shell_quote(temp),
            create_response = create_response,
            delete_branch = delete_branch,
        );
        fs::write(&gh, script).expect("write gh stub");
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("chmod");
        gh
    }

    #[cfg(unix)]
    fn shell_quote(path: &Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
    }

    #[cfg(unix)]
    fn read_log(temp: &Path, name: &str) -> String {
        fs::read_to_string(temp.join(name)).expect(name)
    }

    #[cfg(unix)]
    fn read_json_log(temp: &Path, name: &str) -> Value {
        serde_json::from_str(&read_log(temp, name)).expect(name)
    }
}
