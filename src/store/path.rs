use std::ffi::OsString;
use std::path::PathBuf;

use super::StoreError;

const DATABASE_FILE: &str = "polycode.db";

/// Resolves default database path without creating directories or files.
///
/// # Errors
/// Returns [`StoreError::DataPathUnavailable`] when neither override nor home
/// directory is available.
pub fn database_file() -> Result<PathBuf, StoreError> {
    database_file_with(|name| std::env::var_os(name))
}

fn database_file_with(
    mut get_var: impl FnMut(&str) -> Option<OsString>,
) -> Result<PathBuf, StoreError> {
    if let Some(directory) = non_empty(get_var("POLYCODE_DATA_DIR")) {
        return Ok(PathBuf::from(directory).join(DATABASE_FILE));
    }
    let home = non_empty(get_var("HOME")).ok_or(StoreError::DataPathUnavailable)?;
    Ok(PathBuf::from(home).join(".polycode").join(DATABASE_FILE))
}

fn non_empty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_and_resolution_has_no_side_effects() {
        let path = database_file_with(|name| match name {
            "POLYCODE_DATA_DIR" => Some(OsString::from("/tmp/polycode-data")),
            "HOME" => Some(OsString::from("/home/ignored")),
            _ => None,
        })
        .unwrap();
        assert_eq!(path, PathBuf::from("/tmp/polycode-data/polycode.db"));
    }

    #[test]
    fn home_fallback_matches_contract() {
        let path =
            database_file_with(|name| (name == "HOME").then(|| OsString::from("/home/user")))
                .unwrap();
        assert_eq!(path, PathBuf::from("/home/user/.polycode/polycode.db"));
    }
}
