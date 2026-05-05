use std::io::Write;

use crate::paths::RuntimePaths;

pub(super) fn print_paths<W: Write>(
    stdout: &mut W,
    paths: &RuntimePaths,
) -> Result<(), Box<dyn std::error::Error>> {
    writeln!(stdout, "mode: {}", paths.mode)?;
    writeln!(stdout, "binary_name: {}", paths.binary_name)?;
    writeln!(
        stdout,
        "tracked_project_dir_name: {}",
        paths.tracked_project_dir_name
    )?;
    writeln!(
        stdout,
        "local_overlay_dir_name: {}",
        paths.local_overlay_dir_name
    )?;
    writeln!(stdout, "global_dir: {}", paths.global_dir.display())?;
    writeln!(stdout, "state_dir: {}", paths.state_dir.display())?;
    writeln!(stdout, "daemon_dir: {}", paths.daemon_dir.display())?;
    writeln!(stdout, "daemon_socket: {}", paths.daemon_socket.display())?;
    writeln!(
        stdout,
        "daemon_pid_file: {}",
        paths.daemon_pid_file.display()
    )?;
    writeln!(
        stdout,
        "daemon_log_file: {}",
        paths.daemon_log_file.display()
    )?;
    Ok(())
}
