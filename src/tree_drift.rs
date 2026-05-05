use std::path::Path;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

/// Short opaque signature for the Git working tree relevant to `shipyard run`.
#[must_use]
pub fn compute_signature(cwd: &Path) -> Option<String> {
    let head = git_output(cwd, &["rev-parse", "HEAD"])?;
    let listing = git_output(cwd, &["ls-files", "-m", "-o", "--exclude-standard"])?;
    let diff = git_output(cwd, &["diff", "--no-ext-diff", "HEAD"])?;

    let mut hasher = Sha256::new();
    hasher.update(head.as_bytes());
    hasher.update(b"\0LS\0");
    hasher.update(listing.as_bytes());
    hasher.update(b"\0DIFF\0");
    hasher.update(diff.as_bytes());

    for path in listing.lines().filter(|line| !line.trim().is_empty()) {
        hasher.update(b"\0U\0");
        hasher.update(path.as_bytes());
        match std::fs::read(cwd.join(path)) {
            Ok(bytes) => hasher.update(bytes),
            Err(_) => hasher.update(b"<missing>"),
        }
    }

    Some(hex::encode(hasher.finalize())[..32].to_owned())
}

/// Return `git status --short` dirty paths, or empty when unavailable.
#[must_use]
pub fn list_dirty_paths(cwd: &Path) -> Vec<String> {
    git_output(cwd, &["status", "--short", "--untracked-files=all"])
        .map(|output| {
            output
                .lines()
                .map(str::trim_end)
                .filter(|line| !line.trim().is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Format the user-facing tree-drift error for a stage boundary.
#[must_use]
pub fn format_drift_error(
    stage: &str,
    initial_paths: &[String],
    current_paths: &[String],
) -> String {
    let mut lines = vec![
        format!("working tree changed during `shipyard run` (stage={stage})."),
        "mid-run edits produce non-deterministic failures (#238).".to_owned(),
        "re-run after your other edits settle, or use separate worktrees for parallel work."
            .to_owned(),
    ];
    let initial = initial_paths
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let current = current_paths
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let added = current.difference(&initial).copied().collect::<Vec<_>>();
    let removed = initial.difference(&current).copied().collect::<Vec<_>>();
    if !added.is_empty() || !removed.is_empty() {
        lines.push(String::new());
        lines.push("what changed:".to_owned());
        for entry in added {
            lines.push(format!("  + {entry}"));
        }
        for entry in removed {
            lines.push(format!("  - {entry}"));
        }
    }
    lines.push(String::new());
    lines.push(
        "(pass --allow-tree-drift to suppress this guard when you know a build step mutates the tree on purpose.)"
            .to_owned(),
    );
    lines.join("\n")
}

fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{compute_signature, format_drift_error, list_dirty_paths};

    #[test]
    fn signature_is_stable_when_tree_is_unchanged() {
        let temp = repo();
        assert_eq!(
            compute_signature(temp.path()),
            compute_signature(temp.path())
        );
    }

    #[test]
    fn signature_changes_on_tracked_file_edit() {
        let temp = repo();
        let before = compute_signature(temp.path()).expect("signature");
        std::fs::write(temp.path().join("seed.txt"), "seed\nedit\n").expect("edit");
        let after = compute_signature(temp.path()).expect("signature");
        assert_ne!(before, after);
    }

    #[test]
    fn signature_changes_on_untracked_file_content() {
        let temp = repo();
        std::fs::write(temp.path().join("new.txt"), "v1\n").expect("new");
        let before = compute_signature(temp.path()).expect("signature");
        std::fs::write(temp.path().join("new.txt"), "v2\n").expect("edit");
        let after = compute_signature(temp.path()).expect("signature");
        assert_ne!(before, after);
    }

    #[test]
    fn signature_ignores_gitignored_file() {
        let temp = repo();
        std::fs::write(temp.path().join(".gitignore"), "build/\n").expect("ignore");
        git(temp.path(), &["add", ".gitignore"]);
        git(temp.path(), &["commit", "-q", "-m", "ignore build"]);
        let before = compute_signature(temp.path()).expect("signature");
        std::fs::create_dir_all(temp.path().join("build")).expect("build");
        std::fs::write(temp.path().join("build").join("artifact.o"), "x").expect("artifact");
        let after = compute_signature(temp.path()).expect("signature");
        assert_eq!(before, after);
    }

    #[test]
    fn signature_changes_when_head_moves() {
        let temp = repo();
        let before = compute_signature(temp.path()).expect("signature");
        std::fs::write(temp.path().join("seed.txt"), "seed\nsecond\n").expect("edit");
        git(temp.path(), &["commit", "-aq", "-m", "second"]);
        let after = compute_signature(temp.path()).expect("signature");
        assert_ne!(before, after);
    }

    #[test]
    fn signature_returns_none_outside_git_repo() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(compute_signature(temp.path()).is_none());
    }

    #[test]
    fn dirty_paths_match_git_short_shape() {
        let temp = repo();
        std::fs::write(temp.path().join("seed.txt"), "changed\n").expect("edit");
        std::fs::write(temp.path().join("new.txt"), "new\n").expect("new");
        let dirty = list_dirty_paths(temp.path());
        assert!(dirty.iter().any(|line| line.contains("seed.txt")));
        assert!(dirty.iter().any(|line| line.contains("new.txt")));
    }

    #[test]
    fn drift_error_names_stage_changed_paths_and_escape_hatch() {
        let initial = vec![" M already-dirty.cpp".to_owned()];
        let current = vec![
            " M already-dirty.cpp".to_owned(),
            "?? new-change.cpp".to_owned(),
        ];
        let message = format_drift_error("build", &initial, &current);
        assert!(message.contains("stage=build"));
        assert!(message.contains("#238"));
        assert!(message.contains("+ ?? new-change.cpp"));
        assert!(!message.contains("+  M already-dirty.cpp"));
        assert!(message.contains("--allow-tree-drift"));
    }

    fn repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        git(temp.path(), &["init", "--quiet", "--initial-branch=main"]);
        std::fs::write(temp.path().join("seed.txt"), "seed\n").expect("seed");
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "-q", "-m", "seed"]);
        temp
    }

    fn git(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .expect("git runs");
        assert!(status.success(), "git failed: {args:?}");
    }
}
