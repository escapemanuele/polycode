use std::path::Path;

use super::command::{Git, os, text_output};
use super::GitError;

/// Returns the fetch URL of `remote` as configured for the repository owning
/// `path`, or `None` when no such remote exists.
///
/// Asked of a worktree path rather than a [`super::GitRepository`] because
/// remotes are shared repository configuration: a managed worktree sees
/// exactly the remotes of the checkout it was created from.
pub(crate) fn remote_url(git: &Git, path: &Path, remote: &str) -> Result<Option<String>, GitError> {
    let output = git.output(path, &[os("remote"), os("get-url"), os(remote)], &[])?;
    if output.status.success() {
        return Ok(Some(text_output(output)?));
    }
    // Git reports a missing remote as exit 2 today and exit 128 historically —
    // but 128 is also its generic fatal code, so only the message decides.
    let missing = String::from_utf8_lossy(&output.stderr)
        .to_ascii_lowercase()
        .contains("no such remote");
    if missing {
        Ok(None)
    } else {
        Err(output.into_failure())
    }
}

/// Pushes `branch` to `remote`, creating or updating the remote branch and
/// recording it as upstream.
///
/// Never force-pushes: a remote branch that diverged from what Polycode owns
/// is somebody else's work, and the resulting Git error is the right outcome.
///
/// Credential prompts read the terminal directly, past the nulled stdin, so a
/// push with no usable credentials would hang a publish thread forever under
/// the raw-mode interface. `GIT_TERMINAL_PROMPT=0` turns that hang into an
/// error the operator can read.
pub(crate) fn push_branch(
    git: &Git,
    path: &Path,
    remote: &str,
    branch: &str,
) -> Result<(), GitError> {
    let reference = format!("refs/heads/{branch}");
    git.checked_with(
        path,
        &[
            os("push"),
            os("--set-upstream"),
            os(remote),
            os(format!("{reference}:{reference}")),
        ],
        &[(os("GIT_TERMINAL_PROMPT"), os("0"))],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    fn git_cmd(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn fixture() -> (TempDir, PathBuf, PathBuf) {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git_cmd(&repo, &["init", "-b", "main"]);
        git_cmd(&repo, &["config", "user.name", "Polycode Test"]);
        git_cmd(&repo, &["config", "user.email", "polycode@example.invalid"]);
        git_cmd(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join("README.md"), "base\n").unwrap();
        git_cmd(&repo, &["add", "-A"]);
        git_cmd(&repo, &["commit", "-m", "base"]);
        let origin = temp.path().join("origin.git");
        git_cmd(temp.path(), &["init", "--bare", "origin.git"]);
        (temp, repo, origin)
    }

    #[test]
    fn a_missing_remote_is_an_answer_not_an_error() {
        let (_temp, repo, origin) = fixture();
        let git = Git::default();
        assert_eq!(remote_url(&git, &repo, "origin").unwrap(), None);
        git_cmd(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        assert_eq!(
            remote_url(&git, &repo, "origin").unwrap().as_deref(),
            origin.to_str()
        );
    }

    #[test]
    fn push_creates_and_updates_the_remote_branch_with_upstream() {
        let (_temp, repo, origin) = fixture();
        git_cmd(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_cmd(&repo, &["checkout", "-b", "polycode/run-1"]);
        fs::write(repo.join("work.txt"), "one\n").unwrap();
        git_cmd(&repo, &["add", "-A"]);
        git_cmd(&repo, &["commit", "-m", "one"]);
        let git = Git::default();

        push_branch(&git, &repo, "origin", "polycode/run-1").unwrap();
        let pushed = git_cmd(&origin, &["rev-parse", "refs/heads/polycode/run-1"]);
        assert_eq!(pushed, git_cmd(&repo, &["rev-parse", "HEAD"]));
        assert_eq!(
            git_cmd(
                &repo,
                &["rev-parse", "--abbrev-ref", "polycode/run-1@{upstream}"]
            )
            .trim(),
            "origin/polycode/run-1"
        );

        fs::write(repo.join("work.txt"), "two\n").unwrap();
        git_cmd(&repo, &["add", "-A"]);
        git_cmd(&repo, &["commit", "-m", "two"]);
        push_branch(&git, &repo, "origin", "polycode/run-1").unwrap();
        assert_eq!(
            git_cmd(&origin, &["rev-parse", "refs/heads/polycode/run-1"]),
            git_cmd(&repo, &["rev-parse", "HEAD"])
        );
    }
}
