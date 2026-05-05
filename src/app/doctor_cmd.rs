use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use serde_json::Value;

use crate::config::LoadedConfig;
use crate::doctor::{
    DoctorReport, SystemCommandProbe, check_release_chain, collect_report, collect_runner_checks,
    runner_config_error_checks,
};
use crate::identity::RuntimeMode;
use crate::output::write_json_envelope;

pub(super) fn doctor<W: Write>(
    json: bool,
    mode: RuntimeMode,
    cwd: &Path,
    state_dir: &Path,
    release_chain: bool,
    runners: bool,
    stdout: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut report = collect_report(&SystemCommandProbe, cwd, state_dir);
    if release_chain && let Some(entry) = check_release_chain(cwd) {
        report
            .checks
            .entry("Release pipeline".to_owned())
            .or_default()
            .insert("release_chain".to_owned(), entry);
    }
    if runners {
        let runner_section = match LoadedConfig::load_from_cwd(mode, cwd) {
            Ok(config) => collect_runner_checks(&config),
            Err(error) => Some(runner_config_error_checks(error.to_string())),
        };
        if let Some(section) = runner_section {
            report.checks.insert("Runners".to_owned(), section);
        }
    }
    if json {
        let mut data = BTreeMap::new();
        let value = serde_json::to_value(report)?;
        let Value::Object(map) = value else {
            return Err("doctor report must serialize as an object".into());
        };
        for (key, value) in map {
            data.insert(key, value);
        }
        write_json_envelope(stdout, "doctor", data)?;
        return Ok(());
    }

    write_human_report(stdout, report)?;
    Ok(())
}

fn write_human_report<W: Write>(stdout: &mut W, report: DoctorReport) -> std::io::Result<()> {
    writeln!(stdout, "shipyard doctor")?;
    writeln!(
        stdout,
        "{}",
        if report.ready {
            "ready: yes"
        } else {
            "ready: no"
        }
    )?;
    for (section_name, entries) in report.checks {
        writeln!(stdout, "{section_name}")?;
        for (name, entry) in entries {
            let status = if entry.ok { "ok" } else { "fail" };
            let summary = entry
                .version
                .as_deref()
                .or(entry.error.as_deref())
                .unwrap_or("");
            writeln!(stdout, "  {name}: {status} {summary}")?;
            if !entry.ok
                && let Some(detail) = entry.detail.as_deref()
                && detail != summary
            {
                for line in detail.lines().filter(|line| !line.trim().is_empty()) {
                    writeln!(stdout, "    {line}")?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::doctor::{DoctorEntry, DoctorReport};

    use super::write_human_report;

    #[test]
    fn human_doctor_renders_detail_for_failing_rows() {
        let mut checks = BTreeMap::new();
        let mut section = BTreeMap::new();
        section.insert(
            "rich-bundle".to_owned(),
            DoctorEntry {
                ok: false,
                version: None,
                detail: Some("Reinstall using install.sh.\nRun: curl ... | bash".to_owned()),
                error: Some("broken bundle".to_owned()),
            },
        );
        section.insert(
            "healthy".to_owned(),
            DoctorEntry {
                ok: true,
                version: Some("ok".to_owned()),
                detail: Some("hidden detail".to_owned()),
                error: None,
            },
        );
        checks.insert("Core".to_owned(), section);
        let mut output = Vec::new();

        write_human_report(
            &mut output,
            DoctorReport {
                ready: false,
                checks,
            },
        )
        .expect("render");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("rich-bundle: fail broken bundle"));
        assert!(text.contains("    Reinstall using install.sh."));
        assert!(text.contains("    Run: curl ... | bash"));
        assert!(!text.contains("hidden detail"));
    }
}
