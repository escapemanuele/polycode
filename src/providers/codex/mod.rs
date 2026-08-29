//! Native Codex CLI adapter. No direct `OpenAI` API usage.

mod artifact;
mod command;
mod detection;
mod error;
mod prompt;
mod protocol;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::domain::{EffortSetting, ModelId, ProviderId, ProviderSessionId, Role, StageStatus};
use crate::engine::{Provider, ProviderError, ProviderPoll, ProviderRequest, ProviderSignal};
use crate::process::{
    ExitResult, ManagedProcessStatus, OutputChunk, OutputStream, ProcessBackend, ProcessManager,
    TmuxBackend,
};
use crate::providers::{
    ProviderCommit, ProviderSessionMutation, ProviderSessionRecord, ProviderSessionRecordId,
    ProviderSessionStatus, change_handoff,
};
use crate::store::{SqliteStore, process_root};

pub use detection::{CodexInstallation, suspicious_codex_environment};
pub use error::CodexProviderError;
use protocol::{CodexRecord, first_record};

const PROTOCOL_VERSION: u32 = 1;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

pub struct CodexProvider<B = TmuxBackend> {
    id: ProviderId,
    installation: CodexInstallation,
    model: Option<ModelId>,
    effort: EffortSetting,
    manager: ProcessManager<B>,
    artifact_root: PathBuf,
}

impl CodexProvider<TmuxBackend> {
    /// Builds native adapter using discovered Codex CLI, tmux, and Polycode data root.
    ///
    /// # Errors
    /// Returns missing/auth/capability/process-path failures before execution starts.
    pub fn from_environment(model: Option<ModelId>) -> Result<Self, CodexProviderError> {
        let installation = CodexInstallation::discover()?;
        installation.require_authenticated()?;
        let root = process_root()?;
        Ok(Self {
            id: ProviderId::new("codex")
                .map_err(|error| CodexProviderError::Protocol(error.to_string()))?,
            installation,
            model,
            effort: EffortSetting::NativeDefault,
            manager: ProcessManager::from_environment()?,
            artifact_root: root,
        })
    }

    pub(crate) fn from_runtime(
        model: Option<ModelId>,
        root: PathBuf,
        runner_executable: PathBuf,
    ) -> Result<Self, CodexProviderError> {
        let installation = CodexInstallation::discover()?;
        installation.require_authenticated()?;
        Ok(Self {
            id: ProviderId::new("codex")
                .map_err(|error| CodexProviderError::Protocol(error.to_string()))?,
            installation,
            model,
            effort: EffortSetting::NativeDefault,
            manager: ProcessManager::new(&root, TmuxBackend::new(runner_executable)),
            artifact_root: root,
        })
    }
}

impl<B> CodexProvider<B> {
    /// Sets the requested effort translated onto the native
    /// `model_reasoning_effort` override. `NativeDefault` keeps invocations
    /// byte-identical to pre-effort policy.
    #[must_use]
    pub fn with_effort(mut self, effort: EffortSetting) -> Self {
        self.effort = effort;
        self
    }
}

impl<B: ProcessBackend> CodexProvider<B> {
    #[must_use]
    pub const fn installation(&self) -> &CodexInstallation {
        &self.installation
    }

    fn now() -> DateTime<Utc> {
        std::time::SystemTime::now().into()
    }

    fn final_message_path(
        &self,
        request: &ProviderRequest,
        session: &ProviderSessionRecord,
        invocation: u32,
    ) -> PathBuf {
        self.artifact_root
            .join(request.run_id().to_string())
            .join("provider-output")
            .join("codex")
            .join(session.id().to_string())
            .join(format!("invocation-{invocation}.md"))
    }

