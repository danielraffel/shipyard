//! Git bundle operations for delivering code to remote hosts.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wait_timeout::ChildExt;

use crate::executor::ssh::shlex_quote;
use crate::executor::ssh_windows::{
    WindowsCommandError, powershell_encoded_argv, windows_bundle_path_expression,
};

/// Outcome of a bundle operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleResult {
    /// Whether the operation succeeded.
    pub success: bool,
    /// Human-readable status or failure message.
    pub message: String,
    /// Path produced or used by the operation.
    pub path: Option<String>,
    /// Upload failure classification.
    pub failure_class: String,
    /// Per-attempt upload diagnostics.
    pub attempts: Vec<String>,
}

impl BundleResult {
    /// Successful bundle result.
    #[must_use]
    pub fn success(message: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            path: Some(path.into()),
            failure_class: "other".to_owned(),
            attempts: Vec::new(),
        }
    }

    /// Failed bundle result.
    #[must_use]
    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            path: None,
            failure_class: "other".to_owned(),
            attempts: Vec::new(),
        }
    }

    /// Failed upload result with retry classification and attempts.
    #[must_use]
    pub fn upload_failure(
        message: impl Into<String>,
        failure_class: impl Into<String>,
        attempts: Vec<String>,
    ) -> Self {
        Self {
            success: false,
            message: message.into(),
            path: None,
            failure_class: failure_class.into(),
            attempts,
        }
    }
}

const WINDOWS_UPLOAD_MAX_ATTEMPTS: usize = 3;
const WINDOWS_UPLOAD_BACKOFFS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(5)];
const SSH_UNREACHABLE_FINGERPRINTS: [&str; 6] = [
    "connect to host",
    "connection refused",
    "no route to host",
    "name or service not known",
    "operation timed out",
    "network is unreachable",
];

/// Create a git bundle containing `sha`.
#[must_use]
pub fn create_bundle(
    sha: &str,
    output_path: &Path,
    repo_dir: Option<&Path>,
    basis_shas: &[String],
) -> BundleResult {
    if let Some(parent) = output_path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return BundleResult::failure(format!("OS error: {error}"));
    }

    let mut command = Command::new("git");
    command.args(["bundle", "create"]).arg(output_path).arg(sha);
    if basis_shas.is_empty() {
        command.arg("--all");
    } else {
        for basis in basis_shas {
            command.arg(format!("^{basis}"));
        }
    }
    if let Some(repo_dir) = repo_dir {
        command.current_dir(repo_dir);
    }

    match run_capture(command, Duration::from_mins(2)) {
        Ok(output) if output.timed_out => BundleResult::failure("git bundle create timed out"),
        Ok(output) if output.success() => {
            BundleResult::success("Bundle created", output_path.display().to_string())
        }
        Ok(output) => BundleResult::failure(format!(
            "git bundle create failed: {}",
            output.stderr.trim()
        )),
        Err(error) => BundleResult::failure(format!("OS error: {error}")),
    }
}

/// Upload a bundle to a POSIX remote host via `ssh cat >`.
#[must_use]
pub fn upload_bundle_posix(
    bundle_path: &Path,
    host: &str,
    remote_path: &str,
    ssh_options: &[String],
    timeout: Duration,
) -> BundleResult {
    if !bundle_path.exists() {
        return BundleResult::failure(format!("Bundle file not found: {}", bundle_path.display()));
    }

    let mut command = Command::new("ssh");
    command.args(ssh_options);
    command.arg(host);
    command.arg(format!("cat > {}", shlex_quote(remote_path)));

    match run_capture_with_stdin_file(command, bundle_path, timeout) {
        Ok(output) if output.timed_out => BundleResult::failure("Upload timed out"),
        Ok(output) if output.success() => BundleResult::success("Bundle uploaded", remote_path),
        Ok(output) => BundleResult::failure(format!("Upload failed: {}", output.stderr.trim())),
        Err(error) => BundleResult::failure(format!("OS error: {error}")),
    }
}

