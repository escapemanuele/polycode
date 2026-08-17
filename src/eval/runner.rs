use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::app::{
    AppError, ExecutionTarget, ProviderResolver, QuiescentState, RoutedProvider, RoutingPlan,
    RunService,
};
use crate::domain::{ConfigSnapshotId, ModelId, ProviderId, RunId, StageId, WorkflowDefinition};
use crate::store::{ResolvedConfigSnapshot, SequencedEvent};

use super::case::{EvalCase, EvalScorer, ValidationCommand};
use super::result::{
    EVAL_RESULT_SCHEMA_VERSION, EvalProvider, EvalResultError, EvalResultV1, EvalStatus,
    EvalTarget, EvalUsage,
};
use super::scorer::{ScoreInput, ScoredOutcome, ScoringError, score};
use super::suite::{EvalSuite, EvalSuiteError};

const VALIDATION_OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub struct EvalRunOptions {
    pub target: EvalTarget,
    pub repeat: u32,
    pub allow_native_usage: bool,
    pub output: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct EvalRunSummary {
    pub output_directory: PathBuf,
    pub results: Vec<EvalResultV1>,
}

pub struct EvalRunner {
    options: EvalRunOptions,
}

impl EvalRunner {
    /// Creates a validated local evaluation runner.
    ///
    /// # Errors
    /// Rejects zero repetitions or invalid target identifiers.
    pub fn new(options: EvalRunOptions) -> Result<Self, EvalRunnerError> {
        if options.repeat == 0 {
            return Err(EvalRunnerError::InvalidRepetitionCount);
        }
        EvalTarget::new(
            options.target.provider,
            options.target.configured_model.clone(),
        )?;
        Ok(Self { options })
    }

    /// Executes every suite case in fresh isolated runtime data.
    ///
    /// # Errors
    /// Rejects missing native consent/provider readiness, unsafe output reuse, corrupt fixtures,
    /// result persistence failures, or infrastructure setup failures.
    pub fn run(
        &self,
        suite: &EvalSuite,
        mut progress: impl FnMut(&EvalCase, u32, u32),
    ) -> Result<EvalRunSummary, EvalRunnerError> {
        if self.options.target.provider.is_native() && !self.options.allow_native_usage {
            return Err(EvalRunnerError::NativeUsageConsentRequired(
                self.options.target.provider,
            ));
        }
        let preflight_cli_version = preflight(self.options.target.provider)?;
        let output_directory = self
            .options
            .output
            .clone()
            .unwrap_or(default_output_directory(&self.options.target)?);
        std::fs::create_dir_all(&output_directory)?;
        let total = u32::try_from(suite.cases().len())
            .map_err(|_| EvalRunnerError::SuiteTooLarge)?
            .saturating_mul(self.options.repeat);
        let mut ordinal = 0_u32;
        let mut results = Vec::new();
        for repetition in 1..=self.options.repeat {
            for case in suite.cases() {
                ordinal = ordinal.saturating_add(1);
                progress(case, ordinal, total);
                let result = self.run_case(
                    suite,
                    case,
                    repetition,
                    &output_directory,
                    preflight_cli_version.as_deref(),
                )?;
                results.push(result);
            }
        }
        Ok(EvalRunSummary {
            output_directory,
            results,
        })
    }

    fn run_case(
        &self,
        suite: &EvalSuite,
        case: &EvalCase,
        repetition: u32,
        output_root: &Path,
        preflight_cli_version: Option<&str>,
    ) -> Result<EvalResultV1, EvalRunnerError> {
        let repetition_root = output_root
            .join(case.id)
            .join(format!("rep-{repetition:03}"));
        if repetition_root.join("result.json").exists() {
            return Err(EvalRunnerError::ResultAlreadyExists(repetition_root));
        }
        std::fs::create_dir_all(&repetition_root)?;
        let started_at = now();
        let fixture_hash = EvalSuite::fixture_fingerprint(case);
        let mut evidence = CaseEvidence::default();
        let execution = self.execute_case(case, &repetition_root, &mut evidence);
        let finished_at = now();
        let (status, metrics, detail) = match execution {
            Ok(scored) => (
                if scored.passed {
                    EvalStatus::Passed
                } else {
                    EvalStatus::Failed
                },
                Some(scored.metrics),
                scored.detail,
            ),
            Err(CaseFailure::Benchmark(error)) => {
                (EvalStatus::Failed, None, Some(error.to_string()))
            }
            Err(CaseFailure::Infrastructure(error)) => {
                (EvalStatus::InfrastructureFailure, None, Some(error))
            }
        };
        write_evidence(&repetition_root, &evidence)?;
        let result = EvalResultV1 {
            schema_version: EVAL_RESULT_SCHEMA_VERSION,
            suite: suite.name().to_owned(),
            suite_version: suite.version().to_owned(),
            suite_fingerprint: suite.fingerprint().to_owned(),
            case_id: case.id.to_owned(),
            repetition,
            target: self.options.target.clone(),
            confirmed_model: evidence.confirmed_model,
            provider_cli_version: evidence
                .provider_cli_version
                .or_else(|| preflight_cli_version.map(ToOwned::to_owned)),
            role: case.target_role,
            status,
            metrics,
            usage: evidence.usage,
            latency_ms: evidence
                .latency_ms
                .unwrap_or_else(|| duration_ms(started_at, finished_at)),
            artifact_hash: evidence.artifact.as_deref().map(sha256),
            diff_hash: sha256(evidence.diff.as_bytes()),
            fixture_hash,
            synthetic: self.options.target.provider == EvalProvider::Fake,
            started_at,
            finished_at,
            detail,
        };
        result.write(&repetition_root.join("result.json"))?;
        Ok(result)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one transaction-like orchestration path keeps evidence capture ordered around apply"
    )]
    fn execute_case(
        &self,
        case: &EvalCase,
        repetition_root: &Path,
        evidence: &mut CaseEvidence,
    ) -> Result<ScoredOutcome, CaseFailure> {
        let source = repetition_root.join("source");
        materialize_fixture(case, &source).map_err(infrastructure)?;
        initialize_repository(&source).map_err(infrastructure)?;
        let runtime = repetition_root.join("runtime");
        let workflow = WorkflowDefinition::built_in(case.workflow);
        let target_stage = target_stage(&workflow, case).map_err(infrastructure)?;
        let provider_id = ProviderId::new(self.options.target.provider.as_str())
            .map_err(|error| infrastructure(error.to_string()))?;
        let model_id = self
            .options
            .target
            .configured_model
            .clone()
            .map(ModelId::new)
            .transpose()
            .map_err(|error| infrastructure(error.to_string()))?;
        let target = ExecutionTarget::new(provider_id, model_id);
        let config = crate::app::resolve_eval_config(
            case.target_role,
            &target,
            &workflow,
            ConfigSnapshotId::new(format!("eval-config-{}", ulid::Ulid::new()))
                .map_err(|error| infrastructure(error.to_string()))?,
            now(),
        )
        .map_err(|error| infrastructure(error.to_string()))?;
        let resolver = EvalProviderResolver {
            process_root: runtime.join("runs"),
            runner_executable: std::env::current_exe().map_err(infrastructure)?,
        };
        let service = RunService::new(
            runtime.join("polycode.db"),
            runtime.join("worktrees"),
            resolver,
        );
        let report = service
            .start_run_with_config(case.workflow, case.task, &source, &config)
            .map_err(|error| infrastructure(error.to_string()))?;
        let run_id = report.details.id;
        if !matches!(report.outcome, QuiescentState::Completed) {
            return Err(CaseFailure::Infrastructure(format!(
                "run reached unexpected quiescent state {:?}",
                report.outcome
            )));
        }
        let stage_evidence = service
            .stage_execution_evidence(run_id, &target_stage)
            .map_err(|error| infrastructure(error.to_string()))?;
        if stage_evidence.configured_provider != self.options.target.provider.as_str()
            || stage_evidence.configured_model != self.options.target.configured_model
            || stage_evidence.actual_provider.as_deref()
                != Some(self.options.target.provider.as_str())
        {
            return Err(CaseFailure::Infrastructure(
                "candidate stage target identity mismatch".to_owned(),
            ));
        }
        evidence.confirmed_model = stage_evidence.confirmed_model;
        evidence.provider_cli_version = stage_evidence.provider_cli_version;
        evidence.usage = EvalUsage {
            input_units: stage_evidence.usage.input_units,
            output_units: stage_evidence.usage.output_units,
        };
        evidence.latency_ms = stage_evidence
            .started_at
            .zip(stage_evidence.finished_at)
            .map(|(start, finish)| duration_ms(start, finish));
        let diff = service
            .preview_run_diff(run_id)
            .map_err(|error| infrastructure(error.to_string()))?;
        evidence.diff = diff.text;
        evidence.artifact = match service.read_artifact(run_id, &target_stage) {
            Ok(artifact) => Some(artifact.text),
            Err(AppError::ArtifactNotFound { .. })
                if self.options.target.provider == EvalProvider::Fake =>
            {
                None
            }
            Err(error) => return Err(infrastructure(error.to_string())),
        };
        if !matches!(case.scorer, EvalScorer::Implementer { .. })
            && !evidence.diff.trim().is_empty()
        {
            return Err(CaseFailure::Infrastructure(
                "read-only reviewer modified repository".to_owned(),
            ));
        }
        let validation_pass = match case.scorer {
            EvalScorer::Implementer {
                validation,
                require_plan_mismatch: false,
                ..
            } => {
                service
                    .apply_run(run_id)
                    .map_err(|error| infrastructure(error.to_string()))?;
                let validation = run_validation(&source, validation).map_err(infrastructure)?;
                evidence.validation = validation.output;
                Some(validation.passed)
            }
            _ => None,
        };
        score(
            case,
            ScoreInput {
                artifact: evidence.artifact.as_deref(),
                diff: &evidence.diff,
                validation_pass,
            },
        )
        .map_err(CaseFailure::Benchmark)
    }
}

