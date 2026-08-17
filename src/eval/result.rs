use std::fmt;
use std::io::Write as _;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{ModelId, ProviderId, Role};

pub const EVAL_RESULT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalProvider {
    Claude,
    Codex,
    Fake,
}

impl EvalProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Fake => "fake",
        }
    }

    #[must_use]
    pub const fn is_native(self) -> bool {
        matches!(self, Self::Claude | Self::Codex)
    }
}

impl fmt::Display for EvalProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for EvalProvider {
    type Error = EvalResultError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "fake" => Ok(Self::Fake),
            other => Err(EvalResultError::InvalidTarget(format!(
                "unsupported provider {other:?}; expected claude, codex, or fake"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalTarget {
    pub provider: EvalProvider,
    pub configured_model: Option<String>,
}

impl EvalTarget {
    /// Creates and validates one explicit provider/model candidate.
    ///
    /// # Errors
    /// Rejects invalid provider or model identifiers.
    pub fn new(
        provider: EvalProvider,
        configured_model: Option<String>,
    ) -> Result<Self, EvalResultError> {
        ProviderId::new(provider.as_str())
            .map_err(|error| EvalResultError::InvalidTarget(error.to_string()))?;
        if let Some(model) = &configured_model {
            ModelId::new(model.clone())
                .map_err(|error| EvalResultError::InvalidTarget(error.to_string()))?;
        }
        Ok(Self {
            provider,
            configured_model,
        })
    }

    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{} / {}",
            self.provider,
            self.configured_model.as_deref().unwrap_or("native_default")
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalStatus {
    Passed,
    Failed,
    InfrastructureFailure,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalUsage {
    pub input_units: u64,
    pub output_units: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImplementerMetrics {
    pub behavioral_pass: bool,
    pub scope_pass: bool,
    pub plan_mismatch_behavior: Option<bool>,
    pub validation_pass: Option<bool>,
    pub unexpected_files: Vec<String>,
    pub deletions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityMetrics {
    pub defects_found: u32,
    pub defects_total: u32,
    pub recall: f64,
    pub false_positives: u32,
    pub must_fix_false_positives: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecMetrics {
    pub missing_found: u32,
    pub missing_total: u32,
    pub wrong_found: u32,
    pub wrong_total: u32,
    pub unrequested_found: u32,
    pub unrequested_total: u32,
    pub false_positives: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "values", rename_all = "snake_case")]
pub enum EvalMetrics {
    Implementer(ImplementerMetrics),
    CodeQualityReviewer(QualityMetrics),
    SpecReviewer(SpecMetrics),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalResultV1 {
    pub schema_version: u32,
    pub suite: String,
    pub suite_version: String,
    pub suite_fingerprint: String,
    pub case_id: String,
    pub repetition: u32,
    pub target: EvalTarget,
    pub confirmed_model: Option<String>,
    pub provider_cli_version: Option<String>,
    pub role: Role,
    pub status: EvalStatus,
    pub metrics: Option<EvalMetrics>,
    pub usage: EvalUsage,
    pub latency_ms: u64,
    pub artifact_hash: Option<String>,
    pub diff_hash: String,
    pub fixture_hash: String,
    pub synthetic: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub detail: Option<String>,
}

impl EvalResultV1 {
    /// Decodes and validates schema V1 without accepting future versions.
    ///
    /// # Errors
    /// Rejects malformed JSON, unsupported schemas, or invariant violations.
    pub fn from_json(bytes: &[u8]) -> Result<Self, EvalResultError> {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or(EvalResultError::MissingSchemaVersion)?;
        if version != u64::from(EVAL_RESULT_SCHEMA_VERSION) {
            return Err(EvalResultError::UnsupportedSchema(version));
        }
        let result: Self = serde_json::from_value(value)?;
        result.validate()?;
        Ok(result)
    }

    /// Writes one immutable result file atomically.
    ///
    /// # Errors
    /// Rejects invalid results, existing destinations, or filesystem failures.
    pub fn write(&self, path: &Path) -> Result<(), EvalResultError> {
        self.validate()?;
        let parent = path.parent().ok_or(EvalResultError::InvalidPath)?;
        std::fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut temporary, self)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(path)
            .map_err(|error| EvalResultError::Io(error.error))?;
        Ok(())
    }

    fn validate(&self) -> Result<(), EvalResultError> {
        if self.schema_version != EVAL_RESULT_SCHEMA_VERSION {
            return Err(EvalResultError::UnsupportedSchema(u64::from(
                self.schema_version,
            )));
        }
        EvalTarget::new(self.target.provider, self.target.configured_model.clone())?;
        if let Some(model) = &self.confirmed_model {
            ModelId::new(model.clone())
                .map_err(|error| EvalResultError::InvalidTarget(error.to_string()))?;
        }
        if self.suite.trim().is_empty()
            || self.suite_version.trim().is_empty()
            || self.case_id.trim().is_empty()
            || self.repetition == 0
            || self.finished_at < self.started_at
            || !valid_hash(&self.suite_fingerprint)
            || !valid_hash(&self.diff_hash)
            || !valid_hash(&self.fixture_hash)
            || self
                .artifact_hash
                .as_ref()
                .is_some_and(|hash| !valid_hash(hash))
            || self.synthetic != (self.target.provider == EvalProvider::Fake)
        {
            return Err(EvalResultError::InvalidResult);
        }
        if self.status == EvalStatus::Passed && self.metrics.is_none() {
            return Err(EvalResultError::InvalidResult);
        }
        if self.status == EvalStatus::InfrastructureFailure && self.metrics.is_some() {
            return Err(EvalResultError::InvalidResult);
        }
        match &self.metrics {
            Some(EvalMetrics::Implementer(_)) if self.role == Role::Implementer => {}
            Some(EvalMetrics::CodeQualityReviewer(metrics))
                if self.role == Role::CodeQualityReviewer
                    && metrics.defects_found <= metrics.defects_total
                    && metrics.must_fix_false_positives <= metrics.false_positives
                    && metrics.recall.is_finite()
                    && (0.0..=1.0).contains(&metrics.recall) => {}
            Some(EvalMetrics::SpecReviewer(metrics))
                if self.role == Role::SpecReviewer
                    && metrics.missing_found <= metrics.missing_total
                    && metrics.wrong_found <= metrics.wrong_total
                    && metrics.unrequested_found <= metrics.unrequested_total => {}
            None => {}
            Some(_) => return Err(EvalResultError::InvalidResult),
        }
        Ok(())
    }
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Error)]
pub enum EvalResultError {
    #[error("eval result has no schema_version")]
    MissingSchemaVersion,
    #[error("unsupported eval result schema {0}")]
    UnsupportedSchema(u64),
    #[error("invalid eval target: {0}")]
    InvalidTarget(String),
    #[error("eval result violates schema invariants")]
    InvalidResult,
    #[error("eval result path has no parent")]
    InvalidPath,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    fn result() -> EvalResultV1 {
        let at = Utc
            .with_ymd_and_hms(2026, 8, 17, 12, 0, 0)
            .single()
            .unwrap();
        EvalResultV1 {
            schema_version: EVAL_RESULT_SCHEMA_VERSION,
            suite: "role_core".to_owned(),
            suite_version: "role_core_v1".to_owned(),
            suite_fingerprint: "a".repeat(64),
            case_id: "quality_clean".to_owned(),
            repetition: 1,
            target: EvalTarget::new(EvalProvider::Codex, None).unwrap(),
            confirmed_model: None,
            provider_cli_version: Some("fixture-1".to_owned()),
            role: Role::CodeQualityReviewer,
            status: EvalStatus::Passed,
            metrics: Some(EvalMetrics::CodeQualityReviewer(QualityMetrics {
                defects_found: 0,
                defects_total: 0,
                recall: 1.0,
                false_positives: 0,
                must_fix_false_positives: 0,
            })),
            usage: EvalUsage::default(),
            latency_ms: 25,
            artifact_hash: Some("b".repeat(64)),
            diff_hash: "c".repeat(64),
            fixture_hash: "d".repeat(64),
            synthetic: false,
            started_at: at,
            finished_at: at,
            detail: None,
        }
    }

    #[test]
    fn result_round_trip_preserves_native_default_and_confirmed_model_distinction() {
        let original = result();
        let bytes = serde_json::to_vec(&original).unwrap();
        let restored = EvalResultV1::from_json(&bytes).unwrap();
        assert_eq!(restored, original);
        assert_eq!(restored.target.configured_model, None);
        assert_eq!(restored.confirmed_model, None);
    }

    #[test]
    fn future_result_schema_is_rejected_before_decode() {
        let mut value = serde_json::to_value(result()).unwrap();
        value["schema_version"] = serde_json::json!(2);
        assert!(matches!(
            EvalResultV1::from_json(&serde_json::to_vec(&value).unwrap()),
            Err(EvalResultError::UnsupportedSchema(2))
        ));
    }

    #[test]
    fn impossible_or_role_mismatched_metrics_are_rejected() {
        let mut invalid_recall = result();
        if let Some(EvalMetrics::CodeQualityReviewer(metrics)) = &mut invalid_recall.metrics {
            metrics.recall = 1.5;
        }
        assert!(matches!(
            EvalResultV1::from_json(&serde_json::to_vec(&invalid_recall).unwrap()),
            Err(EvalResultError::InvalidResult)
        ));

        let mut wrong_role = result();
        wrong_role.role = Role::Implementer;
        assert!(matches!(
            EvalResultV1::from_json(&serde_json::to_vec(&wrong_role).unwrap()),
            Err(EvalResultError::InvalidResult)
        ));
    }
}
