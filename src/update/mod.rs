//! Update discovery for official Polycode releases.
//!
//! Update state is application preference/cache state, not run state: it lives
//! as a small JSON document under the Polycode data directory and never enters
//! the `SQLite` run store. Discovery is best-effort by construction — every
//! network, parse, or filesystem problem degrades to
//! [`UpdateStatus::Unavailable`] so that nothing here can make Polycode fail to
//! start.

mod cache;
mod install;
mod release;

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use semver::Version;

pub use cache::{CACHE_SCHEMA_VERSION, UpdateCache};
pub use install::{
    InstallReceipt, InstallSource, InstallStrategy, RECEIPT_SCHEMA_VERSION, classify,
    detect_install_source, load_receipt, store_receipt,
};
pub use release::{GitHubReleases, Release, ReleaseAsset, ReleaseError, ReleaseSource};

/// The one canonical application version: whatever this build was compiled as.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Official upstream repository consulted for release metadata.
pub const OFFICIAL_REPOSITORY: &str = "escapemanuele/polycode";

/// How long a successful check stays authoritative before Polycode asks
/// GitHub again. One check per day keeps unauthenticated API use well inside
/// GitHub's limits without ever requiring a token.
pub const CACHE_TTL: TimeDelta = TimeDelta::hours(24);

/// Environment opt-out. Polycode's configuration file is resolved but not yet
/// parsed, so an environment variable is the only existing knob that can be
/// honored without inventing a preferences system.
pub const DISABLE_ENVIRONMENT_VARIABLE: &str = "POLYCODE_DISABLE_UPDATE_CHECK";

/// What a completed check concluded. Version comparison only: whether the
/// installation *can* be updated automatically is
/// [`InstallStrategy`], a separate and orthogonal question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    /// This build is at or ahead of the newest official stable release.
    Current,
    /// A newer official stable release exists.
    Available(Box<UpdateInfo>),
    /// No conclusion could be reached: checks are disabled, the network was
    /// unreachable, GitHub rate-limited us, or the response was unusable.
    Unavailable,
}

impl UpdateStatus {
    #[must_use]
    pub const fn available(&self) -> Option<&UpdateInfo> {
        match self {
            Self::Available(info) => Some(info),
            Self::Current | Self::Unavailable => None,
        }
    }
}

/// Only facts taken from official release metadata, plus this build's version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateInfo {
    pub current_version: Version,
    pub available_version: Version,
    pub tag: String,
    pub release_url: String,
    pub published_at: Option<DateTime<Utc>>,
}

/// Resolves the running build's version once, for every caller.
///
/// # Errors
/// Returns an error only when the compiled package version is not semver,
/// which a release build cannot be.
pub fn current_version() -> Result<Version, semver::Error> {
    Version::parse(CURRENT_VERSION)
}

/// Whether automatic checking is switched off for this process.
#[must_use]
pub fn checks_disabled() -> bool {
    disabled_with(|name| std::env::var(name).ok())
}

fn disabled_with(mut get_var: impl FnMut(&str) -> Option<String>) -> bool {
    get_var(DISABLE_ENVIRONMENT_VARIABLE).is_some_and(|value| {
        let value = value.trim().to_ascii_lowercase();
        !matches!(value.as_str(), "" | "0" | "false" | "no")
    })
}

/// Reads cached release metadata, refreshes it when stale, and compares
/// versions. Construction never performs I/O.
pub struct UpdateService<S> {
    source: S,
    cache_path: PathBuf,
    current: Version,
    ttl: TimeDelta,
    disabled: bool,
}

impl UpdateService<GitHubReleases> {
    /// Builds the service every normal Polycode invocation uses.
    ///
    /// # Errors
    /// Returns an error when the data directory or the compiled version cannot
    /// be resolved. Network reachability is never an error here.
    pub fn from_environment() -> anyhow::Result<Self> {
        Ok(Self {
            source: GitHubReleases::new(OFFICIAL_REPOSITORY, Duration::from_secs(5)),
            cache_path: crate::store::update_cache_file()?,
            current: current_version()?,
            ttl: CACHE_TTL,
            disabled: checks_disabled(),
        })
    }
}

impl<S: ReleaseSource> UpdateService<S> {
    #[must_use]
    pub const fn new(source: S, cache_path: PathBuf, current: Version) -> Self {
        Self {
            source,
            cache_path,
            current,
            ttl: CACHE_TTL,
            disabled: false,
        }
    }

