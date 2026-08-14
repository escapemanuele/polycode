use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{
    AttentionRequest, ConfigSnapshotId, Run, RunId, RunRehydrationData, RunResumeStatus, RunStatus,
    StageDefinition, StageId, StageRehydrationData, StageResumeStatus, StageStatus,
    StageSuspensionOwner, WorkflowKind,
};

use super::StoreError;

pub const RUN_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct RunSnapshotV1 {
    schema_version: u32,
    id: RunId,
    task: String,
    workflow_kind: WorkflowKind,
    stage_definitions: Vec<StageDefinition>,
    config_snapshot_id: ConfigSnapshotId,
    status: RunStatus,
    suspended_from: Option<RunResumeStatusV1>,
    stages: Vec<StageSnapshotV1>,
    attention_requests: Vec<AttentionRequest>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StageSnapshotV1 {
    id: StageId,
    status: StageStatus,
    suspension: Option<StageSuspensionV1>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RunResumeStatusV1 {
    Running,
    NeedsUser,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct StageSuspensionV1 {
    owner: StageSuspensionOwnerV1,
    resume_to: StageResumeStatusV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StageSuspensionOwnerV1 {
    Stage,
    Run,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StageResumeStatusV1 {
    Running,
    NeedsUser,
}

pub(crate) fn encode_run(run: &Run) -> Result<String, StoreError> {
    let data = run.rehydration_data();
    let snapshot = RunSnapshotV1 {
        schema_version: RUN_SNAPSHOT_SCHEMA_VERSION,
        id: data.id,
        task: data.task,
        workflow_kind: data.workflow_kind,
        stage_definitions: data.stage_definitions,
        config_snapshot_id: data.config_snapshot_id,
        status: data.status,
        suspended_from: data.suspended_from.map(|status| match status {
            RunResumeStatus::Running => RunResumeStatusV1::Running,
            RunResumeStatus::NeedsUser => RunResumeStatusV1::NeedsUser,
        }),
        stages: data
            .stages
            .into_iter()
            .map(|stage| {
                let suspension = match (stage.suspension_owner, stage.resume_to) {
                    (Some(owner), Some(resume_to)) => Some(StageSuspensionV1 {
                        owner: match owner {
                            StageSuspensionOwner::Stage => StageSuspensionOwnerV1::Stage,
                            StageSuspensionOwner::Run => StageSuspensionOwnerV1::Run,
                        },
                        resume_to: match resume_to {
                            StageResumeStatus::Running => StageResumeStatusV1::Running,
                            StageResumeStatus::NeedsUser => StageResumeStatusV1::NeedsUser,
                        },
                    }),
                    (None, None) => None,
                    _ => unreachable!("validated stage always has complete suspension context"),
                };
                StageSnapshotV1 {
                    id: stage.id,
                    status: stage.status,
                    suspension,
                }
            })
            .collect(),
        attention_requests: data.attention_requests,
        created_at: data.created_at,
        updated_at: data.updated_at,
    };
    Ok(serde_json::to_string(&snapshot)?)
}

pub(crate) fn decode_run(snapshot_json: &str, column_version: u32) -> Result<Run, StoreError> {
    let envelope: serde_json::Value = serde_json::from_str(snapshot_json)?;
    let version = envelope
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(StoreError::InvalidSnapshotEnvelope)?;
    if version != column_version {
        return Err(StoreError::SnapshotVersionMismatch {
            snapshot: version,
            column: column_version,
        });
    }
    if version != RUN_SNAPSHOT_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSnapshotVersion(version));
    }

    let snapshot: RunSnapshotV1 = serde_json::from_value(envelope)?;
    let data = RunRehydrationData {
        id: snapshot.id,
        task: snapshot.task,
        workflow_kind: snapshot.workflow_kind,
        stage_definitions: snapshot.stage_definitions,
        config_snapshot_id: snapshot.config_snapshot_id,
        status: snapshot.status,
        suspended_from: snapshot.suspended_from.map(|status| match status {
            RunResumeStatusV1::Running => RunResumeStatus::Running,
            RunResumeStatusV1::NeedsUser => RunResumeStatus::NeedsUser,
        }),
        stages: snapshot
            .stages
            .into_iter()
            .map(|stage| {
                let (suspension_owner, resume_to) =
                    stage.suspension.map_or((None, None), |suspension| {
                        let owner = match suspension.owner {
                            StageSuspensionOwnerV1::Stage => StageSuspensionOwner::Stage,
                            StageSuspensionOwnerV1::Run => StageSuspensionOwner::Run,
                        };
                        let resume_to = match suspension.resume_to {
                            StageResumeStatusV1::Running => StageResumeStatus::Running,
                            StageResumeStatusV1::NeedsUser => StageResumeStatus::NeedsUser,
                        };
                        (Some(owner), Some(resume_to))
                    });
                StageRehydrationData {
                    id: stage.id,
                    status: stage.status,
                    suspension_owner,
                    resume_to,
                }
            })
            .collect(),
        attention_requests: snapshot.attention_requests,
        created_at: snapshot.created_at,
        updated_at: snapshot.updated_at,
    };
    Ok(Run::rehydrate(data)?)
}