struct EvalProviderResolver {
    process_root: PathBuf,
    runner_executable: PathBuf,
}

impl ProviderResolver for EvalProviderResolver {
    type Provider = RoutedProvider;

    fn resolve_for_run(
        &self,
        run_id: RunId,
        config: &ResolvedConfigSnapshot,
        workflow: &WorkflowDefinition,
        _events: &[SequencedEvent],
    ) -> Result<Self::Provider, AppError> {
        let plan = RoutingPlan::from_snapshot(config, workflow).map_err(|error| {
            if config.schema_version() == 1 {
                AppError::LegacyExecutionConfig(run_id)
            } else {
                error.into()
            }
        })?;
        Ok(RoutedProvider::isolated(
            plan,
            workflow.clone(),
            self.process_root.clone(),
            self.runner_executable.clone(),
        ))
    }
}

#[derive(Default)]
struct CaseEvidence {
    artifact: Option<String>,
    diff: String,
    validation: String,
    usage: EvalUsage,
    latency_ms: Option<u64>,
    confirmed_model: Option<String>,
    provider_cli_version: Option<String>,
}

fn materialize_fixture(case: &EvalCase, destination: &Path) -> Result<(), EvalRunnerError> {
    if destination.exists() {
        return Err(EvalRunnerError::FixtureDestinationExists(
            destination.to_path_buf(),
        ));
    }
    std::fs::create_dir_all(destination)?;
    for file in case.fixture {
        let path = destination.join(file.path);
        let parent = path.parent().ok_or_else(|| {
            EvalRunnerError::FixtureCorrupt(format!("fixture path has no parent: {}", file.path))
        })?;
        std::fs::create_dir_all(parent)?;
        std::fs::write(path, file.contents.as_bytes())?;
    }
    Ok(())
}