    #[must_use]
    pub const fn with_disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    #[must_use]
    pub const fn with_ttl(mut self, ttl: TimeDelta) -> Self {
        self.ttl = ttl;
        self
    }

    #[must_use]
    pub const fn current(&self) -> &Version {
        &self.current
    }

    /// The status a normal invocation uses: a fresh cache answers offline, a
    /// stale or unreadable one triggers exactly one bounded network check.
    #[must_use]
    pub fn status(&self, now: DateTime<Utc>) -> UpdateStatus {
        if self.disabled {
            return UpdateStatus::Unavailable;
        }
        match cache::load(&self.cache_path) {
            Some(cached) if cached.is_fresh(now, self.ttl) => self.compare(&cached),
            // A missing, stale, or malformed cache all mean the same thing:
            // there is nothing trustworthy to answer with, so refresh.
            _ => self.refresh(now),
        }
    }

    /// Forces one check regardless of cache age. Still honors the opt-out.
    #[must_use]
    pub fn check_now(&self, now: DateTime<Utc>) -> UpdateStatus {
        if self.disabled {
            return UpdateStatus::Unavailable;
        }
        self.refresh(now)
    }

    fn refresh(&self, now: DateTime<Utc>) -> UpdateStatus {
        match self.source.latest_stable() {
            Ok(latest) => {
                let cached = UpdateCache::from_release(now, latest.as_ref());
                // A cache we cannot write only costs one extra check later.
                cache::store(&self.cache_path, &cached);
                self.compare(&cached)
            }
            Err(error) => {
                tracing::debug!(%error, "update check unavailable");
                UpdateStatus::Unavailable
            }
        }
    }

    /// A completed check that found nothing newer — including a repository
    /// that has published no stable release at all — means this build is not
    /// behind anything. `Unavailable` stays reserved for checks that reached
    /// no conclusion.
    fn compare(&self, cached: &UpdateCache) -> UpdateStatus {
        cached
            .update_over(&self.current)
            .map_or(UpdateStatus::Current, |info| {
                UpdateStatus::Available(Box::new(info))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use chrono::TimeZone as _;
    use tempfile::TempDir;

    use super::*;

    /// Injectable source that records how often it was consulted, so cache
    /// behavior can be asserted without any network.
    struct FakeSource {
        result: fn() -> Result<Option<Release>, ReleaseError>,
        calls: Cell<usize>,
    }

    impl FakeSource {
        fn new(result: fn() -> Result<Option<Release>, ReleaseError>) -> Self {
            Self {
                result,
                calls: Cell::new(0),
            }
        }
    }

    impl ReleaseSource for FakeSource {
        fn latest_stable(&self) -> Result<Option<Release>, ReleaseError> {
            self.calls.set(self.calls.get() + 1);
            (self.result)()
        }
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "matches the ReleaseSource return type the fake must produce"
    )]
    fn release(version: &str) -> Result<Option<Release>, ReleaseError> {
        Ok(Some(Release {
            version: Version::parse(version).unwrap(),
            tag: format!("v{version}"),
            url: "https://example.invalid/r".to_owned(),
            published_at: None,
            assets: Vec::new(),
        }))
    }

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 22, hour, 0, 0)
            .single()
            .unwrap()
    }

    fn service(fixture: &TempDir, source: FakeSource, current: &str) -> UpdateService<FakeSource> {
        UpdateService::new(
            source,
            fixture.path().join("update.json"),
            Version::parse(current).unwrap(),
        )
    }

    #[test]
    fn a_newer_release_is_offered_and_an_equal_or_older_one_is_not() {
        let fixture = TempDir::new().unwrap();
        let status =
            service(&fixture, FakeSource::new(|| release("0.2.0")), "0.1.0").check_now(at(12));
        let info = status.available().expect("0.2.0 is newer than 0.1.0");
        assert_eq!(info.available_version.to_string(), "0.2.0");
        assert_eq!(info.current_version.to_string(), "0.1.0");
        assert_eq!(info.tag, "v0.2.0");

        for remote in ["0.1.0", "0.0.9"] {
            let fixture = TempDir::new().unwrap();
            let status = match remote {
                "0.1.0" => service(&fixture, FakeSource::new(|| release("0.1.0")), "0.1.0"),
                _ => service(&fixture, FakeSource::new(|| release("0.0.9")), "0.1.0"),
            }
            .check_now(at(12));
            assert_eq!(status, UpdateStatus::Current, "remote {remote}");
        }
    }

