//! Where a workspace's verification commands come from.
//!
//! The repository owns its own definition of "verified": a `[verify]` table
//! in its `.polycode.toml` when it has one, otherwise a guess from the build
//! files present. Polycode never invents commands beyond that guess, and it
//! says which of the three it used in the artifact — naming the checkout the
//! file came from — so a green stage can always be read back to what was
//! actually checked.
//!
//! Which checkout answers is `repo_config`'s question, not this module's.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::VerifyError;
use crate::providers::repo_config::{self, ConfigOrigin};

pub use repo_config::CONFIG_FILE;

/// How long one command may run before it is killed and counted as failed.
/// Generous because a full test suite is the common case; the artifact
/// names the limit when it bites.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1800);

/// Build files Polycode recognises when no `[verify]` table exists, with the
/// command each implies. Order matters: the first present wins, so a
/// polyglot repository verifies with one toolchain rather than all of them.
const DETECTION_RULES: [(&str, &str); 5] = [
    ("Cargo.toml", "cargo test"),
    ("package.json", "npm test"),
    ("pyproject.toml", "pytest"),
    ("pytest.ini", "pytest"),
    ("go.mod", "go test ./..."),
];

/// Which of the three sources answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommandSource {
    /// The `[verify]` table of `.polycode.toml`; carries the checkout the
    /// file was read from, because the two are configured differently and a
    /// reader chasing a command needs to know which file to open.
    ConfigFile(ConfigOrigin),
    /// A build file Polycode recognises; carries the file that matched.
    Detected(&'static str),
    /// Neither a table nor a recognised build file.
    Nothing,
}

/// The commands one verification pass will run, in order, and its limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifyPlan {
    pub commands: Vec<String>,
    pub timeout: Duration,
    pub source: CommandSource,
}

/// The file as a whole. Other tables are tolerated, not rejected: the file
/// is documented as the future home of more per-repository settings, and a
/// stage must not fail because a table it does not read appeared.
#[derive(Deserialize)]
struct ConfigFile {
    verify: Option<VerifyTable>,
}

/// The one table this stage reads. Unknown keys inside it are rejected,
/// because a misspelt `commands` silently verifying nothing is exactly the
/// failure a verification stage exists to prevent.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyTable {
    commands: Vec<String>,
    timeout_seconds: Option<u64>,
}

/// Resolves the plan for one worktree.
///
/// # Errors
/// A `.polycode.toml` that exists but cannot be read or parsed, or a
/// `[verify]` table with an empty command or a zero timeout. The stage
/// fails on these rather than falling back to detection: a broken
/// configuration is a finding, not an absence.
pub(crate) fn plan_for(
    worktree: &Path,
    source_repo: Option<&Path>,
) -> Result<VerifyPlan, VerifyError> {
    if let Some(found) = repo_config::locate(worktree, source_repo) {
        let text = std::fs::read_to_string(&found.path)
            .map_err(|error| VerifyError::Config(format!("{CONFIG_FILE}: {error}")))?;
        let file: ConfigFile = toml::from_str(&text)
            .map_err(|error| VerifyError::Config(format!("{CONFIG_FILE}: {}", error.message())))?;
        if let Some(table) = file.verify {
            return plan_from_table(table, found.origin);
        }
    }
    Ok(DETECTION_RULES
        .iter()
        .find(|(marker, _)| worktree.join(marker).is_file())
        .map_or(
            VerifyPlan {
                commands: Vec::new(),
                timeout: DEFAULT_TIMEOUT,
                source: CommandSource::Nothing,
            },
            |(marker, command)| VerifyPlan {
                commands: vec![(*command).to_owned()],
                timeout: DEFAULT_TIMEOUT,
                source: CommandSource::Detected(marker),
            },
        ))
}

