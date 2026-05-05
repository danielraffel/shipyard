//! Pull request title/body composition.

use std::path::Path;
use std::process::Command;

use crate::lane_policy::LanePolicy;

const MAX_COMMIT_WALK: usize = 20;
const MECHANICAL_PREFIXES: [&str; 5] = [
    "chore: bump versions",
    "chore(plugin): bump",
    "chore(release):",
    "chore: regenerate changelog",
    "docs: regenerate changelog",
];

/// Compose a PR title from the most recent meaningful commit.
#[must_use]
pub fn compose_pr_title(cwd: &Path, branch: &str) -> String {
    meaningful_commit(cwd)
        .and_then(|commit| commit.subject)
        .filter(|subject| !subject.is_empty())
        .unwrap_or_else(|| title_from_branch(branch))
}

/// Compose a PR body from the most recent meaningful commit body.
#[must_use]
pub fn compose_pr_body(cwd: &Path) -> String {
    compose_pr_body_with_policy(cwd, None)
}

/// Compose a PR body from commit text plus optional advisory-lane policy.
#[must_use]
pub fn compose_pr_body_with_policy(cwd: &Path, policy: Option<&LanePolicy>) -> String {
    let mut lines = Vec::new();
    if let Some(body) = meaningful_commit(cwd).and_then(|commit| commit.body) {
        lines.push(body);
    }
    if let Some(policy) = policy
        && !policy.advisory_targets.is_empty()
    {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("## Advisory lanes".to_owned());
        lines.push(
            "The following lanes are **advisory** — their status is informational and does not block merge:"
                .to_owned(),
        );
        lines.extend(policy.advisory_targets.iter().map(|target| {
            let suffix = if policy.overrides_from_trailer.contains(target) {
                " (overridden via Lane-Policy trailer)"
            } else {
                ""
            };
            format!("- `{target}`{suffix}")
        }));
    }
    lines.join("\n")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommitText {
    subject: Option<String>,
    body: Option<String>,
}

fn meaningful_commit(cwd: &Path) -> Option<CommitText> {
    (0..MAX_COMMIT_WALK)
        .map(|offset| commit_text(cwd, offset))
        .find(|commit| {
            commit
                .subject
                .as_deref()
                .is_some_and(|subject| !is_mechanical_subject(subject))
        })
}

fn commit_text(cwd: &Path, offset: usize) -> CommitText {
    let rev = if offset == 0 {
        "HEAD".to_owned()
    } else {
        format!("HEAD~{offset}")
    };
    CommitText {
        subject: git_log_field(cwd, &rev, "%s"),
        body: git_log_field(cwd, &rev, "%b"),
    }
}

fn git_log_field(cwd: &Path, rev: &str, format: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["log", "-1", &format!("--format={format}"), rev])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn is_mechanical_subject(subject: &str) -> bool {
    let lowered = subject.to_lowercase();
    MECHANICAL_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
}

fn title_from_branch(branch: &str) -> String {
    let segment = branch.rsplit('/').next().unwrap_or(branch);
    let mut title = segment.replace(['-', '_'], " ");
    if let Some(first) = title.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    title
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use crate::lane_policy::LanePolicy;

    use super::{compose_pr_body, compose_pr_body_with_policy, compose_pr_title};

    fn git(args: &[&str], cwd: &std::path::Path) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git command should run");
        assert!(status.success(), "git command failed: {args:?}");
    }

    fn seed_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        git(&["init", "--quiet", "--initial-branch=main"], temp.path());
        std::fs::write(temp.path().join("README.md"), "seed\n").expect("readme");
        git(&["add", "."], temp.path());
        git(&["commit", "-q", "-m", "seed"], temp.path());
        temp
    }

    #[test]
    fn title_and_body_use_meaningful_commit() {
        let temp = seed_repo();
        std::fs::write(temp.path().join("a.txt"), "a\n").expect("a");
        git(&["add", "."], temp.path());
        git(
            &[
                "commit",
                "-q",
                "-m",
                "Fix controller",
                "-m",
                "Detailed body",
            ],
            temp.path(),
        );

        assert_eq!(
            compose_pr_title(temp.path(), "feature/fallback"),
            "Fix controller"
        );
        assert_eq!(compose_pr_body(temp.path()), "Detailed body");
    }

    #[test]
    fn title_skips_mechanical_tip_commit() {
        let temp = seed_repo();
        std::fs::write(temp.path().join("a.txt"), "a\n").expect("a");
        git(&["add", "."], temp.path());
        git(&["commit", "-q", "-m", "Add feature"], temp.path());
        std::fs::write(temp.path().join("CHANGELOG.md"), "generated\n").expect("changelog");
        git(&["add", "."], temp.path());
        git(
            &["commit", "-q", "-m", "docs: regenerate changelog for v1"],
            temp.path(),
        );

        assert_eq!(
            compose_pr_title(temp.path(), "feature/fallback"),
            "Add feature"
        );
    }

    #[test]
    fn title_falls_back_to_branch_when_git_is_unavailable() {
        let temp = tempfile::tempdir().expect("tempdir");

        assert_eq!(
            compose_pr_title(temp.path(), "feature/fix-shipyard_pin"),
            "Fix shipyard pin"
        );
        assert_eq!(compose_pr_body(temp.path()), "");
    }

    #[test]
    fn body_appends_advisory_lanes() {
        let temp = seed_repo();
        std::fs::write(temp.path().join("a.txt"), "a\n").expect("a");
        git(&["add", "."], temp.path());
        git(
            &["commit", "-q", "-m", "Add feature", "-m", "Why"],
            temp.path(),
        );
        let policy = LanePolicy {
            advisory_targets: ["windows".to_owned()].into_iter().collect(),
            overrides_from_trailer: ["windows".to_owned()].into_iter().collect(),
        };

        assert_eq!(
            compose_pr_body_with_policy(temp.path(), Some(&policy)),
            "Why\n\n## Advisory lanes\nThe following lanes are **advisory** — their status is informational and does not block merge:\n- `windows` (overridden via Lane-Policy trailer)"
        );
    }
}
