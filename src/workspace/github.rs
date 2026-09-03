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

pub struct GhClient {
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
    /// Uses a specific `gh` executable instead of the one on `PATH`.
    #[must_use]
    pub fn with_executable(executable: impl Into<OsString>) -> Self {
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

/// One pull request named by a task, as parsed from its URL.
///
/// Polycode never requires the pull request to live in the run's repository:
/// a review of a remote pull request is a legitimate run whose evidence
/// arrives over the network. What it does require is that the evidence can
/// actually arrive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestRef {
    pub host: String,
    pub owner: String,
    pub repository: String,
    pub number: u64,
}

impl PullRequestRef {
    /// First `https://<host>/<owner>/<repo>/pull/<number>` mentioned in `task`.
    #[must_use]
    pub fn parse(task: &str) -> Option<Self> {
        task.split_whitespace().find_map(Self::parse_url)
    }

    fn parse_url(candidate: &str) -> Option<Self> {
        let rest = candidate
            .trim_matches(|character: char| matches!(character, '<' | '>' | '(' | ')' | ',' | '.'))
            .strip_prefix("https://")
            .or_else(|| candidate.strip_prefix("http://"))?;
        let mut segments = rest.split('/');
        let host = segments.next()?;
        let owner = segments.next()?;
        let repository = segments.next()?;
        if segments.next()? != "pull" {
            return None;
        }
        let number = segments
            .next()?
            .split(|character: char| !character.is_ascii_digit())
            .next()?
            .parse()
            .ok()?;
        if host.is_empty() || owner.is_empty() || repository.is_empty() {
            return None;
        }
        Some(Self {
            host: host.to_owned(),
            owner: owner.to_owned(),
            repository: repository.to_owned(),
            number,
        })
    }

    #[must_use]
    pub fn url(&self) -> String {
        let Self {
            host,
            owner,
            repository,
            number,
        } = self;
        format!("https://{host}/{owner}/{repository}/pull/{number}")
    }
}

/// Result of asking whether a run's stages could read the pull request at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PullRequestReach {
    /// The pull request was read successfully.
    Reachable,
    /// The host answered, and the answer was "no": bad auth, missing
    /// repository, network failure. The run cannot succeed as written.
    Unreachable(String),
    /// Nothing could be concluded — `gh` is not installed, so a route polycode
    /// cannot see (an MCP provider, a pasted diff) may still work. Absence of
    /// `gh` is not evidence of absent access, so this never blocks a run.
    Unknown,
}

impl GhClient {
    /// Whether `gh` can read `pull_request` right now.
    ///
    /// Runs before a run persists anything, so a pull request behind an
    /// expired login or an unreachable host stops the run at creation instead
    /// of stranding every stage on evidence that never arrives.
    #[must_use]
    pub fn pull_request_reach(&self, pull_request: &PullRequestRef) -> PullRequestReach {
        let PullRequestRef {
            host,
            owner,
            repository,
            number,
        } = pull_request;
        let output = Command::new(&self.executable)
            .args([
                OsString::from("api"),
                OsString::from(format!("repos/{owner}/{repository}/pulls/{number}")),
                OsString::from("--hostname"),
                OsString::from(host),
                OsString::from("--jq"),
                OsString::from(".number"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        match output {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                PullRequestReach::Unknown
            }
            Err(source) => PullRequestReach::Unreachable(format!("failed to run gh: {source}")),
            Ok(output) if output.status.success() => PullRequestReach::Reachable,
            Ok(output) => {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                PullRequestReach::Unreachable(if detail.is_empty() {
                    format!("gh api exited with {}", output.status)
                } else {
                    detail
                })
            }
        }
    }
}

#[cfg(test)]
mod pull_request_ref_tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn parses_the_enterprise_pull_request_url_that_started_run_01m1k8ka() {
        let parsed =
            PullRequestRef::parse("https://github.a8c.com/Automattic/missioncontrol/pull/164318")
                .expect("a pull request URL");
        assert_eq!(
            parsed,
            PullRequestRef {
                host: "github.a8c.com".to_owned(),
                owner: "Automattic".to_owned(),
                repository: "missioncontrol".to_owned(),
                number: 164_318,
            }
        );
        assert_eq!(
            parsed.url(),
            "https://github.a8c.com/Automattic/missioncontrol/pull/164318"
        );
    }

    #[test]
    fn finds_a_pull_request_named_inside_a_longer_task() {
        let parsed = PullRequestRef::parse(
            "Review https://github.com/Automattic/wp-calypso/pull/12#discussion_r1 for regressions",
        )
        .expect("a pull request URL");
        assert_eq!(parsed.repository, "wp-calypso");
        assert_eq!(parsed.number, 12);
    }

    #[test]
    fn tasks_without_a_pull_request_url_are_left_alone() {
        for task in [
            "Fix the flaky scheduler test",
            "https://github.com/Automattic/wp-calypso",
            "https://github.com/Automattic/wp-calypso/issues/12",
            "see pull/12",
        ] {
            assert!(PullRequestRef::parse(task).is_none(), "{task}");
        }
    }

    fn stub_gh(directory: &std::path::Path, body: &str) -> GhClient {
        let script = directory.join("gh");
        fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        GhClient::with_executable(script)
    }

    fn pull_request() -> PullRequestRef {
        PullRequestRef {
            host: "github.a8c.com".to_owned(),
            owner: "Automattic".to_owned(),
            repository: "missioncontrol".to_owned(),
            number: 164_318,
        }
    }

    #[test]
    fn a_readable_pull_request_is_reachable() {
        let temp = tempfile::tempdir().unwrap();
        let gh = stub_gh(temp.path(), "echo 164318");
        assert_eq!(
            gh.pull_request_reach(&pull_request()),
            PullRequestReach::Reachable
        );
    }

    /// The real failure behind run `01M1K8KAJ1HMS47H7WR8YMN2PW`: the host is
    /// only reachable over VPN, and `gh` times out at the TCP dial.
    #[test]
    fn a_host_that_cannot_be_dialled_is_unreachable_with_its_own_words() {
        let temp = tempfile::tempdir().unwrap();
        let gh = stub_gh(
            temp.path(),
            "echo 'Get \"https://github.a8c.com/api/v3/repos/Automattic/missioncontrol/pulls/164318\": dial tcp 192.0.80.209:443: i/o timeout' >&2\nexit 1",
        );
        let PullRequestReach::Unreachable(detail) = gh.pull_request_reach(&pull_request()) else {
            panic!("a timing-out host is not reachable");
        };
        assert!(detail.contains("i/o timeout"), "{detail}");
    }

    /// Absence of `gh` says nothing about access: an MCP provider or a pasted
    /// diff may still carry the pull request, so the run is never blocked on it.
    #[test]
    fn a_missing_gh_concludes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let gh = GhClient::with_executable(temp.path().join("no-such-gh"));
        assert_eq!(
            gh.pull_request_reach(&pull_request()),
            PullRequestReach::Unknown
        );
    }
}
