use std::fmt::Write as _;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::domain::{
    ArtifactKind, ArtifactStatus, AttentionKind, AttentionRequestId, AttentionStatus,
    DomainEventKind, Role, RunId, RunStatus, StageId, StageKind, StageStatus, WorkflowKind,
};
use crate::process::{ManagedProcessId, OutputStream, ProcessManager, TmuxBackend};
use crate::store::{RunRevision, SequencedEvent, SqliteStore, StoreError};
use crate::workspace::WorkspaceStatus;

use super::AppError;
use super::RoutingPlan;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunListItem {
    pub id: RunId,
    pub workflow: WorkflowKind,
    pub status: RunStatus,
    pub task_summary: String,
    pub repository: Option<PathBuf>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageSummary {
    pub id: StageId,
    pub kind: StageKind,
    pub role: Role,
    pub status: StageStatus,
    pub configured_provider: String,
    pub configured_model: Option<String>,
    pub actual_provider: Option<String>,
    pub actual_model: Option<String>,
    pub provider_session_record: Option<String>,
    pub native_session: Option<String>,
    pub provider_session_status: Option<String>,
    pub process_status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteSummary {
    pub role: Role,
    pub configured_provider: String,
    pub configured_model: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttentionSummary {
    pub id: AttentionRequestId,
    pub stage_id: StageId,
    pub kind: AttentionKind,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageSummary {
    pub input_units: u64,
    pub output_units: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunDetails {
    pub id: RunId,
    pub task: Option<String>,
    pub workflow: WorkflowKind,
    pub status: RunStatus,
    pub repository: Option<PathBuf>,
    pub workspace_status: Option<WorkspaceStatus>,
    pub base_commit: Option<String>,
    pub profile: String,
    pub profile_version: String,
    pub routes: Vec<RouteSummary>,
    pub revision: RunRevision,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub stages: Vec<StageSummary>,
    pub attention: Vec<AttentionSummary>,
    pub usage: UsageSummary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedEvent {
    pub sequence: u64,
    pub stage_id: Option<StageId>,
    pub kind: DomainEventKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactSummary {
    pub stage_id: StageId,
    pub kind: ArtifactKind,
    pub status: ArtifactStatus,
    pub attempt: u32,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub content_size: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactView {
    pub summary: ArtifactSummary,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFileSummary {
    pub path: String,
    pub binary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunDiffPreview {
    pub text: String,
    pub changed_files: Vec<ChangedFileSummary>,
    pub total_bytes: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessLogStream {
    pub text: String,
    pub total_bytes: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessLogView {
    pub process_id: ManagedProcessId,
    pub process_status: String,
    pub stdout: ProcessLogStream,
    pub stderr: ProcessLogStream,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageExecutionEvidence {
    pub stage_id: StageId,
    pub configured_provider: String,
    pub configured_model: Option<String>,
    pub actual_provider: Option<String>,
    pub confirmed_model: Option<String>,
    pub provider_cli_version: Option<String>,
    pub usage: UsageSummary,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

pub(crate) fn list(store: &SqliteStore) -> Result<Vec<RunListItem>, AppError> {
    Ok(store
        .list_runs()?
        .into_iter()
        .map(|run| RunListItem {
            id: run.id,
            workflow: run.workflow,
            status: run.status,
            task_summary: task_summary(run.task.as_deref()),
            repository: run.repository_path.map(PathBuf::from),
            updated_at: run.updated_at,
        })
        .collect())
}

#[allow(
    clippy::too_many_lines,
    reason = "one query assembly keeps configured and actual stage projections aligned"
)]
pub(crate) fn inspect(store: &mut SqliteStore, run_id: RunId) -> Result<RunDetails, AppError> {
    let loaded = store.load_run(run_id)?;
    let input = store.load_run_input(run_id)?;
    let workspace = store.load_workspace(run_id)?;
    let events = store.load_events(run_id)?;
    let plan = match RoutingPlan::from_snapshot(&loaded.config_snapshot, loaded.run.workflow()) {
        Ok(plan) => Some(plan),
        Err(_) if loaded.config_snapshot.schema_version() == 1 => None,
        Err(error) => return Err(error.into()),
    };
    let usage = usage_summary(&events);
    let sessions = store.list_provider_sessions(run_id)?;
    let mut routes = plan
        .iter()
        .flat_map(RoutingPlan::routes)
        .map(|(role, route)| RouteSummary {
            role,
            configured_provider: route.target().provider_id().to_string(),
            configured_model: route.target().model_id().map(ToString::to_string),
            reason: route.reason().to_owned(),
        })
        .collect::<Vec<_>>();
    routes.sort_by_key(|route| role_order(route.role));
    let mut stages = Vec::new();
    for stage in loaded.run.stages() {
        let route = plan.as_ref().and_then(|plan| plan.route(stage.role()));
        let session = sessions
            .iter()
            .filter(|session| session.stage_id() == stage.id())
            .max_by_key(|session| session.attempt());
        let started = events.iter().rev().find_map(|event| {
            if event.event.stage_id() != Some(stage.id()) {
                return None;
            }
            match event.event.kind() {
                DomainEventKind::ProviderStarted {
                    provider_id,
                    model_id,
                    ..
                } => Some((
                    provider_id.to_string(),
                    model_id.as_ref().map(ToString::to_string),
                )),
                _ => None,
            }
        });
        let process_status = session
            .and_then(crate::providers::ProviderSessionRecord::current_process_id)
            .map(|process_id| store.load_managed_process(process_id))
            .transpose()?
            .map(|process| process.status().as_str().to_owned());
        stages.push(StageSummary {
            id: stage.id().clone(),
            kind: stage.kind(),
            role: stage.role(),
            status: stage.status(),
            configured_provider: route.map_or_else(
                || "unavailable".to_owned(),
                |route| route.target().provider_id().to_string(),
            ),
            configured_model: route
                .and_then(|route| route.target().model_id())
                .map(ToString::to_string),
            actual_provider: session
                .map(|session| session.provider_id().to_string())
                .or_else(|| started.as_ref().map(|(provider, _)| provider.clone())),
            actual_model: session
                .and_then(crate::providers::ProviderSessionRecord::model_id)
                .map(ToString::to_string)
                .or_else(|| started.and_then(|(_, model)| model)),
            provider_session_record: session.map(|session| session.id().to_string()),
            native_session: session
                .and_then(crate::providers::ProviderSessionRecord::native_session_id)
                .map(ToString::to_string),
            provider_session_status: session.map(|session| session.status().as_str().to_owned()),
            process_status,
        });
    }
    Ok(RunDetails {
        id: loaded.run.id(),
        task: input.map(|input| input.task().to_owned()),
        workflow: loaded.run.workflow_kind(),
        status: loaded.run.status(),
        repository: workspace
            .as_ref()
            .map(|workspace| workspace.source_repo_path().to_path_buf()),
        workspace_status: workspace
            .as_ref()
            .map(crate::workspace::RunWorkspace::status),
        base_commit: workspace
            .as_ref()
            .map(|workspace| workspace.base_commit().to_owned()),
        profile: plan.as_ref().map_or_else(
            || {
                loaded
                    .config_snapshot
                    .payload()
                    .get("profile")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unavailable")
                    .to_owned()
            },
            |plan| plan.profile().to_owned(),
        ),
        profile_version: plan.as_ref().map_or_else(
            || "unavailable".to_owned(),
            |plan| plan.profile_version().to_owned(),
        ),
        routes,
        revision: loaded.revision,
        created_at: *loaded.run.created_at(),
        updated_at: *loaded.run.updated_at(),
        stages,
        attention: loaded
            .run
            .attention_requests()
            .iter()
            .filter(|request| request.status() == &AttentionStatus::Pending)
            .map(|request| AttentionSummary {
                id: request.id(),
                stage_id: request.stage_id().clone(),
                kind: request.kind(),
                summary: request.summary().to_owned(),
            })
            .collect(),
        usage,
    })
}

pub(crate) fn list_artifacts(
    store: &SqliteStore,
    run_id: RunId,
) -> Result<Vec<ArtifactSummary>, AppError> {
    Ok(store
        .list_artifacts(run_id)?
        .into_iter()
        .map(|artifact| artifact_summary(&artifact))
        .collect())
}

pub(crate) fn read_artifact(
    store: &SqliteStore,
    run_id: RunId,
    stage_id: &StageId,
) -> Result<ArtifactView, AppError> {
    let artifact = store
        .list_artifacts(run_id)?
        .into_iter()
        .filter(|artifact| artifact.metadata().stage_id() == stage_id)
        .max_by_key(crate::providers::ArtifactRecord::attempt)
        .ok_or_else(|| AppError::ArtifactNotFound {
            run_id,
            stage_id: stage_id.clone(),
        })?;
    let bytes = std::fs::read(artifact.path()).map_err(StoreError::Io)?;
    let size_matches = u64::try_from(bytes.len()) == Ok(artifact.content_size());
    let mut hash = String::with_capacity(64);
    for byte in Sha256::digest(&bytes) {
        write!(hash, "{byte:02x}").expect("writing to String cannot fail");
    }
    if !size_matches || hash != artifact.content_hash() {
        return Err(StoreError::ArtifactIntegrity(artifact.path().to_path_buf()).into());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| StoreError::ArtifactIntegrity(artifact.path().to_path_buf()))?;
    Ok(ArtifactView {
        summary: artifact_summary(&artifact),
        text,
    })
}

pub(crate) fn process_log_tail(
    store: &SqliteStore,
    manager: &ProcessManager<TmuxBackend>,
    run_id: RunId,
    stage_id: &StageId,
    max_bytes: usize,
) -> Result<ProcessLogView, AppError> {
    let process = store
        .list_managed_processes(run_id)?
        .into_iter()
        .filter(|process| process.stage_id() == stage_id)
        .max_by_key(|process| {
            (
                process.attempt(),
                process.invocation(),
                *process.created_at(),
            )
        })
        .ok_or_else(|| AppError::ProcessLogNotFound {
            run_id,
            stage_id: stage_id.clone(),
        })?;
    let (stdout, stdout_total, stdout_truncated) =
        manager.read_output_tail(store, process.id(), OutputStream::Stdout, max_bytes)?;
    let (stderr, stderr_total, stderr_truncated) =
        manager.read_output_tail(store, process.id(), OutputStream::Stderr, max_bytes)?;
    Ok(ProcessLogView {
        process_id: process.id(),
        process_status: process.status().as_str().to_owned(),
        stdout: ProcessLogStream {
            text: String::from_utf8_lossy(stdout.bytes()).into_owned(),
            total_bytes: stdout_total,
            truncated: stdout_truncated,
        },
        stderr: ProcessLogStream {
            text: String::from_utf8_lossy(stderr.bytes()).into_owned(),
            total_bytes: stderr_total,
            truncated: stderr_truncated,
        },
    })
}

pub(crate) fn stage_execution_evidence(
    store: &mut SqliteStore,
    run_id: RunId,
    stage_id: &StageId,
) -> Result<StageExecutionEvidence, AppError> {
    let loaded = store.load_run(run_id)?;
    let stage = loaded
        .run
        .stage(stage_id)
        .ok_or_else(|| AppError::StageNotFound {
            run_id,
            stage_id: stage_id.clone(),
        })?;
    let plan = RoutingPlan::from_snapshot(&loaded.config_snapshot, loaded.run.workflow())?;
    let route = plan
        .route(stage.role())
        .ok_or(super::RoutingError::MissingRoleRoute(stage.role()))?;
    let events = store.load_events(run_id)?;
    let mut usage = UsageSummary::default();
    let mut started_at = None;
    let mut finished_at = None;
    let mut started_target = None;
    for event in events
        .iter()
        .filter(|event| event.event.stage_id() == Some(stage_id))
    {
        match event.event.kind() {
            DomainEventKind::ProviderStarted {
                provider_id,
                model_id,
                ..
            } => {
                started_at.get_or_insert(*event.event.occurred_at());
                started_target = Some((
                    provider_id.to_string(),
                    model_id.as_ref().map(ToString::to_string),
                ));
            }
            DomainEventKind::ProviderUsageUpdated {
                input_units,
                output_units,
                ..
            } => {
                usage.input_units = usage.input_units.saturating_add(*input_units);
                usage.output_units = usage.output_units.saturating_add(*output_units);
            }
            DomainEventKind::ProviderCompleted { .. } | DomainEventKind::ProviderFailed { .. } => {
                finished_at = Some(*event.event.occurred_at());
            }
            _ => {}
        }
    }
    let session = store
        .list_provider_sessions(run_id)?
        .into_iter()
        .filter(|session| session.stage_id() == stage_id)
        .max_by_key(crate::providers::ProviderSessionRecord::attempt);
    Ok(StageExecutionEvidence {
        stage_id: stage_id.clone(),
        configured_provider: route.target().provider_id().to_string(),
        configured_model: route.target().model_id().map(ToString::to_string),
        actual_provider: session
            .as_ref()
            .map(|session| session.provider_id().to_string())
            .or_else(|| {
                started_target
                    .as_ref()
                    .map(|(provider, _)| provider.clone())
            }),
        confirmed_model: session
            .as_ref()
            .and_then(crate::providers::ProviderSessionRecord::model_id)
            .map(ToString::to_string)
            .or_else(|| started_target.and_then(|(_, model)| model)),
        provider_cli_version: session
            .as_ref()
            .and_then(crate::providers::ProviderSessionRecord::cli_version)
            .map(ToOwned::to_owned),
        usage,
        started_at,
        finished_at,
    })
}

fn artifact_summary(artifact: &crate::providers::ArtifactRecord) -> ArtifactSummary {
    ArtifactSummary {
        stage_id: artifact.metadata().stage_id().clone(),
        kind: artifact.metadata().kind(),
        status: artifact.metadata().status(),
        attempt: artifact.attempt(),
        provider: artifact.metadata().provider_id().map(ToString::to_string),
        model: artifact.metadata().model_id().map(ToString::to_string),
        content_size: artifact.content_size(),
        created_at: *artifact.metadata().created_at(),
    }
}

pub(crate) fn summarize_diff(text: String, total_bytes: u64, truncated: bool) -> RunDiffPreview {
    let mut changed_files = Vec::<ChangedFileSummary>::new();
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("diff --git a/") {
            let path = path
                .split_once(" b/")
                .map_or(path, |(_, destination)| destination)
                .to_owned();
            if !changed_files.iter().any(|file| file.path == path) {
                changed_files.push(ChangedFileSummary {
                    path,
                    binary: false,
                });
            }
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            if let Some(file) = changed_files.last_mut() {
                file.binary = true;
            }
        }
    }
    RunDiffPreview {
        text,
        changed_files,
        total_bytes,
        truncated,
    }
}

const fn role_order(role: Role) -> u8 {
    match role {
        Role::Researcher => 0,
        Role::Architect => 1,
        Role::Implementer => 2,
        Role::CodeQualityReviewer => 3,
        Role::SpecReviewer => 4,
        Role::Reviewer => 5,
        Role::EngineeringLead => 6,
    }
}

fn task_summary(task: Option<&str>) -> String {
    let Some(task) = task else {
        return "<legacy input unavailable>".to_owned();
    };
    let first = task
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(task);
    let mut chars = first.trim().chars();
    let summary = chars.by_ref().take(72).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}…")
    } else {
        summary
    }
}

fn usage_summary(events: &[SequencedEvent]) -> UsageSummary {
    events
        .iter()
        .fold(UsageSummary::default(), |mut usage, event| {
            if let DomainEventKind::ProviderUsageUpdated {
                input_units,
                output_units,
                ..
            } = event.event.kind()
            {
                usage.input_units = usage.input_units.saturating_add(*input_units);
                usage.output_units = usage.output_units.saturating_add(*output_units);
            }
            usage
        })
}
