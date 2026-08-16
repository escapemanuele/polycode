//! Native Claude Code CLI adapter. No direct Anthropic API usage.

mod artifact;
mod command;
mod detection;
mod error;
mod prompt;
mod protocol;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::domain::{
    AttentionKind, AttentionRequestId, ModelId, ProviderId, ProviderSessionId, Role,
};
use crate::engine::{Provider, ProviderError, ProviderPoll, ProviderRequest, ProviderSignal};
use crate::process::{
    ManagedProcessStatus, OutputChunk, OutputStream, ProcessBackend, ProcessManager, TmuxBackend,
};
use crate::providers::{
    PendingProviderAttention, ProviderCommit, ProviderSessionMutation, ProviderSessionRecord,
    ProviderSessionRecordId, ProviderSessionStatus,
};
use crate::store::{SqliteStore, process_root};

pub use detection::{ClaudeInstallation, suspicious_secret_environment};
pub use error::ClaudeProviderError;
use protocol::{ClaudeRecord, PermissionDenial, first_record};

const PROTOCOL_VERSION: u32 = 1;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

pub struct ClaudeProvider<B = TmuxBackend> {
    id: ProviderId,
    installation: ClaudeInstallation,
    model: Option<ModelId>,
    manager: ProcessManager<B>,
    artifact_root: PathBuf,
}

impl ClaudeProvider<TmuxBackend> {
    /// Builds native adapter using discovered Claude Code, tmux, and Polycode data root.
    ///
    /// # Errors
    /// Returns missing/auth/process-path failures before execution starts.
    pub fn from_environment(model: Option<ModelId>) -> Result<Self, ClaudeProviderError> {
        let installation = ClaudeInstallation::discover()?;
        installation.require_authenticated()?;
        let root = process_root()?;
        Ok(Self {
            id: ProviderId::new("claude")
                .map_err(|error| ClaudeProviderError::Protocol(error.to_string()))?,
            installation,
            model,
            manager: ProcessManager::from_environment()?,
            artifact_root: root,
        })
    }
}

impl<B: ProcessBackend> ClaudeProvider<B> {
    #[must_use]
    pub const fn installation(&self) -> &ClaudeInstallation {
        &self.installation
    }

    fn now() -> DateTime<Utc> {
        std::time::SystemTime::now().into()
    }

    fn start_invocation(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
        mut session: ProviderSessionRecord,
    ) -> Result<ProviderPoll, ClaudeProviderError> {
        let invocation = session
            .invocation()
            .checked_add(1)
            .ok_or_else(|| ClaudeProviderError::Protocol("invocation overflow".to_owned()))?;
        if let Some(orphan) = store
            .load_managed_process_for_attempt(
                request.run_id(),
                request.stage_id(),
                request.attempt(),
            )?
            .filter(|process| {
                process.invocation() == invocation
                    && session.current_process_id() != Some(process.id())
            })
        {
            if orphan.status() != ManagedProcessStatus::Preparing {
                return Err(ClaudeProviderError::Protocol(
                    "unbound provider invocation is not safely restartable".to_owned(),
                ));
            }
            let expected = session.revision();
            session
                .bind_process(orphan.id(), invocation, Self::now())
                .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
            store.update_provider_session(&session, expected)?;
            self.manager.start(store, orphan.id())?;
            return Ok(ProviderPoll::Pending);
        }
        let command = if invocation == 1 {
            let artifacts = store.list_artifacts(request.run_id())?;
            command::initial(&prompt::compose(request, &artifacts)?, self.model.as_ref())
        } else {
            let native = session.native_session_id().ok_or_else(|| {
                ClaudeProviderError::Protocol("resume has no native session ID".to_owned())
            })?;
            let denials = session
                .pending_attention()
                .map(|pending| Self::read_pending_denials(store, pending))
                .transpose()?
                .unwrap_or_default();
            let response = session
                .pending_attention()
                .map(|pending| self.read_response(session.id(), pending.attention_id()))
                .transpose()?
                .flatten();
            command::resume(native, &denials, response.as_deref(), self.model.as_ref())?
        };
        let process = self.manager.prepare_with_input(
            store,
            request.run_id(),
            request.stage_id().clone(),
            request.attempt(),
            invocation,
            self.installation.executable(),
            command.argv,
            BTreeMap::new(),
            &command.stdin,
        )?;
        let expected = session.revision();
        session
            .bind_process(process.id(), invocation, Self::now())
            .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
        let session = store.update_provider_session(&session, expected)?;
        self.manager.start(store, process.id())?;
        debug_assert_eq!(session.current_process_id(), Some(process.id()));
        Ok(ProviderPoll::Pending)
    }

