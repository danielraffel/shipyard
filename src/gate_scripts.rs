use std::env;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use crate::config::LoadedConfig;

/// One repo-local gate script that `shipyard pr` needs to resolve.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GateScript {
    /// Human-readable script name used in diagnostics.
    pub name: &'static str,
    /// Environment variable override.
    pub env_var: &'static str,
    /// Dotted Shipyard config key override.
    pub config_key: &'static str,
    /// Default filename under supported script directories.
    pub filename: &'static str,
}

/// Skill synchronization gate.
pub const SKILL_SYNC: GateScript = GateScript {
    name: "skill_sync_check",
    env_var: "SHIPYARD_SKILL_SYNC_SCRIPT",
    config_key: "validation.skill_sync_script",
    filename: "skill_sync_check.py",
};

/// Version bump gate.
pub const VERSION_BUMP: GateScript = GateScript {
    name: "version_bump_check",
    env_var: "SHIPYARD_VERSION_BUMP_SCRIPT",
    config_key: "validation.version_bump_script",
    filename: "version_bump_check.py",
};

/// Versioning surface configuration.
pub const VERSIONING_CONFIG: GateScript = GateScript {
    name: "versioning_config",
    env_var: "SHIPYARD_VERSIONING_CONFIG",
    config_key: "validation.versioning_config",
    filename: "versioning.json",
};

const DEFAULT_DIRS: [&str; 2] = ["tools/scripts", "scripts"];

/// Error raised when a gate script cannot be resolved.
#[derive(Debug)]
pub struct GateScriptNotFound {
    message: String,
}

impl Display for GateScriptNotFound {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for GateScriptNotFound {}

/// Resolve a gate-script path using Python Shipyard's precedence order.
pub fn resolve(
    script: GateScript,
    repo_root: &Path,
    config: &LoadedConfig,
) -> Result<PathBuf, GateScriptNotFound> {
    resolve_with_env(script, repo_root, config, |key| env::var(key).ok())
}

fn resolve_with_env<F>(
    script: GateScript,
    repo_root: &Path,
    config: &LoadedConfig,
    mut env_value: F,
) -> Result<PathBuf, GateScriptNotFound>
where
    F: FnMut(&str) -> Option<String>,
{
    if let Some(value) = env_value(script.env_var).filter(|value| !value.is_empty()) {
        let candidate = absolute(Path::new(&value), repo_root);
        if candidate.exists() {
            return Ok(candidate);
        }
        return Err(not_found(
            script,
            repo_root,
            vec![(format!("env {}", script.env_var), candidate)],
            Some(value),
            None,
        ));
    }

    let config_value = config
        .get_str(script.config_key)
        .filter(|value| !value.is_empty());
    if let Some(value) = config_value {
        let candidate = absolute(Path::new(value), repo_root);
        if candidate.exists() {
            return Ok(candidate);
        }
        return Err(not_found(
            script,
            repo_root,
            vec![(format!("config {}", script.config_key), candidate)],
            None,
            Some(value.to_owned()),
        ));
    }

    let mut probed = Vec::new();
    for directory in DEFAULT_DIRS {
        let candidate = repo_root.join(directory).join(script.filename);
        probed.push((format!("{directory}/"), candidate.clone()));
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(not_found(script, repo_root, probed, None, None))
}

fn absolute(path: &Path, repo_root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

fn not_found(
    script: GateScript,
    repo_root: &Path,
    probed: Vec<(String, PathBuf)>,
    env_value: Option<String>,
    config_value: Option<String>,
) -> GateScriptNotFound {
    let mut lines = vec![format!("shipyard: could not find {}.", script.filename)];
    lines.push("  Tried:".to_owned());
    for (label, path) in probed {
        lines.push(format!("    - {label} {}", path.display()));
    }
    lines.push(String::new());
    lines.push("  Override by setting one of:".to_owned());
    lines.push(format!("    - env {}=<path>", script.env_var));
    lines.push(format!(
        "    - {} in .shipyard/config.toml",
        script.config_key
    ));
    lines.push(format!(
        "    - place the file at {}",
        repo_root
            .join("tools")
            .join("scripts")
            .join(script.filename)
            .display()
    ));
    lines.push(format!(
        "    - or at {}",
        repo_root.join("scripts").join(script.filename).display()
    ));
    if let Some(value) = env_value {
        lines.push(String::new());
        lines.push(format!(
            "  Note: env {}={value:?} did not resolve to an existing file.",
            script.env_var
        ));
    }
    if let Some(value) = config_value {
        lines.push(String::new());
        lines.push(format!(
            "  Note: config {}={value:?} did not resolve to an existing file.",
            script.config_key
        ));
    }
    GateScriptNotFound {
        message: lines.join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::config::{LoadedConfig, LocalOverlaySource};

    use super::*;

    fn config_from_toml(contents: &str) -> LoadedConfig {
        LoadedConfig {
            data: contents.parse::<toml::Table>().expect("config TOML"),
            global_dir: PathBuf::from("/tmp/global"),
            project_dir: None,
            local_dir: None,
            local_overlay_source: LocalOverlaySource::None,
        }
    }

    fn touch(path: &Path) -> PathBuf {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, "# placeholder\n").expect("write");
        path.to_path_buf()
    }

    #[test]
    fn tools_scripts_wins_over_scripts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = config_from_toml("");
        touch(&temp.path().join("scripts").join("skill_sync_check.py"));
        let tools = touch(
            &temp
                .path()
                .join("tools")
                .join("scripts")
                .join("skill_sync_check.py"),
        );

        let resolved =
            resolve_with_env(SKILL_SYNC, temp.path(), &config, |_| None).expect("resolved");

        assert_eq!(resolved, tools);
    }

    #[test]
    fn env_override_wins_over_config_and_defaults() {
        let temp = tempfile::tempdir().expect("tempdir");
        let env_target = touch(&temp.path().join("from-env").join("sync.py"));
        touch(&temp.path().join("from-config").join("sync.py"));
        touch(&temp.path().join("scripts").join("skill_sync_check.py"));
        let config = config_from_toml(
            r#"
[validation]
skill_sync_script = "from-config/sync.py"
"#,
        );

        let resolved = resolve_with_env(SKILL_SYNC, temp.path(), &config, |key| {
            (key == "SHIPYARD_SKILL_SYNC_SCRIPT").then(|| String::from("from-env/sync.py"))
        })
        .expect("resolved");

        assert_eq!(resolved, env_target);
    }

    #[test]
    fn missing_override_reports_override_knobs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = config_from_toml("");

        let error = resolve_with_env(SKILL_SYNC, temp.path(), &config, |_| {
            Some(String::from("missing.py"))
        })
        .expect_err("missing");
        let message = error.to_string();

        assert!(message.contains("SHIPYARD_SKILL_SYNC_SCRIPT"));
        assert!(message.contains("validation.skill_sync_script"));
        assert!(message.contains("missing.py"));
        assert!(message.contains(&format!(
            "{}{}{}",
            "tools",
            std::path::MAIN_SEPARATOR,
            "scripts"
        )));
        assert!(message.contains("skill_sync_check.py"));
    }
}