fn plan_from_table(table: VerifyTable, origin: ConfigOrigin) -> Result<VerifyPlan, VerifyError> {
    for (index, command) in table.commands.iter().enumerate() {
        if command.split_whitespace().next().is_none() {
            return Err(VerifyError::Config(format!(
                "{CONFIG_FILE}: [verify] command {} is empty",
                index + 1
            )));
        }
    }
    let timeout = match table.timeout_seconds {
        None => DEFAULT_TIMEOUT,
        Some(0) => {
            return Err(VerifyError::Config(format!(
                "{CONFIG_FILE}: [verify] timeout_seconds must be positive"
            )));
        }
        Some(seconds) => Duration::from_secs(seconds),
    };
    Ok(VerifyPlan {
        commands: table.commands,
        timeout,
        source: CommandSource::ConfigFile(origin),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_verify_table_wins_over_every_build_file() {
        let dir = worktree();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "[verify]\ncommands = [\"cargo fmt --check\", \"cargo test\"]\ntimeout_seconds = 60\n",
        )
        .unwrap();

        let plan = plan_for(dir.path(), None).unwrap();

        assert_eq!(plan.commands, ["cargo fmt --check", "cargo test"]);
        assert_eq!(plan.timeout, Duration::from_secs(60));
        assert_eq!(
            plan.source,
            CommandSource::ConfigFile(ConfigOrigin::Worktree)
        );
    }

    #[test]
    fn the_source_repository_configures_a_repository_polycode_cannot_commit_to() {
        let dir = worktree();
        let source = worktree();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(source.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(
            source.path().join(CONFIG_FILE),
            "[verify]\ncommands = [\"yarn test-client client/dashboard\"]\n",
        )
        .unwrap();

        let plan = plan_for(dir.path(), Some(source.path())).unwrap();

        // Detection would have said `npm test` here; the source repository's
        // file is what stops that from being the answer forever.
        assert_eq!(plan.commands, ["yarn test-client client/dashboard"]);
        assert_eq!(
            plan.source,
            CommandSource::ConfigFile(ConfigOrigin::SourceRepo)
        );
    }

    #[test]
    fn the_worktree_file_still_wins_over_the_source_repository() {
        let dir = worktree();
        let source = worktree();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "[verify]\ncommands = [\"from the worktree\"]\n",
        )
        .unwrap();
        std::fs::write(
            source.path().join(CONFIG_FILE),
            "[verify]\ncommands = [\"from the source repo\"]\n",
        )
        .unwrap();

        let plan = plan_for(dir.path(), Some(source.path())).unwrap();

        assert_eq!(plan.commands, ["from the worktree"]);
        assert_eq!(
            plan.source,
            CommandSource::ConfigFile(ConfigOrigin::Worktree)
        );
    }

    #[test]
    fn a_worktree_file_without_a_verify_table_does_not_reach_past_itself() {
        let dir = worktree();
        let source = worktree();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), "[permissions]\nallow = []\n").unwrap();
        std::fs::write(
            source.path().join(CONFIG_FILE),
            "[verify]\ncommands = [\"from the source repo\"]\n",
        )
        .unwrap();

        let plan = plan_for(dir.path(), Some(source.path())).unwrap();

        // One file answers, not two merged: the worktree's file is the
        // repository's current word on every table, including the ones it
        // leaves out.
        assert_eq!(plan.commands, ["npm test"]);
        assert_eq!(plan.source, CommandSource::Detected("package.json"));
    }

    #[test]
    fn a_config_file_without_a_verify_table_falls_back_to_detection() {
        let dir = worktree();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), "[other]\nkey = 1\n").unwrap();

        let plan = plan_for(dir.path(), None).unwrap();

        assert_eq!(plan.commands, ["npm test"]);
        assert_eq!(plan.source, CommandSource::Detected("package.json"));
        assert_eq!(plan.timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn the_first_recognised_build_file_wins() {
        let dir = worktree();
        std::fs::write(dir.path().join("go.mod"), "module x\n").unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool]\n").unwrap();

        let plan = plan_for(dir.path(), None).unwrap();

        assert_eq!(plan.commands, ["pytest"]);
        assert_eq!(plan.source, CommandSource::Detected("pyproject.toml"));
    }

    #[test]
    fn nothing_recognised_means_an_empty_plan_not_an_error() {
        let dir = worktree();

        let plan = plan_for(dir.path(), None).unwrap();

        assert!(plan.commands.is_empty());
        assert_eq!(plan.source, CommandSource::Nothing);
    }

    #[test]
    fn malformed_toml_empty_commands_and_zero_timeouts_are_configuration_errors() {
        let dir = worktree();
        for text in [
            "[verify\ncommands = 1\n",
            "[verify]\ncommands = [\"cargo test\", \"   \"]\n",
            "[verify]\ncommands = [\"cargo test\"]\ntimeout_seconds = 0\n",
            "[verify]\ncommand = [\"cargo test\"]\n",
        ] {
            std::fs::write(dir.path().join(CONFIG_FILE), text).unwrap();
            let error = plan_for(dir.path(), None).unwrap_err();
            assert!(
                matches!(&error, VerifyError::Config(message) if message.starts_with(CONFIG_FILE)),
                "{text:?} produced {error:?}"
            );
        }
    }
}