    fn poll_session(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
        session: ProviderSessionRecord,
    ) -> Result<ProviderPoll, ClaudeProviderError> {
        if session.status() == ProviderSessionStatus::Created {
            return self.start_invocation(store, request, session);
        }
        if matches!(
            session.status(),
            ProviderSessionStatus::NeedsUser | ProviderSessionStatus::Interrupted
        ) && request.stage_status() == crate::domain::StageStatus::Running
        {
            return self.start_invocation(store, request, session);
        }
        let process_id = session.current_process_id().ok_or_else(|| {
            ClaudeProviderError::Protocol("provider session has no current process".to_owned())
        })?;
        let inspection = self.manager.inspect(store, process_id)?;
        if matches!(
            inspection.process.status(),
            ManagedProcessStatus::Preparing | ManagedProcessStatus::Starting
        ) {
            self.manager.start(store, process_id)?;
        }
        let inspection = self.manager.inspect(store, process_id)?;
        let chunk =
            self.manager
                .read_output(store, process_id, OutputStream::Stdout, MAX_OUTPUT_BYTES)?;
        if let Some((record, consumed)) = first_record(chunk.bytes())? {
            let consumed = u64::try_from(consumed)
                .map_err(|_| ClaudeProviderError::Protocol("record size overflow".to_owned()))?;
            let end = chunk.start_offset().checked_add(consumed).ok_or_else(|| {
                ClaudeProviderError::Protocol("output offset overflow".to_owned())
            })?;
            return self.map_record(store, request, session, chunk, end, record);
        }
        if inspection.process.status().is_active() {
            return Ok(ProviderPoll::Pending);
        }
        Self::map_terminal_without_result(request, session, chunk, inspection.process.status())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "single exhaustive protocol mapping keeps semantic checkpoint behavior auditable"
    )]
    fn map_record(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
        mut session: ProviderSessionRecord,
        chunk: OutputChunk,
        end: u64,
        record: ClaudeRecord,
    ) -> Result<ProviderPoll, ClaudeProviderError> {
        let expected = session.revision();
        let mut commit = ProviderCommit::new(chunk, end);
        let signal = match record {
            ClaudeRecord::Initialized { session_id, model } => {
                let native = ProviderSessionId::new(session_id)
                    .map_err(|error| ClaudeProviderError::Protocol(error.to_string()))?;
                let model = model
                    .map(ModelId::new)
                    .transpose()
                    .map_err(|error| ClaudeProviderError::Protocol(error.to_string()))?;
                session
                    .activate(native.clone(), model.clone(), Self::now())
                    .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
                commit = commit.with_session(ProviderSessionMutation::new(session, expected));
                if request.signal_index() == 0 {
                    ProviderSignal::Started {
                        model_id: model,
                        session_id: Some(native),
                    }
                } else {
                    ProviderSignal::Resumed
                }
            }
            ClaudeRecord::Progress(message) => ProviderSignal::Progress(message),
            ClaudeRecord::Usage(usage) => ProviderSignal::Usage(usage),
            ClaudeRecord::NeedsUser {
                summary,
                denials,
                question,
            } => {
                let attention_id = AttentionRequestId::new();
                if let Some(process_id) = session.current_process_id() {
                    let process = store.load_managed_process(process_id)?;
                    if process.status().is_active() {
                        self.manager.interrupt(store, process_id)?;
                    }
                }
                let pending = PendingProviderAttention::new(
                    attention_id,
                    commit.output().process_id(),
                    commit.output().start_offset(),
                    end,
                )
                .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
                session
                    .need_user(pending, Self::now())
                    .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
                commit = commit.with_session(ProviderSessionMutation::new(session, expected));
                debug_assert!(!denials.is_empty());
                ProviderSignal::NeedsUser {
                    kind: if question {
                        AttentionKind::Question
                    } else {
                        AttentionKind::Permission
                    },
                    summary,
                    request_id: Some(attention_id),
                }
            }
            ClaudeRecord::Result {
                content,
                success,
                error,
                denials,
                ..
            } if !denials.is_empty() => {
                let attention_id = AttentionRequestId::new();
                let summary = permission_summary(&denials);
                let pending = PendingProviderAttention::new(
                    attention_id,
                    commit.output().process_id(),
                    commit.output().start_offset(),
                    end,
                )
                .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
                session
                    .need_user(pending, Self::now())
                    .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
                commit = commit.with_session(ProviderSessionMutation::new(session, expected));
                let _ = (content, success, error);
                ProviderSignal::NeedsUser {
                    kind: AttentionKind::Permission,
                    summary,
                    request_id: Some(attention_id),
                }
            }
            ClaudeRecord::Result {
                content,
                success: true,
                ..
            } => {
                let workspace = store.load_workspace(request.run_id())?.ok_or_else(|| {
                    ClaudeProviderError::Protocol("run workspace disappeared".to_owned())
                })?;
                let artifact = artifact::persist(
                    &self.artifact_root,
                    request,
                    &self.id,
                    session.model_id(),
                    workspace.base_commit(),
                    &content,
                    Self::now(),
                )?;
                session
                    .complete(Self::now())
                    .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
                commit = commit
                    .with_session(ProviderSessionMutation::new(session, expected))
                    .with_artifact(artifact);
                ProviderSignal::Completed
            }
            ClaudeRecord::Result { error, .. } => {
                session
                    .fail(Self::now())
                    .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
                commit = commit.with_session(ProviderSessionMutation::new(session, expected));
                ProviderSignal::Failed(
                    error.unwrap_or_else(|| "Claude Code execution failed".to_owned()),
                )
            }
            ClaudeRecord::Ignored => return Ok(ProviderPoll::Checkpoint(commit)),
        };
        Ok(ProviderPoll::Emission {
            signals: vec![signal],
            commit,
        })
    }

    fn map_terminal_without_result(
        request: &ProviderRequest,
        mut session: ProviderSessionRecord,
        chunk: OutputChunk,
        status: ManagedProcessStatus,
    ) -> Result<ProviderPoll, ClaudeProviderError> {
        if session.native_session_id().is_none() {
            return Err(ClaudeProviderError::Command {
                operation: "session initialization",
                message: format!(
                    "process ended as {status:?} before Claude emitted structured init"
                ),
            });
        }
        let expected = session.revision();
        let end = chunk.end_offset();
        let signal = if matches!(
            status,
            ManagedProcessStatus::Interrupted | ManagedProcessStatus::Missing
        ) {
            session
                .interrupt(Self::now())
                .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
            ProviderSignal::Interrupted
        } else {
            session
                .fail(Self::now())
                .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
            ProviderSignal::Failed(format!(
                "Claude Code process ended as {status:?} without terminal result for {}",
                request.stage_id()
            ))
        };
        Ok(ProviderPoll::Emission {
            signals: vec![signal],
            commit: ProviderCommit::new(chunk, end)
                .with_session(ProviderSessionMutation::new(session, expected)),
        })
    }

    fn read_pending_denials(
        store: &SqliteStore,
        pending: &PendingProviderAttention,
    ) -> Result<Vec<PermissionDenial>, ClaudeProviderError> {
        let process = store.load_managed_process(pending.process_id())?;
        let mut file = File::open(process.spec().stdout_path())?;
        file.seek(SeekFrom::Start(pending.record_start()))?;
        let length = pending
            .record_end()
            .checked_sub(pending.record_start())
            .ok_or_else(|| {
                ClaudeProviderError::Protocol("attention range regression".to_owned())
            })?;
        let mut bytes = vec![
            0;
            usize::try_from(length).map_err(|_| ClaudeProviderError::Protocol(
                "attention record too large".to_owned()
            ))?
        ];
        file.read_exact(&mut bytes)?;
        let Some((record, consumed)) = first_record(&bytes)? else {
            return Err(ClaudeProviderError::Protocol(
                "pending attention record is incomplete".to_owned(),
            ));
        };
        if consumed != bytes.len() {
            return Err(ClaudeProviderError::Protocol(
                "pending attention range contains multiple records".to_owned(),
            ));
        }
        match record {
            ClaudeRecord::Result { denials, .. } | ClaudeRecord::NeedsUser { denials, .. } => {
                Ok(denials)
            }
            _ => Err(ClaudeProviderError::Protocol(
                "pending attention record has wrong type".to_owned(),
            )),
        }
    }

    fn response_path(
        &self,
        session_id: ProviderSessionRecordId,
        attention_id: AttentionRequestId,
    ) -> PathBuf {
        self.artifact_root
            .join("provider-responses")
            .join(session_id.to_string())
            .join(format!("{attention_id}.txt"))
    }

    fn read_response(
        &self,
        session_id: ProviderSessionRecordId,
        attention_id: AttentionRequestId,
    ) -> Result<Option<String>, ClaudeProviderError> {
        let path = self.response_path(session_id, attention_id);
        match std::fs::read_to_string(path) {
            Ok(response) => Ok(Some(response)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn write_response_once(
        &self,
        session_id: ProviderSessionRecordId,
        attention_id: AttentionRequestId,
        response: &str,
    ) -> Result<(), ClaudeProviderError> {
        use std::io::Write as _;

        let bytes = response.as_bytes();
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(ClaudeProviderError::ResponseTooLarge(MAX_RESPONSE_BYTES));
        }
        if response.trim().is_empty() {
            return Err(ClaudeProviderError::QuestionResponseRequired);
        }
        let path = self.response_path(session_id, attention_id);
        let directory = path.parent().ok_or_else(|| {
            ClaudeProviderError::Protocol("response path has no parent".to_owned())
        })?;
        std::fs::create_dir_all(directory)?;
        if path.exists() {
            return if std::fs::read(&path)? == bytes {
                Ok(())
            } else {
                Err(ClaudeProviderError::ArtifactConflict(path))
            };
        }
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&path) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::read(&path)? != bytes {
                    return Err(ClaudeProviderError::ArtifactConflict(path));
                }
            }
            Err(error) => return Err(error.error.into()),
        }
        Ok(())
    }
}