    fn start_invocation(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
        mut session: ProviderSessionRecord,
    ) -> Result<ProviderPoll, CodexProviderError> {
        let invocation = session
            .invocation()
            .checked_add(1)
            .ok_or_else(|| CodexProviderError::Protocol("invocation overflow".to_owned()))?;
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
                return Err(CodexProviderError::Protocol(
                    "unbound provider invocation is not safely restartable".to_owned(),
                ));
            }
            let expected = session.revision();
            session
                .bind_process(orphan.id(), invocation, Self::now())
                .map_err(|error| CodexProviderError::Protocol(error.to_owned()))?;
            store.update_provider_session(&session, expected)?;
            self.manager.start(store, orphan.id())?;
            return Ok(ProviderPoll::Pending);
        }

        let final_message_path = self.final_message_path(request, &session, invocation);
        create_private_parent(&final_message_path)?;
        let command = if let Some(native) = session.native_session_id() {
            command::resume(
                native,
                &prompt::continuation(request),
                request.stage_kind(),
                self.model.as_ref(),
                self.effort,
                &final_message_path,
            )
        } else {
            let artifacts = store.list_artifacts(request.run_id())?;
            let handoff = change_handoff::for_request(store, request)?;
            command::initial(
                &prompt::compose(request, &artifacts, handoff.as_ref())?,
                request.stage_kind(),
                self.model.as_ref(),
                self.effort,
                &final_message_path,
            )
        };
        debug_assert_eq!(command.final_message_path, final_message_path);
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
            .map_err(|error| CodexProviderError::Protocol(error.to_owned()))?;
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
    ) -> Result<ProviderPoll, CodexProviderError> {
        // See the Claude adapter: an observing poll may never start work.
        if !request.observe_only()
            && (session.status() == ProviderSessionStatus::Created
                || (session.status() == ProviderSessionStatus::Interrupted
                    && matches!(
                        request.stage_status(),
                        StageStatus::Ready | StageStatus::Running
                    )))
        {
            return self.start_invocation(store, request, session);
        }
        let Some(process_id) = session.current_process_id() else {
            if request.observe_only() {
                // Nothing was ever launched, so there is nothing to observe.
                return Ok(ProviderPoll::Pending);
            }
            return Err(CodexProviderError::Protocol(
                "provider session has no current process".to_owned(),
            ));
        };
        let inspection = self.manager.inspect(store, process_id)?;
        if matches!(
            inspection.process.status(),
            ManagedProcessStatus::Preparing | ManagedProcessStatus::Starting
        ) && !request.observe_only()
        {
            self.manager.start(store, process_id)?;
        }
        let inspection = self.manager.inspect(store, process_id)?;
        let chunk =
            self.manager
                .read_output(store, process_id, OutputStream::Stdout, MAX_OUTPUT_BYTES)?;
        if let Some((record, consumed)) = first_record(chunk.bytes())? {
            let consumed = u64::try_from(consumed)
                .map_err(|_| CodexProviderError::Protocol("record size overflow".to_owned()))?;
            let end = chunk
                .start_offset()
                .checked_add(consumed)
                .ok_or_else(|| CodexProviderError::Protocol("output offset overflow".to_owned()))?;
            let successful_exit = inspection.exit_evidence.as_ref().is_some_and(|evidence| {
                matches!(evidence.result(), ExitResult::ExitCode { code: 0 })
            });
            return self.map_record(
                store,
                request,
                session,
                chunk,
                end,
                record,
                inspection.process.status(),
                successful_exit,
            );
        }
        if inspection.process.status().is_active() {
            return Ok(ProviderPoll::Pending);
        }
        if !chunk.bytes().is_empty() {
            return Err(CodexProviderError::Protocol(
                "Codex process ended with incomplete JSON record".to_owned(),
            ));
        }
        Self::map_terminal_without_result(
            store,
            request,
            session,
            chunk,
            inspection.process.status(),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mapping needs exact raw checkpoint plus reconciled process evidence"
    )]
    fn map_record(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
        mut session: ProviderSessionRecord,
        chunk: OutputChunk,
        end: u64,
        record: CodexRecord,
        process_status: ManagedProcessStatus,
        successful_exit: bool,
    ) -> Result<ProviderPoll, CodexProviderError> {
        let expected = session.revision();
        let mut commit = ProviderCommit::new(chunk, end);
        let signals = match record {
            CodexRecord::ThreadStarted { thread_id } => {
                let native = ProviderSessionId::new(thread_id)
                    .map_err(|error| CodexProviderError::Protocol(error.to_string()))?;
                if let Some(previous) = session.native_session_id() {
                    if previous != &native {
                        return Err(CodexProviderError::SessionMismatch {
                            expected: previous.to_string(),
                            actual: native.to_string(),
                        });
                    }
                    if session.status() != ProviderSessionStatus::Starting {
                        return Ok(ProviderPoll::Checkpoint(commit));
                    }
                }
                session
                    .activate(native.clone(), None, Self::now())
                    .map_err(|error| CodexProviderError::Protocol(error.to_owned()))?;
                commit = commit.with_session(ProviderSessionMutation::new(session, expected));
                vec![if request.signal_index() == 0 {
                    ProviderSignal::Started {
                        model_id: None,
                        session_id: Some(native),
                    }
                } else {
                    ProviderSignal::Resumed
                }]
            }
            CodexRecord::Progress(message) => {
                if session.native_session_id().is_none() {
                    return Err(CodexProviderError::Protocol(
                        "Codex progress preceded thread.started".to_owned(),
                    ));
                }
                vec![ProviderSignal::Progress(message)]
            }
            CodexRecord::TurnCompleted(usage) => {
                if session.native_session_id().is_none() {
                    return Err(CodexProviderError::Protocol(
                        "Codex completion preceded thread.started".to_owned(),
                    ));
                }
                if process_status.is_active() {
                    return Ok(ProviderPoll::Pending);
                }
                if process_status != ManagedProcessStatus::Exited || !successful_exit {
                    return Err(CodexProviderError::Protocol(format!(
                        "Codex emitted turn.completed but process ended as {process_status:?} without successful exit evidence"
                    )));
                }
                let final_path = self.final_message_path(request, &session, session.invocation());
                let workspace = store.load_workspace(request.run_id())?.ok_or_else(|| {
                    CodexProviderError::Protocol("run workspace disappeared".to_owned())
                })?;
                let artifact = artifact::persist(
                    &self.artifact_root,
                    &final_path,
                    request,
                    &self.id,
                    session.model_id(),
                    workspace.base_commit(),
                    Self::now(),
                )?;
                session
                    .complete(Self::now())
                    .map_err(|error| CodexProviderError::Protocol(error.to_owned()))?;
                commit = commit
                    .with_session(ProviderSessionMutation::new(session, expected))
                    .with_artifact(artifact);
                vec![ProviderSignal::Usage(usage), ProviderSignal::Completed]
            }
            CodexRecord::Failed(message) => {
                if session.native_session_id().is_none() {
                    return Err(CodexProviderError::MissingThreadId(message));
                }
                session
                    .fail(Self::now())
                    .map_err(|error| CodexProviderError::Protocol(error.to_owned()))?;
                commit = commit.with_session(ProviderSessionMutation::new(session, expected));
                vec![ProviderSignal::Failed(message)]
            }
            CodexRecord::Ignored => return Ok(ProviderPoll::Checkpoint(commit)),
        };
        Ok(ProviderPoll::Emission { signals, commit })
    }

    fn map_terminal_without_result(
        store: &mut SqliteStore,
        request: &ProviderRequest,
        mut session: ProviderSessionRecord,
        chunk: OutputChunk,
        status: ManagedProcessStatus,
    ) -> Result<ProviderPoll, CodexProviderError> {
        // Execution cannot continue a thread Codex never started, so it says
        // so. Observation can: a stop is asking what happened, and "the
        // invocation died before it ever started" is an answer, not a failure.
        // Raising here instead would make the run unstoppable.
        if session.native_session_id().is_none() && !request.observe_only() {
            if matches!(
                status,
                ManagedProcessStatus::Interrupted | ManagedProcessStatus::Missing
            ) {
                let expected = session.revision();
                session
                    .interrupt(Self::now())
                    .map_err(|error| CodexProviderError::Protocol(error.to_owned()))?;
                store.update_provider_session(&session, expected)?;
            }
            return Err(CodexProviderError::MissingThreadId(format!(
                "process ended as {status:?} before Codex emitted thread.started for {}",
                request.stage_id()
            )));
        }
        let expected = session.revision();
        let end = chunk.end_offset();
        let signal = if matches!(
            status,
            ManagedProcessStatus::Interrupted | ManagedProcessStatus::Missing
        ) {
            session
                .interrupt(Self::now())
                .map_err(|error| CodexProviderError::Protocol(error.to_owned()))?;
            ProviderSignal::Interrupted
        } else {
            session
                .fail(Self::now())
                .map_err(|error| CodexProviderError::Protocol(error.to_owned()))?;
            ProviderSignal::Failed(format!(
                "Codex process ended as {status:?} without turn completion for {}",
                request.stage_id()
            ))
        };
        Ok(ProviderPoll::Emission {
            signals: vec![signal],
            commit: ProviderCommit::new(chunk, end)
                .with_session(ProviderSessionMutation::new(session, expected)),
        })
    }
}