    #[test]
    fn a_repository_without_stable_releases_is_not_behind_anything() {
        let fixture = TempDir::new().unwrap();
        let status = service(&fixture, FakeSource::new(|| Ok(None)), "0.1.0").check_now(at(12));
        assert_eq!(status, UpdateStatus::Current);
    }

    #[test]
    fn a_fresh_cache_answers_without_touching_the_network() {
        let fixture = TempDir::new().unwrap();
        let first = service(&fixture, FakeSource::new(|| release("0.2.0")), "0.1.0");
        assert!(first.status(at(12)).available().is_some());
        assert_eq!(first.source.calls.get(), 1);

        let second = service(&fixture, FakeSource::new(|| release("0.9.0")), "0.1.0");
        let status = second.status(at(20));
        assert_eq!(second.source.calls.get(), 0, "the fresh cache answered");
        assert_eq!(
            status.available().unwrap().available_version.to_string(),
            "0.2.0",
            "the cached release, not a new fetch"
        );
    }

    #[test]
    fn a_stale_cache_triggers_exactly_one_refresh() {
        let fixture = TempDir::new().unwrap();
        let _ = service(&fixture, FakeSource::new(|| release("0.2.0")), "0.1.0").status(at(12));
        let later = service(&fixture, FakeSource::new(|| release("0.3.0")), "0.1.0");
        let status = later.status(at(12) + TimeDelta::hours(25));
        assert_eq!(later.source.calls.get(), 1);
        assert_eq!(
            status.available().unwrap().available_version.to_string(),
            "0.3.0"
        );
    }

    #[test]
    fn a_malformed_cache_refreshes_instead_of_failing() {
        let fixture = TempDir::new().unwrap();
        std::fs::write(fixture.path().join("update.json"), "{ not json").unwrap();
        let service = service(&fixture, FakeSource::new(|| release("0.2.0")), "0.1.0");
        assert!(service.status(at(12)).available().is_some());
        assert_eq!(service.source.calls.get(), 1);
    }

    #[test]
    fn every_network_failure_is_boring() {
        for failure in [
            (|| Err(ReleaseError::Unreachable("dns".to_owned()))) as fn() -> _,
            || Err(ReleaseError::Unreachable("timed out".to_owned())),
            || Err(ReleaseError::RateLimited),
            || {
                Err(ReleaseError::Malformed(
                    "unexpected end of input".to_owned(),
                ))
            },
        ] {
            let fixture = TempDir::new().unwrap();
            let service = service(&fixture, FakeSource::new(failure), "0.1.0");
            assert_eq!(
                service.status(at(12)),
                UpdateStatus::Unavailable,
                "a failed check never becomes an update or an error"
            );
            assert!(
                !fixture.path().join("update.json").exists(),
                "a failed check never poisons the cache"
            );
        }
    }

    #[test]
    fn disabled_checks_never_reach_the_network() {
        let fixture = TempDir::new().unwrap();
        let service =
            service(&fixture, FakeSource::new(|| release("0.2.0")), "0.1.0").with_disabled(true);
        assert_eq!(service.status(at(12)), UpdateStatus::Unavailable);
        assert_eq!(service.check_now(at(12)), UpdateStatus::Unavailable);
        assert_eq!(service.source.calls.get(), 0);
    }

    #[test]
    fn a_custom_ttl_is_honored() {
        let fixture = TempDir::new().unwrap();
        let _ = service(&fixture, FakeSource::new(|| release("0.2.0")), "0.1.0").status(at(12));
        let short = service(&fixture, FakeSource::new(|| release("0.4.0")), "0.1.0")
            .with_ttl(TimeDelta::minutes(1));
        let _ = short.status(at(12) + TimeDelta::minutes(2));
        assert_eq!(short.source.calls.get(), 1);
    }

    #[test]
    fn compiled_version_is_valid_semver_and_matches_the_package() {
        assert_eq!(current_version().unwrap().to_string(), CURRENT_VERSION);
        assert_eq!(CURRENT_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn opt_out_accepts_only_meaningful_values() {
        assert!(disabled_with(|_| Some("1".to_owned())));
        assert!(disabled_with(|_| Some("true".to_owned())));
        assert!(!disabled_with(|_| None));
        assert!(!disabled_with(|_| Some("0".to_owned())));
        assert!(!disabled_with(|_| Some("false".to_owned())));
        assert!(!disabled_with(|_| Some("  ".to_owned())));
    }
}
