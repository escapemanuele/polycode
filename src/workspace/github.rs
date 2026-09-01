//! Minimal GitHub CLI (`gh`) boundary for opening pull requests.
//!
//! Publishing is push-first: the branch reaches the remote through Git alone,
//! and everything here is the optional last step that turns a pushed branch
//! into a pull request. Every failure is therefore reported as an outcome the
//! caller can present, never as an error that undoes the push.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Stdio};

/// Why a pull request could not be opened, in words meant for the operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhUnavailable(pub String);

pub(crate) struct GhClient {
    executable: OsString,
}

impl Default for GhClient {
    fn default() -> Self {
        Self {
            executable: OsString::from("gh"),
        }
    }
}

impl GhClient {
    #[cfg(test)]
    pub(crate) fn with_executable(executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    /// Returns the URL of an open pull request whose head is `branch`, if one
    /// exists.
    pub(crate) fn existing_pull_request(
        &self,
        cwd: &Path,
        branch: &str,
    ) -> Result<Option<String>, GhUnavailable> {
        let stdout = self.run(
            cwd,
            &[
                OsStr::new("pr"),
                OsStr::new("list"),
                OsStr::new("--head"),
                OsStr::new(branch),
                OsStr::new("--json"),
                OsStr::new("url"),
                OsStr::new("--jq"),
                OsStr::new(".[].url"),
            ],
        )?;
        Ok(stdout.lines().next().map(str::to_owned))
    }

    /// Opens a pull request from `branch` against the repository's default
    /// branch and returns its URL.
    pub(crate) fn create_pull_request(
        &self,
        cwd: &Path,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String, GhUnavailable> {
        let stdout = self.run(
            cwd,
            &[
                OsStr::new("pr"),
                OsStr::new("create"),
                OsStr::new("--head"),
                OsStr::new(branch),
                OsStr::new("--title"),
                OsStr::new(title),
                OsStr::new("--body"),
                OsStr::new(body),
            ],
        )?;
        // gh prints the new pull request's URL as the last stdout line.
        stdout
            .lines()
            .rev()
            .find(|line| line.starts_with("https://"))
            .map(str::to_owned)
            .ok_or_else(|| {
                GhUnavailable(format!(
                    "gh pr create reported success without a pull request URL: {stdout}"
                ))
            })
    }

    fn run(&self, cwd: &Path, args: &[&OsStr]) -> Result<String, GhUnavailable> {
        let output = Command::new(&self.executable)
            .current_dir(cwd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|source| match source.kind() {
                std::io::ErrorKind::NotFound => GhUnavailable(
                    "GitHub CLI (gh) is not installed — the branch is pushed; open the pull request manually or install gh".to_owned(),
                ),
                _ => GhUnavailable(format!("failed to run gh: {source}")),
            })?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(GhUnavailable(format!(
                "gh {} failed: {}",
                args.first()
                    .and_then(|arg| arg.to_str())
                    .unwrap_or("command"),
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }
}