impl<B: ProcessBackend> Provider for CodexProvider<B> {
    fn provider_id_for(&self, _request: &ProviderRequest) -> Result<ProviderId, ProviderError> {
        Ok(self.id.clone())
    }

    fn supports_role(&self, _role: Role) -> bool {
        true
    }

    fn keep_attached_for(&self, _request: &ProviderRequest) -> Result<bool, ProviderError> {
        Ok(true)
    }

    fn poll(
        &mut self,
        store: &mut SqliteStore,
        request: &ProviderRequest,
    ) -> Result<ProviderPoll, ProviderError> {
        let result = (|| -> Result<ProviderPoll, CodexProviderError> {
            match store.load_provider_session_for_attempt(
                request.run_id(),
                request.stage_id(),
                request.attempt(),
            )? {
                Some(session) if session.provider_id() != &self.id => {
                    Err(CodexProviderError::Protocol(
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

fn create_private_parent(path: &Path) -> Result<(), CodexProviderError> {
    let parent = path.parent().ok_or_else(|| {
        CodexProviderError::Protocol("final-message path has no parent".to_owned())
    })?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::fs::File;
    use std::io::{Read as _, Seek as _, SeekFrom};
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::domain::{
        ConfigSnapshotId, DomainEventKind, EventId, EventMetadata, Run, RunStatus,
        WorkflowDefinition, WorkflowKind,
    };
    use crate::engine::{EngineStatus, WorkflowEngine};
    use crate::process::{
        BackendAvailability, BackendSessionId, BackendSessionState, ExitEvidence, ManagedProcess,
        ManagedProcessId, ProcessError,
    };
    use crate::store::{ResolvedConfigSnapshot, RunInput};
    use crate::workspace::WorkspaceManager;

    const SUCCESS_OUTPUT: &str = concat!(
        "{\"type\":\"thread.started\",\"thread_id\":\"codex-thread-1\"}\n",
        "{\"type\":\"turn.started\"}\n",
        "{\"type\":\"item.completed\",\"item\":{\"id\":\"m1\",\"type\":\"agent_message\",\"text\":\"Fixture progress\"}}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":50,\"output_tokens\":20,\"reasoning_output_tokens\":5}}\n"
    );

    #[derive(Clone)]
    struct FixtureBackend {
        started: Arc<Mutex<HashSet<ManagedProcessId>>>,
        completed: Arc<Mutex<HashSet<ManagedProcessId>>>,
        output: Arc<String>,
    }

    #[derive(Clone, Default)]
    struct RecoveryBackend {
        started: Arc<Mutex<HashSet<ManagedProcessId>>>,
        inspections: Arc<Mutex<HashMap<ManagedProcessId, usize>>>,
        invocations: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl FixtureBackend {
        fn new(output: &str) -> Self {
            Self {
                started: Arc::new(Mutex::new(HashSet::new())),
                completed: Arc::new(Mutex::new(HashSet::new())),
                output: Arc::new(output.to_owned()),
            }
        }
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
            std::fs::write(process.spec().stdout_path(), self.output.as_bytes())?;
            let args = process.spec().argv();
            let output_index = args
                .iter()
                .position(|arg| arg == "--output-last-message")
                .expect("fixture command has final-message option");
            std::fs::write(
                PathBuf::from(&args[output_index + 1]),
                "# Codex result\nFixture\n",
            )?;
            self.started.lock().unwrap().insert(process.id());
            Ok(())
        }

        fn inspect_session(
            &self,
            process: &ManagedProcess,
        ) -> Result<BackendSessionState, ProcessError> {
            if self.started.lock().unwrap().contains(&process.id()) {
                self.completed.lock().unwrap().insert(process.id());
            }
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
            if !self.completed.lock().unwrap().contains(&process.id()) {
                return Ok(None);
            }
            let now = CodexProvider::<Self>::now();
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

    impl ProcessBackend for RecoveryBackend {
        fn kind(&self) -> &'static str {
            "recovery_fixture"
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
            let args = process
                .spec()
                .argv()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            self.invocations.lock().unwrap().push(args.clone());
            let output = if process.invocation() == 1 {
                "{\"type\":\"thread.started\",\"thread_id\":\"recovery-thread\"}\n"
            } else {
                concat!(
                    "{\"type\":\"thread.started\",\"thread_id\":\"recovery-thread\"}\n",
                    "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":9,\"output_tokens\":4}}\n"
                )
            };
            std::fs::write(process.spec().stdout_path(), output)?;
            if process.invocation() == 2 {
                let output_index = args
                    .iter()
                    .position(|arg| arg == "--output-last-message")
                    .unwrap();
                std::fs::write(&args[output_index + 1], "Recovered result\n")?;
            }
            self.started.lock().unwrap().insert(process.id());
            Ok(())
        }

        fn inspect_session(
            &self,
            process: &ManagedProcess,
        ) -> Result<BackendSessionState, ProcessError> {
            if !self.started.lock().unwrap().contains(&process.id()) {
                return Ok(BackendSessionState::Absent);
            }
            if process.invocation() != 1 {
                return Ok(BackendSessionState::Absent);
            }
            let mut inspections = self.inspections.lock().unwrap();
            let count = inspections.entry(process.id()).or_default();
            let state = if *count == 0 {
                BackendSessionState::Owned
            } else {
                BackendSessionState::Absent
            };
            *count += 1;
            Ok(state)
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
            if process.invocation() != 2 || !self.started.lock().unwrap().contains(&process.id()) {
                return Ok(None);
            }
            let now = CodexProvider::<Self>::now();
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
    fn successful_turn_with_disappeared_session_and_durable_exit_completes_once() {
        let (_temp, database, run_id, mut store, provider) = fixture(SUCCESS_OUTPUT);
        let mut engine = WorkflowEngine::new(provider, "SUPER_SECRET_TASK_MARKER");
        loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::Finished {
                    run_status: RunStatus::Completed,
                } => break,
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("unexpected status: {status:?}"),
            }
        }

        let session = store.list_provider_sessions(run_id).unwrap().pop().unwrap();
        assert_eq!(session.status(), ProviderSessionStatus::Completed);
        assert_eq!(
            session.native_session_id().unwrap().as_str(),
            "codex-thread-1"
        );
        let process = store
            .load_managed_process(session.current_process_id().unwrap())
            .unwrap();
        let argv = process
            .spec()
            .argv()
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(
            !argv
                .iter()
                .any(|arg| arg.contains("SUPER_SECRET_TASK_MARKER"))
        );
        assert!(
            std::fs::read_to_string(process.spec().stdin_path().unwrap())
                .unwrap()
                .contains("SUPER_SECRET_TASK_MARKER")
        );
        let events = store.load_events(run_id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event.event.kind(),
                    DomainEventKind::ProviderUsageUpdated {
                        input_units: 100,
                        output_units: 20,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event.event.kind(),
                    DomainEventKind::ProviderCompleted { .. }
                ))
                .count(),
            1
        );
        let artifacts = store.list_artifacts(run_id).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            artifacts[0].metadata().provider_id().unwrap().as_str(),
            "codex"
        );

        drop(store);
        let mut store = SqliteStore::open(database).unwrap();
        assert_eq!(
            store.load_run(run_id).unwrap().run.status(),
            RunStatus::Completed
        );
        assert_eq!(store.list_artifacts(run_id).unwrap().len(), 1);
    }

    #[test]
    fn turn_completion_transaction_failure_replays_batch_without_duplicate_usage() {
        let (_temp, _database, run_id, mut store, provider) = fixture(SUCCESS_OUTPUT);
        let mut engine = WorkflowEngine::new(provider, "fixture task");
        loop {
            let before = store.load_events(run_id).unwrap();
            let next_is_completion = before.iter().any(|event| {
                matches!(event.event.kind(), DomainEventKind::ProviderProgress { .. })
            });
            if next_is_completion {
                break;
            }
            let _ = engine.tick(&mut store, run_id).unwrap();
        }
        let session_before = store.list_provider_sessions(run_id).unwrap().pop().unwrap();
        let process_id = session_before.current_process_id().unwrap();
        let cursor_before = store
            .load_managed_process(process_id)
            .unwrap()
            .cursor(OutputStream::Stdout);
        let events_before = store.load_events(run_id).unwrap();
        store.install_event_insert_failure();
        assert!(engine.tick(&mut store, run_id).is_err());
        store.remove_event_insert_failure();

        assert_eq!(store.load_events(run_id).unwrap(), events_before);
        assert_eq!(
            store.list_provider_sessions(run_id).unwrap().pop().unwrap(),
            session_before
        );
        assert_eq!(
            store
                .load_managed_process(process_id)
                .unwrap()
                .cursor(OutputStream::Stdout),
            cursor_before
        );
        assert!(store.list_artifacts(run_id).unwrap().is_empty());

        assert!(matches!(
            engine.tick(&mut store, run_id).unwrap(),
            EngineStatus::Advanced { .. }
        ));
        let events = store.load_events(run_id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event.event.kind(),
                    DomainEventKind::ProviderUsageUpdated { .. }
                ))
                .count(),
            1
        );
        assert_eq!(store.list_artifacts(run_id).unwrap().len(), 1);
    }

    #[test]
    fn conflicting_thread_identity_fails_without_acknowledging_record() {
        let output = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-A\"}\n",
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-B\"}\n"
        );
        let (_temp, _database, run_id, mut store, provider) = fixture(output);
        let mut engine = WorkflowEngine::new(provider, "fixture task");
        for _ in 0..4 {
            let _ = engine.tick(&mut store, run_id).unwrap();
        }
        let session = store.list_provider_sessions(run_id).unwrap().pop().unwrap();
        let cursor = store
            .load_managed_process(session.current_process_id().unwrap())
            .unwrap()
            .cursor(OutputStream::Stdout);
        let error = engine.tick(&mut store, run_id).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("expected thread-A, received thread-B")
        );
        let session_after = store.list_provider_sessions(run_id).unwrap().pop().unwrap();
        assert_eq!(
            session_after.native_session_id().unwrap().as_str(),
            "thread-A"
        );
        assert_eq!(
            store
                .load_managed_process(session.current_process_id().unwrap())
                .unwrap()
                .cursor(OutputStream::Stdout),
            cursor
        );
    }

    #[test]
    fn unknown_record_checkpoints_but_invalid_json_keeps_cursor() {
        let unknown = concat!(
            "{\"type\":\"future.codex.event\",\"something\":123}\n",
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-A\"}\n"
        );
        let (_temp, _database, run_id, mut store, provider) = fixture(unknown);
        let mut engine = WorkflowEngine::new(provider, "fixture task");
        for _ in 0..3 {
            let _ = engine.tick(&mut store, run_id).unwrap();
        }
        let session = store.list_provider_sessions(run_id).unwrap().pop().unwrap();
        let process_id = session.current_process_id().unwrap();
        assert!(matches!(
            engine.tick(&mut store, run_id).unwrap(),
            EngineStatus::Advanced { .. }
        ));
        let checkpoint = store
            .load_managed_process(process_id)
            .unwrap()
            .cursor(OutputStream::Stdout);
        assert_eq!(
            checkpoint.offset(),
            u64::try_from(unknown.lines().next().unwrap().len() + 1).unwrap()
        );
        assert!(!store.load_events(run_id).unwrap().iter().any(|event| {
            matches!(event.event.kind(), DomainEventKind::ProviderStarted { .. })
        }));

        let (_temp, _database, run_id, mut store, provider) = fixture("{\"type\":\"broken\"\n");
        let mut engine = WorkflowEngine::new(provider, "fixture task");
        for _ in 0..3 {
            let _ = engine.tick(&mut store, run_id).unwrap();
        }
        let session = store.list_provider_sessions(run_id).unwrap().pop().unwrap();
        let process_id = session.current_process_id().unwrap();
        assert!(engine.tick(&mut store, run_id).is_err());
        assert_eq!(
            store
                .load_managed_process(process_id)
                .unwrap()
                .cursor(OutputStream::Stdout)
                .offset(),
            0
        );
        assert_eq!(
            store.list_provider_sessions(run_id).unwrap().pop().unwrap(),
            session
        );
    }

    #[test]
    fn lost_process_recovers_exact_thread_with_new_invocation() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        init_repository(&source);
        let database = temp.path().join("polycode.db");
        let process_root = temp.path().join("runs");
        let run_id = crate::domain::RunId::new();
        let created_at = CodexProvider::<RecoveryBackend>::now();
        let config_id = ConfigSnapshotId::new(format!("m8-{run_id}")).unwrap();
        let run = Run::new(
            run_id,
            WorkflowDefinition::built_in(WorkflowKind::Fast),
            config_id.clone(),
            created_at,
        );
        let input = RunInput::new(run_id, "recover task", created_at).unwrap();
        let config = codex_config(config_id, created_at);
        let created = run.created_event(EventMetadata::new(EventId::new(), created_at));
        let mut store = SqliteStore::open(&database).unwrap();
        store
            .create_run_with_input(&run, &input, &config, &[created])
            .unwrap();
        WorkspaceManager::new(temp.path().join("worktrees"))
            .prepare_run_workspace(&mut store, run_id, &source)
            .unwrap();
        let backend = RecoveryBackend::default();
        let invocations = Arc::clone(&backend.invocations);
        let provider = CodexProvider {
            id: ProviderId::new("codex").unwrap(),
            installation: CodexInstallation::fixture(PathBuf::from("/bin/true")),
            model: None,
            effort: EffortSetting::NativeDefault,
            manager: ProcessManager::new(&process_root, backend),
            artifact_root: process_root,
        };
        let mut engine = WorkflowEngine::new(provider, "recover task");
        loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::Interrupted { stages } => {
                    assert_eq!(
                        stages,
                        vec![crate::domain::StageId::new("implementation").unwrap()]
                    );
                    break;
                }
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("unexpected status: {status:?}"),
            }
        }
        engine
            .recover_stage(
                &mut store,
                run_id,
                &crate::domain::StageId::new("implementation").unwrap(),
            )
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
        let session = store.list_provider_sessions(run_id).unwrap().pop().unwrap();
        assert_eq!(session.invocation(), 2);
        assert_eq!(
            session.native_session_id().unwrap().as_str(),
            "recovery-thread"
        );
        assert_eq!(store.list_managed_processes(run_id).unwrap().len(), 2);
        let invocations = invocations.lock().unwrap();
        assert!(!invocations[0].iter().any(|arg| arg == "resume"));
        assert!(
            invocations[1]
                .windows(2)
                .any(|pair| pair == ["resume", "recovery-thread"])
        );
        assert!(!invocations[1].iter().any(|arg| arg == "--last"));
    }

