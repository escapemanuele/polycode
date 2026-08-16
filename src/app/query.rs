use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::domain::{
    AttentionKind, AttentionRequestId, AttentionStatus, DomainEventKind, RunId, RunStatus, StageId,
    StageKind, StageStatus, WorkflowKind,
};
use crate::store::{RunRevision, SequencedEvent, SqliteStore};
use crate::workspace::WorkspaceStatus;

use super::AppError;

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
    pub status: StageStatus,
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
    pub provider: Option<String>,
    pub profile: Option<String>,
    pub provider_model: Option<String>,
    pub provider_session_record: Option<String>,
    pub provider_session: Option<String>,
    pub provider_session_status: Option<String>,
    pub process_status: Option<String>,
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

pub(crate) fn inspect(store: &mut SqliteStore, run_id: RunId) -> Result<RunDetails, AppError> {
    let loaded = store.load_run(run_id)?;
    let input = store.load_run_input(run_id)?;
    let workspace = store.load_workspace(run_id)?;
    let events = store.load_events(run_id)?;
    let payload = loaded.config_snapshot.payload();
    let provider = payload
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let profile = payload
        .get("profile")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let usage = usage_summary(&events);
    let provider_session = store.list_provider_sessions(run_id)?.pop();
    let process_status = provider_session
        .as_ref()
        .and_then(crate::providers::ProviderSessionRecord::current_process_id)
        .map(|process_id| store.load_managed_process(process_id))
        .transpose()?
        .map(|process| process.status().as_str().to_owned());
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
        provider,
        profile,
        provider_model: provider_session
            .as_ref()
            .and_then(|session| session.model_id())
            .map(ToString::to_string),
        provider_session_record: provider_session
            .as_ref()
            .map(|session| session.id().to_string()),
        provider_session: provider_session
            .as_ref()
            .and_then(|session| session.native_session_id())
            .map(ToString::to_string),
        provider_session_status: provider_session
            .as_ref()
            .map(|session| session.status().as_str().to_owned()),
        process_status,
        revision: loaded.revision,
        created_at: *loaded.run.created_at(),
        updated_at: *loaded.run.updated_at(),
        stages: loaded
            .run
            .stages()
            .iter()
            .map(|stage| StageSummary {
                id: stage.id().clone(),
                kind: stage.kind(),
                status: stage.status(),
            })
            .collect(),
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
