//! How this particular Polycode was installed, and therefore what an update
//! is allowed to touch.
//!
//! Nothing here guesses destructively: an installation Polycode does not
//! positively recognize as its own is reported as unsupported rather than
//! overwritten. Only a binary Polycode itself installed — recorded in a
//! receipt beside the update cache — is eligible for self-replacement.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Bumped only when older receipts must be discarded rather than migrated.
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;

/// Where this executable came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallSource {
    /// An official release binary this Polycode installed and recorded.
    OfficialBinary,
    /// A `cargo install` destination (`~/.cargo/bin`).
    Cargo,
    /// A third-party package manager's prefix.
    Homebrew,
    /// A `cargo build` / `cargo run` artifact inside a checkout.
    Source,
    /// Somewhere Polycode has no basis for reasoning about.
    Unknown,
}

impl InstallSource {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::OfficialBinary => "official binary",
            Self::Cargo => "cargo install",
            Self::Homebrew => "Homebrew",
            Self::Source => "development/source",
            Self::Unknown => "unrecognized",
        }
    }

    #[must_use]
    pub const fn strategy(self) -> InstallStrategy {
        match self {
            Self::OfficialBinary => InstallStrategy::SelfReplace,
            Self::Cargo => InstallStrategy::ExternalManager {
                manager: "cargo",
                command: "cargo install --git https://github.com/escapemanuele/polycode",
            },
            Self::Homebrew => InstallStrategy::ExternalManager {
                manager: "Homebrew",
                command: "brew upgrade polycode",
            },
            Self::Source => InstallStrategy::Unsupported {
                reason: "This installation is managed from source.",
            },
            Self::Unknown => InstallStrategy::Unsupported {
                reason: "Polycode cannot identify how this executable was installed.",
            },
        }
    }
}

/// What Polycode may do about an available update for this installation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallStrategy {
    /// Download, verify, and atomically replace the executable in place.
    SelfReplace,
    /// Another tool owns this executable; Polycode only names the command.
    ExternalManager {
        manager: &'static str,
        command: &'static str,
    },
    /// Nothing automatic is safe here.
    Unsupported { reason: &'static str },
}

impl InstallStrategy {
    /// Whether Polycode itself may replace the executable.
    #[must_use]
    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::SelfReplace)
    }

    /// One line of honest guidance for a non-automatic installation.
    #[must_use]
    pub fn guidance(self) -> String {
        match self {
            Self::SelfReplace => "Automatic installation is supported.".to_owned(),
            Self::ExternalManager { manager, command } => {
                format!("{manager} manages this installation — update it with `{command}`.")
            }
            Self::Unsupported { reason } => {
                format!("{reason} Automatic installation is unavailable for this build.")
            }
        }
    }
}

/// Proof that Polycode installed the executable at a specific path. Written
/// only by a successful self-update, and only ever consulted for the path it
/// names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReceipt {
    pub schema_version: u32,
    pub executable: PathBuf,
    pub version: String,
    pub asset: String,
    pub installed_at: DateTime<Utc>,
}

impl InstallReceipt {
    #[must_use]
    pub fn new(
        executable: PathBuf,
        version: impl Into<String>,
        asset: impl Into<String>,
        installed_at: DateTime<Utc>,
    ) -> Self {
        Self {
            schema_version: RECEIPT_SCHEMA_VERSION,
            executable,
            version: version.into(),
            asset: asset.into(),
            installed_at,
        }
    }
}

/// Reads the receipt, treating every failure as "no receipt".
#[must_use]
pub fn load_receipt(path: &Path) -> Option<InstallReceipt> {
    let raw = std::fs::read_to_string(path).ok()?;
    let receipt: InstallReceipt = serde_json::from_str(&raw).ok()?;
    (receipt.schema_version == RECEIPT_SCHEMA_VERSION).then_some(receipt)
}

/// Best-effort persistence; a receipt that cannot be written only costs the
/// next process its automatic-update eligibility.
pub fn store_receipt(path: &Path, receipt: &InstallReceipt) {
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(receipt)?)
    };
    if let Err(error) = write() {
        tracing::debug!(%error, "install receipt not persisted");
    }
}

/// Classifies the running executable, using the receipt beside the update
/// cache as the only positive evidence of an official installation.
///
/// # Errors
/// Returns an error only when the current executable path cannot be resolved.
pub fn detect_install_source() -> anyhow::Result<InstallSource> {
    let executable = std::env::current_exe()?;
    let receipt = crate::store::install_receipt_file()
        .ok()
        .and_then(|path| load_receipt(&path));
    Ok(classify(&executable, receipt.as_ref()))
}

/// Pure classification, so every branch is testable without installing
/// anything.
#[must_use]
pub fn classify(executable: &Path, receipt: Option<&InstallReceipt>) -> InstallSource {
    // A checkout artifact is never official, whatever a stale receipt claims.
    if is_cargo_target(executable) {
        return InstallSource::Source;
    }
    if receipt.is_some_and(|receipt| same_path(&receipt.executable, executable)) {
        return InstallSource::OfficialBinary;
    }
    if executable.parent().is_some_and(is_cargo_bin) {
        return InstallSource::Cargo;
    }
    if is_package_manager_prefix(executable) {
        return InstallSource::Homebrew;
    }
    InstallSource::Unknown
}