impl<B: ProcessBackend> Provider for ClaudeProvider<B> {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn supports_role(&self, _role: Role) -> bool {
        true
    }

    fn keep_attached(&self) -> bool {
        true
    }

    fn stage_attention_response(
        &mut self,
        store: &mut SqliteStore,
        run_id: crate::domain::RunId,
        request_id: AttentionRequestId,
        response: Option<&str>,
    ) -> Result<(), ProviderError> {
        let result = (|| -> Result<(), ClaudeProviderError> {
            let session = store
                .list_provider_sessions(run_id)?
                .into_iter()
                .find(|session| {
                    session
                        .pending_attention()
                        .is_some_and(|pending| pending.attention_id() == request_id)
                })
                .ok_or_else(|| {
                    ClaudeProviderError::Protocol(
                        "attention has no matching Claude provider session".to_owned(),
                    )
                })?;
            let pending = session
                .pending_attention()
                .expect("matched pending attention");
            let denials = Self::read_pending_denials(store, pending)?;
            if denials
                .iter()
                .any(|denial| denial.tool_name == "AskUserQuestion")
            {
                self.write_response_once(
                    session.id(),
                    request_id,
                    response.ok_or(ClaudeProviderError::QuestionResponseRequired)?,
                )?;
            }
            Ok(())
        })();
        result.map_err(|error| ProviderError::new(error.to_string()))
    }

