//! The repository's own standing tool allowlist.
//!
//! Every Claude invocation runs with `--permission-mode dontAsk`, so anything
//! Polycode has not granted is denied outright. Derived grants only ever come
//! from a denial the user then approved, which means the first attempt at a
//! repository's own build, lint and test commands is always denied, always
//! costs a round trip through attention, and — when the command is compound —
//! can never be granted exactly at all.
//!
//! A repository answers that once, in its own `.polycode.toml`, with the rules
//! it is willing to hand every run:
//!
//! ```toml
//! [permissions]
//! allow = ["Bash(yarn jest:*)", "Bash(yarn lint:css:*)", "mcp__linear-server"]
//! ```
//!
//! The strings are native Claude Code `--allowedTools` rules, passed through
//! verbatim: this file is the repository's explicit intent, not a guess
//! Polycode derives from a denial, so it is not re-parsed or widened here. The
//! one thing refused is a rule that grants everything, because a permission
//! model that can be turned off in a config file is not one.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::providers::repo_config;

/// The per-repository configuration file, shared with `[verify]`. Read from
/// the run's worktree so a change the run itself makes is what takes effect,
/// falling back to the repository the worktree was cut from; `repo_config`
/// owns that order and the reason for it.
pub(crate) use repo_config::CONFIG_FILE;

/// Rules that would grant every tool, refused however they are spelled.
const BLANKET_RULES: [&str; 2] = ["*", "Bash(*)"];

#[derive(Debug, Error)]
pub enum PermissionsConfigError {
    #[error("{CONFIG_FILE}: {0}")]
    Unreadable(String),
    #[error("{CONFIG_FILE}: [permissions] allow rule {index} is empty")]
    EmptyRule { index: usize },
    #[error(
        "{CONFIG_FILE}: [permissions] allow rule {index} grants every tool ({rule}); list the commands the repository actually needs"
    )]
    BlanketRule { index: usize, rule: String },
}

/// The whole file. Other tables are tolerated so a repository that configures
/// `[verify]` and nothing else still reads cleanly here.
#[derive(Deserialize)]
struct ConfigFile {
    permissions: Option<PermissionsTable>,
}

/// Unknown keys are rejected: a misspelt `allow` that silently granted nothing
/// would look exactly like the denial storm this table exists to end.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionsTable {
    allow: Vec<String>,
}

/// Reads the standing allowlist for one run.
///
/// A missing file, or a file without a `[permissions]` table, is an empty
/// allowlist rather than an error — most repositories have neither.
///
/// # Errors
/// A `.polycode.toml` that exists but cannot be read or parsed, an empty rule,
/// or a rule that grants every tool.
pub(crate) fn allow_rules(
    worktree: &Path,
    source_repo: Option<&Path>,
) -> Result<BTreeSet<String>, PermissionsConfigError> {
    let Some(found) = repo_config::locate(worktree, source_repo) else {
        return Ok(BTreeSet::new());
    };
    let text = std::fs::read_to_string(&found.path)
        .map_err(|error| PermissionsConfigError::Unreadable(error.to_string()))?;
    let file: ConfigFile = toml::from_str(&text)
        .map_err(|error| PermissionsConfigError::Unreadable(error.message().to_owned()))?;
    let Some(table) = file.permissions else {
        return Ok(BTreeSet::new());
    };
    let mut rules = BTreeSet::new();
    for (index, rule) in table.allow.iter().enumerate() {
        let rule = rule.trim();
        if rule.is_empty() {
            return Err(PermissionsConfigError::EmptyRule { index: index + 1 });
        }
        if BLANKET_RULES.contains(&rule) {
            return Err(PermissionsConfigError::BlanketRule {
                index: index + 1,
                rule: rule.to_owned(),
            });
        }
        rules.insert(rule.to_owned());
    }
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree_with(contents: &str) -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("temp worktree");
        std::fs::write(directory.path().join(CONFIG_FILE), contents).expect("write config");
        directory
    }

    #[test]
    fn missing_file_and_missing_table_both_grant_nothing() {
        let empty = tempfile::tempdir().expect("temp worktree");
        assert!(allow_rules(empty.path(), None).expect("no file").is_empty());

        let other_table = worktree_with("[verify]\ncommands = [\"cargo test\"]\n");
        assert!(
            allow_rules(other_table.path(), None)
                .expect("verify-only config")
                .is_empty()
        );
    }

    #[test]
    fn the_source_repository_grants_for_a_repository_polycode_cannot_commit_to() {
        let worktree = tempfile::tempdir().expect("temp worktree");
        let source = worktree_with("[permissions]\nallow = [\"Bash(yarn jest:*)\"]\n");

        let rules = allow_rules(worktree.path(), Some(source.path())).expect("source-repo grants");

        assert_eq!(
            rules.into_iter().collect::<Vec<_>>(),
            vec!["Bash(yarn jest:*)".to_owned()]
        );
    }

    #[test]
    fn the_worktree_file_still_wins_over_the_source_repository() {
        let worktree = worktree_with("[permissions]\nallow = [\"Bash(cargo test:*)\"]\n");
        let source = worktree_with("[permissions]\nallow = [\"Bash(yarn jest:*)\"]\n");

        let rules = allow_rules(worktree.path(), Some(source.path())).expect("worktree grants");

        assert_eq!(
            rules.into_iter().collect::<Vec<_>>(),
            vec!["Bash(cargo test:*)".to_owned()]
        );
    }

    #[test]
    fn rules_reach_the_command_verbatim_and_deduplicated() {
        let directory = worktree_with(
            "[permissions]\nallow = [\"Bash(yarn jest:*)\", \" mcp__linear-server \", \"Bash(yarn jest:*)\"]\n",
        );
        let rules = allow_rules(directory.path(), None).expect("valid allowlist");
        assert_eq!(
            rules.into_iter().collect::<Vec<_>>(),
            vec![
                "Bash(yarn jest:*)".to_owned(),
                "mcp__linear-server".to_owned()
            ]
        );
    }

    #[test]
    fn blanket_and_empty_rules_are_refused_by_position() {
        let blanket = worktree_with("[permissions]\nallow = [\"Bash(yarn jest:*)\", \"*\"]\n");
        let error = allow_rules(blanket.path(), None).expect_err("blanket rule");
        assert!(error.to_string().contains("rule 2"), "{error}");

        let empty = worktree_with("[permissions]\nallow = [\"  \"]\n");
        let error = allow_rules(empty.path(), None).expect_err("empty rule");
        assert!(error.to_string().contains("rule 1"), "{error}");
    }

    #[test]
    fn misspelt_key_fails_instead_of_granting_nothing() {
        let directory = worktree_with("[permissions]\nallowed = [\"Bash(yarn jest:*)\"]\n");
        assert!(matches!(
            allow_rules(directory.path(), None),
            Err(PermissionsConfigError::Unreadable(_))
        ));
    }
}