fn initialize_repository(path: &Path) -> Result<(), EvalRunnerError> {
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "eval@polycode.invalid"][..],
        &["config", "user.name", "Polycode Eval"][..],
        &["add", "-A"][..],
        &["commit", "-qm", "eval baseline"][..],
    ] {
        let output = Command::new("git").args(args).current_dir(path).output()?;
        if !output.status.success() {
            return Err(EvalRunnerError::Git {
                args: args.join(" "),
                message: bounded_text(&output.stderr),
            });
        }
    }
    Ok(())
}

fn target_stage(
    workflow: &WorkflowDefinition,
    case: &EvalCase,
) -> Result<StageId, EvalRunnerError> {
    let stages = workflow
        .stages()
        .iter()
        .filter(|stage| stage.role() == case.target_role)
        .collect::<Vec<_>>();
    if stages.len() != 1 {
        return Err(EvalRunnerError::FixtureCorrupt(format!(
            "case {} target role occurs {} times",
            case.id,
            stages.len()
        )));
    }
    Ok(stages[0].id().clone())
}

struct ValidationOutcome {
    passed: bool,
    output: String,
}

fn run_validation(
    repository: &Path,
    commands: &[ValidationCommand],
) -> Result<ValidationOutcome, EvalRunnerError> {
    let mut passed = true;
    let mut log = String::new();
    for command in commands {
        let output = Command::new(command.program)
            .args(command.args)
            .current_dir(repository)
            .output()?;
        writeln!(
            log,
            "$ {} {}\nexit: {}\nstdout:\n{}\nstderr:\n{}",
            command.program,
            command.args.join(" "),
            output.status,
            bounded_text(&output.stdout),
            bounded_text(&output.stderr)
        )
        .expect("String write cannot fail");
        passed &= output.status.success();
    }
    Ok(ValidationOutcome {
        passed,
        output: log,
    })
}

