//! Where a workspace's verification commands come from.
//!
//! The repository owns its own definition of "verified": a `[verify]` table
//! in `<worktree>/.polycode.toml` when it has one, otherwise a guess from the
//! build files present. Polycode never invents commands beyond that guess,
//! and it says which of the three it used in the artifact, so a green stage
//! can always be read back to what was actually checked.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::VerifyError;

/// The per-repository configuration file, read from the run's worktree so a
/// change to it made by the run itself is what gets verified.
pub const CONFIG_FILE: &str = ".polycode.toml";

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
    /// The `[verify]` table of `.polycode.toml`.
    ConfigFile,
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
pub(crate) fn plan_for(worktree: &Path) -> Result<VerifyPlan, VerifyError> {
    let config_path = worktree.join(CONFIG_FILE);
    if config_path.is_file() {
        let text = std::fs::read_to_string(&config_path)
            .map_err(|error| VerifyError::Config(format!("{CONFIG_FILE}: {error}")))?;
        let file: ConfigFile = toml::from_str(&text)
            .map_err(|error| VerifyError::Config(format!("{CONFIG_FILE}: {}", error.message())))?;
        if let Some(table) = file.verify {
            return plan_from_table(table);
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

fn plan_from_table(table: VerifyTable) -> Result<VerifyPlan, VerifyError> {
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
        source: CommandSource::ConfigFile,
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

        let plan = plan_for(dir.path()).unwrap();

        assert_eq!(plan.commands, ["cargo fmt --check", "cargo test"]);
        assert_eq!(plan.timeout, Duration::from_secs(60));
        assert_eq!(plan.source, CommandSource::ConfigFile);
    }

    #[test]
    fn a_config_file_without_a_verify_table_falls_back_to_detection() {
        let dir = worktree();
        std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), "[other]\nkey = 1\n").unwrap();

        let plan = plan_for(dir.path()).unwrap();

        assert_eq!(plan.commands, ["npm test"]);
        assert_eq!(plan.source, CommandSource::Detected("package.json"));
        assert_eq!(plan.timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn the_first_recognised_build_file_wins() {
        let dir = worktree();
        std::fs::write(dir.path().join("go.mod"), "module x\n").unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool]\n").unwrap();

        let plan = plan_for(dir.path()).unwrap();

        assert_eq!(plan.commands, ["pytest"]);
        assert_eq!(plan.source, CommandSource::Detected("pyproject.toml"));
    }

    #[test]
    fn nothing_recognised_means_an_empty_plan_not_an_error() {
        let dir = worktree();

        let plan = plan_for(dir.path()).unwrap();

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
            let error = plan_for(dir.path()).unwrap_err();
            assert!(
                matches!(&error, VerifyError::Config(message) if message.starts_with(CONFIG_FILE)),
                "{text:?} produced {error:?}"
            );
        }
    }
}