/// Upload a bundle to a Windows remote host via `PowerShell` raw stdin copy.
#[must_use]
pub fn upload_bundle_windows(
    bundle_path: &Path,
    host: &str,
    remote_path: &str,
    ssh_options: &[String],
    timeout: Duration,
) -> BundleResult {
    if !bundle_path.exists() {
        return BundleResult::failure(format!("Bundle file not found: {}", bundle_path.display()));
    }
    let script = match windows_upload_bundle_script(remote_path) {
        Ok(script) => script,
        Err(error) => return BundleResult::failure(error.to_string()),
    };
    let mut command = Command::new("ssh");
    command.args(ssh_options);
    command.args(powershell_encoded_argv(host, &script));

    let mut attempts = Vec::new();
    let mut failure_class = "upload_failed".to_owned();
    let mut last_detail = "no stderr captured".to_owned();

    for attempt in 1..=WINDOWS_UPLOAD_MAX_ATTEMPTS {
        match run_capture_with_stdin_file(command_from_template(&command), bundle_path, timeout) {
            Ok(output) if output.timed_out => {
                last_detail = format!("timed out after {}s", timeout.as_secs());
                "upload_failed".clone_into(&mut failure_class);
                attempts.push(format!(
                    "attempt {attempt}/{WINDOWS_UPLOAD_MAX_ATTEMPTS}: timed out after {}s",
                    timeout.as_secs()
                ));
            }
            Ok(output) if output.success() => {
                if attempt > 1 {
                    attempts.push(format!(
                        "attempt {attempt}/{WINDOWS_UPLOAD_MAX_ATTEMPTS}: success"
                    ));
                }
                let mut result = BundleResult::success("Bundle uploaded", remote_path);
                result.attempts = attempts;
                return result;
            }
            Ok(output) => {
                let stderr = output.stderr.trim();
                classify_upload_failure(stderr).clone_into(&mut failure_class);
                last_detail = if stderr.is_empty() {
                    "no stderr captured".to_owned()
                } else {
                    stderr.to_owned()
                };
                attempts.push(format!(
                    "attempt {attempt}/{WINDOWS_UPLOAD_MAX_ATTEMPTS} failed: {}",
                    if stderr.is_empty() {
                        "(no stderr)"
                    } else {
                        stderr
                    }
                ));
                if failure_class == "ssh_unreachable" {
                    break;
                }
            }
            Err(error) => {
                attempts.push(format!(
                    "attempt {attempt}/{WINDOWS_UPLOAD_MAX_ATTEMPTS}: OS error: {error}"
                ));
                return BundleResult::upload_failure(
                    format!("OS error: {error}"),
                    "other",
                    attempts,
                );
            }
        }

        if attempt < WINDOWS_UPLOAD_MAX_ATTEMPTS {
            thread::sleep(jittered_backoff(WINDOWS_UPLOAD_BACKOFFS[attempt - 1]));
        }
    }
    BundleResult::upload_failure(
        format!(
            "Upload failed after {} attempt(s): {last_detail}",
            attempts.len()
        ),
        failure_class,
        attempts,
    )
}

/// Apply a bundle on a POSIX remote host.
#[must_use]
pub fn apply_bundle_posix(
    host: &str,
    bundle_path: &str,
    repo_path: &str,
    ssh_options: &[String],
    timeout: Duration,
) -> BundleResult {
    let remote_cmd = format!(
        "cd {} && git bundle verify {} && git fetch {} '+refs/heads/*:refs/shipyard-bundles/heads/*' '+refs/tags/*:refs/shipyard-bundles/tags/*'",
        shlex_quote(repo_path),
        shlex_quote(bundle_path),
        shlex_quote(bundle_path)
    );
    let mut command = Command::new("ssh");
    command.args(ssh_options);
    command.arg(host);
    command.arg(remote_cmd);

    match run_capture(command, timeout) {
        Ok(output) if output.timed_out => BundleResult::failure("Remote bundle apply timed out"),
        Ok(output) if output.success() => BundleResult::success("Bundle applied", bundle_path),
        Ok(output) => BundleResult::failure(format!(
            "Remote bundle apply failed: {}",
            output.stderr.trim()
        )),
        Err(error) => BundleResult::failure(format!("OS error: {error}")),
    }
}