    fn fixture(
        output: &str,
    ) -> (
        TempDir,
        PathBuf,
        crate::domain::RunId,
        SqliteStore,
        CodexProvider<FixtureBackend>,
    ) {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        init_repository(&source);
        let database = temp.path().join("polycode.db");
        let process_root = temp.path().join("runs");
        let run_id = crate::domain::RunId::new();
        let created_at = CodexProvider::<FixtureBackend>::now();
        let config_id = ConfigSnapshotId::new(format!("m8-{run_id}")).unwrap();
        let run = Run::new(
            run_id,
            WorkflowDefinition::built_in(WorkflowKind::Fast),
            config_id.clone(),
            created_at,
        );
        let input = RunInput::new(run_id, "fixture task", created_at).unwrap();
        let config = codex_config(config_id, created_at);
        let created = run.created_event(EventMetadata::new(EventId::new(), created_at));
        let mut store = SqliteStore::open(&database).unwrap();
        store
            .create_run_with_input(&run, &input, &config, &[created])
            .unwrap();
        WorkspaceManager::new(temp.path().join("worktrees"))
            .prepare_run_workspace(&mut store, run_id, &source)
            .unwrap();
        let provider = CodexProvider {
            id: ProviderId::new("codex").unwrap(),
            installation: CodexInstallation::fixture(PathBuf::from("/bin/true")),
            model: None,
            effort: EffortSetting::NativeDefault,
            manager: ProcessManager::new(&process_root, FixtureBackend::new(output)),
            artifact_root: process_root,
        };
        (temp, database, run_id, store, provider)
    }

    #[test]
    fn provider_accepts_specialized_reviewer_roles() {
        let (_temp, _database, _run_id, _store, provider) = fixture("");
        assert!(provider.supports_role(Role::CodeQualityReviewer));
        assert!(provider.supports_role(Role::SpecReviewer));
    }

    fn codex_config(
        config_id: ConfigSnapshotId,
        created_at: DateTime<Utc>,
    ) -> ResolvedConfigSnapshot {
        ResolvedConfigSnapshot::new(
            config_id,
            1,
            json!({
                "schema_version":1,
                "profile":"native_codex",
                "provider":"codex",
                "model":null,
                "provider_options":{
                    "execution_protocol":"exec_json_v1",
                    "sandbox_policy":"stage_kind_v1",
                    "approval_policy":"never"
                }
            }),
            created_at,
        )
        .unwrap()
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
