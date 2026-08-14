use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    AttentionKind, AttentionRequestId, EventId, ModelId, ProviderId, ProviderSessionId, RunId,
    StageId, WorkflowKind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventMetadata {
    id: EventId,
    occurred_at: DateTime<Utc>,
}

impl EventMetadata {
    #[must_use]
    pub const fn new(id: EventId, occurred_at: DateTime<Utc>) -> Self {
        Self { id, occurred_at }
    }

    #[must_use]
    pub const fn id(&self) -> EventId {
        self.id
    }

    #[must_use]
    pub const fn occurred_at(self) -> DateTime<Utc> {
        self.occurred_at
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainEvent {
    id: EventId,
    occurred_at: DateTime<Utc>,
    run_id: RunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage_id: Option<StageId>,
    #[serde(flatten)]
    kind: DomainEventKind,
}

impl DomainEvent {
    #[must_use]
    pub const fn new(
        metadata: EventMetadata,
        run_id: RunId,
        stage_id: Option<StageId>,
        kind: DomainEventKind,
    ) -> Self {
        Self {
            id: metadata.id,
            occurred_at: metadata.occurred_at,
            run_id,
            stage_id,
            kind,
        }
    }

    #[must_use]
    pub const fn id(&self) -> EventId {
        self.id
    }

    #[must_use]
    pub const fn run_id(&self) -> RunId {
        self.run_id
    }

    #[must_use]
    pub const fn stage_id(&self) -> Option<&StageId> {
        self.stage_id.as_ref()
    }

    #[must_use]
    pub const fn occurred_at(&self) -> &DateTime<Utc> {
        &self.occurred_at
    }

    #[must_use]
    pub const fn kind(&self) -> &DomainEventKind {
        &self.kind
    }
}

/// Provider-neutral semantic history and integration signal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DomainEventKind {
    RunCreated {
        workflow: WorkflowKind,
    },
    RunPreparationStarted,
    RunPrepared,
    RunStarted,
    RunPaused,
    RunInterrupted,
    RunResumed,
    RunRecovered,
    RunCompleted,
    RunFailed,
    RunApplied,
    RunDiscarded,
    StageReady {
        degraded: bool,
    },
    StageStarted,
    StagePaused,
    StageInterrupted,
    StageResumed,
    StageRecovered,
    StageCompleted,
    StageSkipped,
    StageFailed,
    StageRetryScheduled,
    NeedsUser {
        attention_request_id: AttentionRequestId,
        kind: AttentionKind,
    },
    AttentionResolved {
        attention_request_id: AttentionRequestId,
    },
    AttentionCancelled {
        attention_request_id: AttentionRequestId,
    },
    ProviderStarted {
        provider_id: ProviderId,
        model_id: Option<ModelId>,
        session_id: Option<ProviderSessionId>,
    },
    ProviderResumed {
        provider_id: ProviderId,
        session_id: ProviderSessionId,
    },
    ProviderProgress {
        provider_id: ProviderId,
        message: String,
    },
    ProviderNeedsUser {
        provider_id: ProviderId,
        session_id: Option<ProviderSessionId>,
        attention_request_id: AttentionRequestId,
    },
    ProviderPaused {
        provider_id: ProviderId,
        session_id: Option<ProviderSessionId>,
    },
    ProviderInterrupted {
        provider_id: ProviderId,
        session_id: Option<ProviderSessionId>,
    },
    ProviderCompleted {
        provider_id: ProviderId,
        session_id: Option<ProviderSessionId>,
    },
    ProviderFailed {
        provider_id: ProviderId,
        session_id: Option<ProviderSessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    UsageUpdated,
    ProviderUsageUpdated {
        provider_id: ProviderId,
        input_units: u64,
        output_units: u64,
    },
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    #[test]
    fn semantic_event_round_trip_uses_explicit_snake_case_type() {
        let event = DomainEvent::new(
            EventMetadata::new(
                EventId::from_u128(1),
                Utc.with_ymd_and_hms(2026, 8, 14, 8, 0, 0).single().unwrap(),
            ),
            RunId::from_u128(2),
            Some(StageId::new("review").unwrap()),
            DomainEventKind::StageReady { degraded: true },
        );
        let encoded = serde_json::to_string(&event).unwrap();
        let decoded: DomainEvent = serde_json::from_str(&encoded).unwrap();

        assert!(encoded.contains("\"type\":\"stage_ready\""));
        assert_eq!(decoded, event);
    }
}