/// Compares paths after resolving symlinks where the filesystem allows it, so
/// a receipt still matches an executable reached through a link.
fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn is_cargo_target(executable: &Path) -> bool {
    let components: Vec<_> = executable
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    components
        .windows(2)
        .any(|pair| pair[0] == "target" && matches!(pair[1].as_str(), "debug" | "release"))
}

fn is_cargo_bin(parent: &Path) -> bool {
    let mut components = parent.components().rev();
    let last = components
        .next()
        .map(|component| component.as_os_str().to_owned());
    let previous = components
        .next()
        .map(|component| component.as_os_str().to_owned());
    last.is_some_and(|last| last == "bin") && previous.is_some_and(|previous| previous == ".cargo")
}

fn is_package_manager_prefix(executable: &Path) -> bool {
    executable.components().any(|component| {
        matches!(
            component.as_os_str().to_string_lossy().as_ref(),
            "Cellar" | "homebrew" | "linuxbrew"
        )
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;
    use tempfile::TempDir;

    use super::*;

    fn receipt(path: &str) -> InstallReceipt {
        InstallReceipt::new(
            PathBuf::from(path),
            "0.2.0",
            "polycode-aarch64-apple-darwin",
            Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0)
                .single()
                .unwrap(),
        )
    }

    #[test]
    fn a_checkout_build_is_always_a_source_install() {
        for path in [
            "/Users/e/Code/polycode/target/debug/polycode",
            "/Users/e/Code/polycode/target/release/polycode",
        ] {
            assert_eq!(
                classify(Path::new(path), None),
                InstallSource::Source,
                "{path}"
            );
        }
    }

    #[test]
    fn a_stale_receipt_never_promotes_a_checkout_build() {
        let path = "/Users/e/Code/polycode/target/release/polycode";
        assert_eq!(
            classify(Path::new(path), Some(&receipt(path))),
            InstallSource::Source,
            "a development build is never treated as an official installation"
        );
    }

    #[test]
    fn only_a_matching_receipt_marks_an_official_binary() {
        let installed = "/usr/local/bin/polycode";
        assert_eq!(
            classify(Path::new(installed), Some(&receipt(installed))),
            InstallSource::OfficialBinary
        );
        assert_eq!(
            classify(Path::new(installed), Some(&receipt("/opt/other/polycode"))),
            InstallSource::Unknown,
            "a receipt for a different path proves nothing about this one"
        );
        assert_eq!(classify(Path::new(installed), None), InstallSource::Unknown);
    }

    #[test]
    fn cargo_and_package_manager_prefixes_are_recognized() {
        assert_eq!(
            classify(Path::new("/Users/e/.cargo/bin/polycode"), None),
            InstallSource::Cargo
        );
        assert_eq!(
            classify(Path::new("/opt/homebrew/bin/polycode"), None),
            InstallSource::Homebrew
        );
        assert_eq!(
            classify(
                Path::new("/usr/local/Cellar/polycode/0.1.0/bin/polycode"),
                None
            ),
            InstallSource::Homebrew
        );
    }

    #[test]
    fn only_an_official_binary_is_automatically_updatable() {
        assert!(InstallSource::OfficialBinary.strategy().is_automatic());
        for source in [
            InstallSource::Cargo,
            InstallSource::Homebrew,
            InstallSource::Source,
            InstallSource::Unknown,
        ] {
            assert!(
                !source.strategy().is_automatic(),
                "{source:?} must never be replaced automatically"
            );
            assert!(!source.strategy().guidance().is_empty());
        }
    }

    #[test]
    fn guidance_names_the_owning_tool_without_assuming_a_checkout() {
        let cargo = InstallSource::Cargo.strategy().guidance();
        assert!(cargo.contains("cargo install"));
        let source = InstallSource::Source.strategy().guidance();
        assert!(source.contains("managed from source"));
        assert!(
            !source.contains("git pull") && !source.contains('~'),
            "no checkout location is ever assumed"
        );
    }

    #[test]
    fn receipts_round_trip_and_reject_foreign_schemas() {
        let fixture = TempDir::new().unwrap();
        let path = fixture.path().join("install.json");
        let entry = receipt("/usr/local/bin/polycode");
        store_receipt(&path, &entry);
        assert_eq!(load_receipt(&path).unwrap(), entry);

        std::fs::write(&path, r#"{"schema_version":99,"executable":"/x","version":"1.0.0","asset":"a","installed_at":"2026-08-22T12:00:00Z"}"#).unwrap();
        assert!(load_receipt(&path).is_none());
        std::fs::write(&path, "not json").unwrap();
        assert!(load_receipt(&path).is_none());
    }
}