/// Build the binary-safe `PowerShell` upload script for a Windows host.
///
/// The script reads raw stdin and writes it to the resolved destination.
/// Relative paths resolve under `$HOME`, matching the apply-side bundle
/// path contract and avoiding SSHD working-directory drift.
pub fn windows_upload_bundle_script(remote_path: &str) -> Result<String, WindowsCommandError> {
    let destination = windows_bundle_path_expression(remote_path)?;
    Ok(format!(
        "$Dest = {destination};\
         $Parent = Split-Path -Parent $Dest;\
         if ($Parent) {{ New-Item -ItemType Directory -Force -Path $Parent | Out-Null }};\
         $stdin = [Console]::OpenStandardInput();\
         $fs = [System.IO.File]::Open($Dest, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read);\
         try {{\
         $stdin.CopyTo($fs);\
         $fs.Flush()\
         }} finally {{\
         $fs.Dispose();\
         $stdin.Dispose()\
         }};\
         exit 0"
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandCapture {
    returncode: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

impl CommandCapture {
    fn success(&self) -> bool {
        self.returncode == Some(0) && !self.timed_out
    }
}

fn run_capture(mut command: Command, timeout: Duration) -> io::Result<CommandCapture> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let timed_out = child.wait_timeout(timeout)?.is_none();
    if timed_out {
        let _ = child.kill();
    }
    let output = child.wait_with_output()?;
    Ok(CommandCapture {
        returncode: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        timed_out,
    })
}

fn command_from_template(template: &Command) -> Command {
    let mut command = Command::new(template.get_program());
    command.args(template.get_args());
    command
}

fn classify_upload_failure(stderr: &str) -> &'static str {
    let lower = stderr.to_lowercase();
    if SSH_UNREACHABLE_FINGERPRINTS
        .iter()
        .any(|fingerprint| lower.contains(fingerprint))
    {
        "ssh_unreachable"
    } else {
        "upload_failed"
    }
}

fn jittered_backoff(base: Duration) -> Duration {
    let millis = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
    let max_extra = millis / 2;
    if max_extra == 0 {
        return base;
    }
    let jitter = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::from(duration.subsec_nanos()) % (max_extra + 1)
        });
    base.saturating_add(Duration::from_millis(jitter))
}

fn run_capture_with_stdin_file(
    mut command: Command,
    stdin_path: &Path,
    timeout: Duration,
) -> io::Result<CommandCapture> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin piped");
    let stdin_path = stdin_path.to_path_buf();
    let writer = thread::spawn(move || copy_file_to_stdin(&stdin_path, &mut stdin));

    let timed_out = child.wait_timeout(timeout)?.is_none();
    if timed_out {
        let _ = child.kill();
    }
    let output = child.wait_with_output()?;
    let mut returncode = output.status.code();
    let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    match writer.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            if !stderr.is_empty() && !stderr.ends_with('\n') {
                stderr.push('\n');
            }
            let _ = writeln!(stderr, "stdin upload failed: {error}");
            returncode = None;
        }
        Err(_) => {
            if !stderr.is_empty() && !stderr.ends_with('\n') {
                stderr.push('\n');
            }
            stderr.push_str("stdin upload failed: writer thread panicked\n");
            returncode = None;
        }
    }

    Ok(CommandCapture {
        returncode,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr,
        timed_out,
    })
}