fn write_evidence(root: &Path, evidence: &CaseEvidence) -> Result<(), EvalRunnerError> {
    std::fs::write(
        root.join("artifact.md"),
        evidence
            .artifact
            .as_deref()
            .unwrap_or("<!-- candidate produced no artifact -->\n"),
    )?;
    std::fs::write(root.join("diff.patch"), &evidence.diff)?;
    std::fs::write(root.join("validation.txt"), &evidence.validation)?;
    Ok(())
}

fn preflight(provider: EvalProvider) -> Result<Option<String>, EvalRunnerError> {
    match provider {
        EvalProvider::Fake => Ok(Some("deterministic-fake-v1".to_owned())),
        EvalProvider::Claude => {
            let installation = crate::providers::claude::ClaudeInstallation::discover()
                .map_err(|error| EvalRunnerError::ProviderUnavailable(error.to_string()))?;
            if !installation.authenticated() {
                return Err(EvalRunnerError::ProviderUnavailable(
                    "Claude Code is not authenticated".to_owned(),
                ));
            }
            Ok(Some(installation.version().to_owned()))
        }
        EvalProvider::Codex => {
            let installation = crate::providers::codex::CodexInstallation::discover()
                .map_err(|error| EvalRunnerError::ProviderUnavailable(error.to_string()))?;
            if !installation.authenticated() {
                return Err(EvalRunnerError::ProviderUnavailable(
                    "Codex CLI is not authenticated".to_owned(),
                ));
            }
            Ok(Some(installation.version().to_owned()))
        }
    }
}

fn default_output_directory(target: &EvalTarget) -> Result<PathBuf, EvalRunnerError> {
    let base = if let Some(data) =
        std::env::var_os("POLYCODE_DATA_DIR").filter(|value| !value.is_empty())
    {
        PathBuf::from(data)
    } else {
        PathBuf::from(std::env::var_os("HOME").ok_or(EvalRunnerError::OutputDirectoryUnavailable)?)
            .join(".polycode")
    };
    let model = target
        .configured_model
        .as_deref()
        .map_or_else(|| "native-default".to_owned(), sanitize_component);
    Ok(base.join("evals").join(format!(
        "{}-{}-{}-{}",
        now().timestamp_millis(),
        target.provider,
        model,
        ulid::Ulid::new()
    )))
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn bounded_text(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(VALIDATION_OUTPUT_LIMIT);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

fn sha256(bytes: impl AsRef<[u8]>) -> String {
    let digest = Sha256::digest(bytes.as_ref());
    let mut hash = String::with_capacity(64);
    for byte in digest {
        write!(hash, "{byte:02x}").expect("String write cannot fail");
    }
    hash
}

fn duration_ms(start: DateTime<Utc>, finish: DateTime<Utc>) -> u64 {
    u64::try_from(
        finish
            .signed_duration_since(start)
            .num_milliseconds()
            .max(0),
    )
    .unwrap_or(u64::MAX)
}

fn now() -> DateTime<Utc> {
    std::time::SystemTime::now().into()
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err adapters receive owned heterogeneous error values"
)]
fn infrastructure(error: impl ToString) -> CaseFailure {
    CaseFailure::Infrastructure(error.to_string())
}

enum CaseFailure {
    Benchmark(ScoringError),
    Infrastructure(String),
}

#[derive(Debug, Error)]
pub enum EvalRunnerError {
    #[error("repeat must be at least 1")]
    InvalidRepetitionCount,
    #[error("eval suite has too many cases")]
    SuiteTooLarge,
    #[error("native eval provider {0} may consume provider usage; rerun with --allow-native-usage")]
    NativeUsageConsentRequired(EvalProvider),
    #[error("eval provider unavailable: {0}")]
    ProviderUnavailable(String),
    #[error("eval output directory cannot be resolved; set HOME or --out")]
    OutputDirectoryUnavailable,
    #[error("eval result already exists under {0}; choose a new --out path")]
    ResultAlreadyExists(PathBuf),
    #[error("fixture destination already exists: {0}")]
    FixtureDestinationExists(PathBuf),
    #[error("fixture is corrupt: {0}")]
    FixtureCorrupt(String),
    #[error("fixture git command `git {args}` failed: {message}")]
    Git { args: String, message: String },
    #[error(transparent)]
    Suite(#[from] EvalSuiteError),
    #[error(transparent)]
    Result(#[from] EvalResultError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