    fn poll(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
    ) -> Result<ProviderPoll, ProviderError> {
        let result = (|| -> Result<ProviderPoll, ClaudeProviderError> {
            match store.load_provider_session_for_attempt(
                request.run_id(),
                request.stage_id(),
                request.attempt(),
            )? {
                Some(session) if session.provider_id() != &self.id => {
                    Err(ClaudeProviderError::Protocol(
                        "persisted provider session belongs to another provider".to_owned(),
                    ))
                }
                Some(session) => self.poll_session(store, request, session),
                None => {
                    let session = ProviderSessionRecord::new(
                        ProviderSessionRecordId::new(),
                        request.run_id(),
                        request.stage_id().clone(),
                        request.attempt(),
                        self.id.clone(),
                        PROTOCOL_VERSION,
                        Some(self.installation.version().to_owned()),
                        Self::now(),
                    );
                    let session = store.insert_provider_session(&session)?;
                    self.start_invocation(store, request, session)
                }
            }
        })();
        result.map_err(|error| ProviderError::new(error.to_string()))
    }
}

fn permission_summary(denials: &[PermissionDenial]) -> String {
    let names = denials
        .iter()
        .map(|denial| denial.tool_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("Claude Code requests permission for: {names}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::io::{Read as _, Seek as _, SeekFrom};
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    use chrono::Utc;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::domain::{
        ConfigSnapshotId, EventId, EventMetadata, Run, RunStatus, WorkflowDefinition, WorkflowKind,
    };
    use crate::engine::{EngineStatus, WorkflowEngine};
    use crate::process::{
        BackendAvailability, BackendSessionId, BackendSessionState, ExitEvidence, ExitResult,
        ManagedProcess, ManagedProcessId, OutputStream, ProcessError,
    };
    use crate::store::{ResolvedConfigSnapshot, RunInput};
    use crate::workspace::WorkspaceManager;

    #[derive(Clone, Default)]
    struct FixtureBackend {
        started: Arc<Mutex<HashSet<ManagedProcessId>>>,
    }

    impl ProcessBackend for FixtureBackend {
        fn kind(&self) -> &'static str {
            "fixture"
        }

        fn session_id(&self, process_id: ManagedProcessId) -> BackendSessionId {
            BackendSessionId::for_process(process_id)
        }

        fn availability(&self) -> Result<BackendAvailability, ProcessError> {
            Ok(BackendAvailability {
                kind: self.kind(),
                version: "fixture-1".to_owned(),
            })
        }

        fn start(&self, process: &ManagedProcess, _manifest: &Path) -> Result<(), ProcessError> {
            let output = if process.invocation() == 1 {
                concat!(
                    "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"native-session-1\",\"model\":\"fixture-model\"}\n",
                    "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\",\"session_id\":\"native-session-1\",\"permission_denials\":[{\"tool_name\":\"Write\",\"tool_input\":{\"file_path\":\"/tmp/fixture\"}}]}\n"
                )
            } else {
                concat!(
                    "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"native-session-1\",\"model\":\"fixture-model\"}\n",
                    "{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":3},\"content\":[]}}\n",
                    "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"# Completed\\nFixture result\",\"session_id\":\"native-session-1\",\"permission_denials\":[]}\n"
                )
            };
            std::fs::write(process.spec().stdout_path(), output)?;
            self.started.lock().unwrap().insert(process.id());
            Ok(())
        }

        fn inspect_session(
            &self,
            _process: &ManagedProcess,
        ) -> Result<BackendSessionState, ProcessError> {
            Ok(BackendSessionState::Absent)
        }

        fn read_output(
            &self,
            process: &ManagedProcess,
            stream: OutputStream,
            offset: u64,
            max_bytes: usize,
        ) -> Result<OutputChunk, ProcessError> {
            let path = match stream {
                OutputStream::Stdout => process.spec().stdout_path(),
                OutputStream::Stderr => process.spec().stderr_path(),
            };
            let mut file = File::open(path)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = Vec::new();
            file.take(u64::try_from(max_bytes).unwrap())
                .read_to_end(&mut bytes)?;
            OutputChunk::new(
                process.id(),
                stream,
                process.cursor(stream).revision(),
                offset,
                bytes,
            )
        }

        fn output_length(
            &self,
            process: &ManagedProcess,
            stream: OutputStream,
        ) -> Result<u64, ProcessError> {
            let path = match stream {
                OutputStream::Stdout => process.spec().stdout_path(),
                OutputStream::Stderr => process.spec().stderr_path(),
            };
            Ok(std::fs::metadata(path)?.len())
        }

        fn read_exit_evidence(
            &self,
            process: &ManagedProcess,
        ) -> Result<Option<ExitEvidence>, ProcessError> {
            if !self.started.lock().unwrap().contains(&process.id()) {
                return Ok(None);
            }
            let now: DateTime<Utc> = std::time::SystemTime::now().into();
            Ok(Some(ExitEvidence::new(
                process.id(),
                process.command_fingerprint().to_owned(),
                ExitResult::ExitCode { code: 0 },
                false,
                now,
                now,
            )))
        }

        fn interrupt(&self, _process: &ManagedProcess) -> Result<(), ProcessError> {
            Ok(())
        }
        fn cleanup(&self, _process: &ManagedProcess) -> Result<(), ProcessError> {
            Ok(())
        }
    }

    #[test]
    fn fixture_permission_resume_completes_same_session_and_persists_artifact() {
        let (temp, database, run_id, mut store, provider) = fixture();
        let mut engine = WorkflowEngine::new(provider, "make fixture change");
        let attention = loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::NeedsUser { requests } => break requests[0],
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("unexpected status: {status:?}"),
            }
        };
        engine
            .resolve_attention(&mut store, run_id, attention)
            .unwrap();
        loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::Finished {
                    run_status: RunStatus::Completed,
                } => break,
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("unexpected status: {status:?}"),
            }
        }

        drop(store);
        let mut store = SqliteStore::open(&database).unwrap();
        assert_eq!(
            store.load_run(run_id).unwrap().run.status(),
            RunStatus::Completed
        );
        let sessions = store.list_provider_sessions(run_id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].native_session_id().unwrap().as_str(),
            "native-session-1"
        );
        assert_eq!(sessions[0].invocation(), 2);
        assert_eq!(sessions[0].status(), ProviderSessionStatus::Completed);
        assert_eq!(store.list_managed_processes(run_id).unwrap().len(), 2);
        let artifacts = store.list_artifacts(run_id).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert!(
            std::fs::read_to_string(artifacts[0].path())
                .unwrap()
                .contains("Fixture result")
        );
        drop(temp);
    }

    #[test]
    fn event_failure_rolls_back_run_session_and_output_checkpoint() {
        let (_temp, _database, run_id, mut store, provider) = fixture();
        let mut engine = WorkflowEngine::new(provider, "make fixture change");
        assert!(matches!(
            engine.tick(&mut store, run_id).unwrap(),
            EngineStatus::Advanced { .. }
        ));
        assert!(matches!(
            engine.tick(&mut store, run_id).unwrap(),
            EngineStatus::Advanced { .. }
        ));
        assert!(matches!(
            engine.tick(&mut store, run_id).unwrap(),
            EngineStatus::WaitingForProvider { .. }
        ));
        assert!(matches!(
            engine.tick(&mut store, run_id).unwrap(),
            EngineStatus::Advanced { .. }
        ));

        let before_events = store.load_events(run_id).unwrap();
        let before_session = store.list_provider_sessions(run_id).unwrap().pop().unwrap();
        let process_id = before_session.current_process_id().unwrap();
        let before_cursor = store
            .load_managed_process(process_id)
            .unwrap()
            .cursor(OutputStream::Stdout);
        store.install_event_insert_failure();
        assert!(engine.tick(&mut store, run_id).is_err());
        store.remove_event_insert_failure();

        assert_eq!(store.load_events(run_id).unwrap(), before_events);
        assert_eq!(
            store.list_provider_sessions(run_id).unwrap().pop().unwrap(),
            before_session
        );
        assert_eq!(
            store
                .load_managed_process(process_id)
                .unwrap()
                .cursor(OutputStream::Stdout),
            before_cursor
        );
        assert_eq!(
            store.load_run(run_id).unwrap().run.status(),
            RunStatus::Running
        );
        assert!(matches!(
            engine.tick(&mut store, run_id).unwrap(),
            EngineStatus::Advanced { .. }
        ));
        assert_eq!(
            store.load_run(run_id).unwrap().run.status(),
            RunStatus::NeedsUser
        );
    }

    #[test]
    fn restart_binds_prepared_invocation_left_after_session_crash_window() {
        let (_temp, _database, run_id, mut store, provider) = fixture();
        let stage_id = crate::domain::StageId::new("implementation").unwrap();
        let session = ProviderSessionRecord::new(
            ProviderSessionRecordId::new(),
            run_id,
            stage_id.clone(),
            1,
            ProviderId::new("claude").unwrap(),
            PROTOCOL_VERSION,
            Some("fixture".to_owned()),
            ClaudeProvider::<FixtureBackend>::now(),
        );
        let session = store.insert_provider_session(&session).unwrap();
        let orphan = provider
            .manager
            .prepare_with_input(
                &mut store,
                run_id,
                stage_id,
                1,
                1,
                "/bin/true",
                Vec::new(),
                BTreeMap::new(),
                b"already durable prompt",
            )
            .unwrap();
        let mut engine = WorkflowEngine::new(provider, "make fixture change");
        assert!(matches!(
            engine.tick(&mut store, run_id).unwrap(),
            EngineStatus::Advanced { .. }
        ));
        assert!(matches!(
            engine.tick(&mut store, run_id).unwrap(),
            EngineStatus::Advanced { .. }
        ));
        assert!(matches!(
            engine.tick(&mut store, run_id).unwrap(),
            EngineStatus::WaitingForProvider { .. }
        ));
        let recovered = store.load_provider_session(session.id()).unwrap();
        assert_eq!(recovered.current_process_id(), Some(orphan.id()));
        assert_eq!(recovered.invocation(), 1);
        assert_eq!(
            store.load_managed_process(orphan.id()).unwrap().status(),
            ManagedProcessStatus::Exited
        );
    }

    fn fixture() -> (
        TempDir,
        PathBuf,
        crate::domain::RunId,
        SqliteStore,
        ClaudeProvider<FixtureBackend>,
    ) {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        init_repository(&source);
        let database = temp.path().join("polycode.db");
        let process_root = temp.path().join("runs");
        let run_id = crate::domain::RunId::new();
        let created_at: DateTime<Utc> = std::time::SystemTime::now().into();
        let config_id = ConfigSnapshotId::new(format!("m7-{run_id}")).unwrap();
        let run = Run::new(
            run_id,
            WorkflowDefinition::built_in(WorkflowKind::Fast),
            config_id.clone(),
            created_at,
        );
        let input = RunInput::new(run_id, "make fixture change", created_at).unwrap();
        let config = ResolvedConfigSnapshot::new(
            config_id,
            1,
            json!({"schema_version":1,"profile":"native_claude","provider":"claude","model":null,"provider_options":{}}),
            created_at,
        )
        .unwrap();
        let created = run.created_event(EventMetadata::new(EventId::new(), created_at));
        let mut store = SqliteStore::open(&database).unwrap();
        store
            .create_run_with_input(&run, &input, &config, &[created])
            .unwrap();
        WorkspaceManager::new(temp.path().join("worktrees"))
            .prepare_run_workspace(&mut store, run_id, &source)
            .unwrap();
        let provider = ClaudeProvider {
            id: ProviderId::new("claude").unwrap(),
            installation: ClaudeInstallation::fixture(PathBuf::from("/bin/true")),
            model: None,
            manager: ProcessManager::new(&process_root, FixtureBackend::default()),
            artifact_root: process_root,
        };
        (temp, database, run_id, store, provider)
    }

    #[test]
    fn provider_accepts_specialized_reviewer_roles() {
        let (_temp, _database, _run_id, _store, provider) = fixture();
        assert!(provider.supports_role(Role::CodeQualityReviewer));
        assert!(provider.supports_role(Role::SpecReviewer));
    }

    fn init_repository(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        command(path, &["init"]);
        command(path, &["config", "user.email", "polycode@example.invalid"]);
        command(path, &["config", "user.name", "Polycode Test"]);
        std::fs::write(path.join("README.md"), "fixture\n").unwrap();
        command(path, &["add", "README.md"]);
        command(path, &["commit", "-m", "fixture"]);
    }

    fn command(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