fn copy_file_to_stdin(path: &PathBuf, stdin: &mut impl Write) -> io::Result<()> {
    let mut file = File::open(path)?;
    io::copy(&mut file, stdin)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        BundleResult, apply_bundle_posix, classify_upload_failure, create_bundle,
        upload_bundle_posix, windows_upload_bundle_script,
    };

    #[test]
    fn bundle_result_helpers_preserve_shape() {
        assert_eq!(
            BundleResult::success("ok", "/tmp/x"),
            BundleResult {
                success: true,
                message: "ok".to_owned(),
                path: Some("/tmp/x".to_owned()),
                failure_class: "other".to_owned(),
                attempts: Vec::new()
            }
        );
        assert_eq!(
            BundleResult::failure("bad"),
            BundleResult {
                success: false,
                message: "bad".to_owned(),
                path: None,
                failure_class: "other".to_owned(),
                attempts: Vec::new()
            }
        );
    }

    #[test]
    fn upload_failure_classifier_matches_unreachable_fingerprints() {
        for stderr in [
            "ssh: connect to host 1.2.3.4 port 22: Operation timed out",
            "Connection refused",
            "No route to host",
            "Name or service not known",
            "Network is unreachable",
        ] {
            assert_eq!(classify_upload_failure(stderr), "ssh_unreachable");
        }
        assert_eq!(
            classify_upload_failure("remote write failed"),
            "upload_failed"
        );
    }

    #[test]
    fn create_bundle_reports_git_failure_without_panic() {
        let temp = tempfile::tempdir().expect("tempdir");
        let result = create_bundle(
            "definitely-not-a-sha",
            &temp.path().join("shipyard.bundle"),
            Some(temp.path()),
            &[],
        );

        assert!(!result.success);
        assert!(result.message.contains("git bundle create failed"));
    }

    #[test]
    fn upload_bundle_rejects_missing_file() {
        let result = upload_bundle_posix(
            std::path::Path::new("/definitely/missing/shipyard.bundle"),
            "host",
            "/tmp/shipyard.bundle",
            &[],
            Duration::from_secs(1),
        );

        assert!(!result.success);
        assert!(result.message.contains("Bundle file not found"));
    }

    #[test]
    fn apply_bundle_surfaces_ssh_spawn_error() {
        let result = apply_bundle_posix(
            "host",
            "/tmp/shipyard.bundle",
            "/tmp/repo",
            &["-F".to_owned(), "/definitely/missing/ssh_config".to_owned()],
            Duration::from_secs(1),
        );

        assert!(!result.success);
        assert!(
            result.message.contains("Remote bundle apply failed")
                || result.message.contains("OS error")
                || result.message.contains("timed out")
        );
    }

    #[test]
    fn windows_upload_resolves_relative_remote_path_under_home() {
        let script = windows_upload_bundle_script("shipyard.bundle").expect("script");
        assert!(script.contains("(Join-Path $HOME 'shipyard.bundle')"));
        assert!(!script.contains("[System.IO.File]::Create('shipyard.bundle')"));
        assert!(script.contains("[System.IO.File]::Open($Dest"));
        assert!(script.contains("[System.IO.FileShare]::Read"));
        assert!(script.contains("$fs.Dispose()"));
    }

    #[test]
    fn windows_upload_uses_absolute_path_as_is() {
        let script = windows_upload_bundle_script(r"C:\shipyard.bundle").expect("script");
        assert!(script.contains(r"'C:\shipyard.bundle'"));
        assert!(!script.contains("Join-Path"));
    }

    #[test]
    fn windows_upload_treats_slash_prefixed_path_as_absolute() {
        let script = windows_upload_bundle_script("/tmp/shipyard.bundle").expect("script");
        assert!(script.contains("'/tmp/shipyard.bundle'"));
        assert!(!script.contains("Join-Path"));
    }

    #[test]
    fn windows_upload_treats_unc_path_as_absolute() {
        let script =
            windows_upload_bundle_script(r"\\server\share\shipyard.bundle").expect("script");
        assert!(script.contains(r"'\\server\share\shipyard.bundle'"));
        assert!(!script.contains("Join-Path"));
    }
}
