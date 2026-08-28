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
    AttentionKind, AttentionRequestId, EffortSetting, ModelId, ProviderId, ProviderSessionId, Role,
    StageKind,
};
use crate::engine::{
    Provider, ProviderAttentionContext, ProviderError, ProviderPoll, ProviderRequest,
    ProviderSignal,
};
use crate::process::{
    ManagedProcessStatus, OutputChunk, OutputStream, ProcessBackend, ProcessManager, TmuxBackend,
};
use crate::providers::{
    PendingProviderAttention, ProviderCommit, ProviderSessionMutation, ProviderSessionRecord,
    ProviderSessionRecordId, ProviderSessionStatus, change_handoff,
};
use crate::store::{SqliteStore, process_root};
use crate::workspace::WorkspaceStatus;

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
    effort: EffortSetting,
    manager: ProcessManager<B>,
    artifact_root: PathBuf,
    eval_auto_approve: bool,
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
            effort: EffortSetting::NativeDefault,
            manager: ProcessManager::from_environment()?,
            artifact_root: root,
            eval_auto_approve: false,
        })
    }

    pub(crate) fn from_runtime(
        model: Option<ModelId>,
        root: PathBuf,
        runner_executable: PathBuf,
    ) -> Result<Self, ClaudeProviderError> {
        Self::from_runtime_with_eval_policy(model, root, runner_executable, false)
    }

    pub(crate) fn from_runtime_with_eval_policy(
        model: Option<ModelId>,
        root: PathBuf,
        runner_executable: PathBuf,
        eval_auto_approve: bool,
    ) -> Result<Self, ClaudeProviderError> {
        let installation = ClaudeInstallation::discover()?;
        installation.require_authenticated()?;
        Ok(Self {
            id: ProviderId::new("claude")
                .map_err(|error| ClaudeProviderError::Protocol(error.to_string()))?,
            installation,
            model,
            effort: EffortSetting::NativeDefault,
            manager: ProcessManager::new(&root, TmuxBackend::new(runner_executable)),
            artifact_root: root,
            eval_auto_approve,
        })
    }
}

impl<B: ProcessBackend> ClaudeProvider<B> {
    #[must_use]
    pub const fn installation(&self) -> &ClaudeInstallation {
        &self.installation
    }

    /// Sets the requested effort translated onto native `--effort`.
    /// `NativeDefault` keeps invocations byte-identical to pre-effort policy.
    #[must_use]
    pub fn with_effort(mut self, effort: EffortSetting) -> Self {
        self.effort = effort;
        self
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
            let handoff = change_handoff::for_request(store, request)?;
            command::initial(
                &prompt::compose(request, &artifacts, handoff.as_ref())?,
                self.model.as_ref(),
                self.effort,
            )
        } else {
            let native = session.native_session_id().ok_or_else(|| {
                ClaudeProviderError::Protocol("resume has no native session ID".to_owned())
            })?;
            let denials = session
                .pending_attention()
                .map(|pending| Self::pending_new_denials(store, &session, pending))
                .transpose()?
                .unwrap_or_default();
            let denials = if self.eval_auto_approve {
                // Disposable eval never grants anything except the exact safe
                // Edit/Write subset. Historical/denied Bash stays denied.
                let context = ProviderAttentionContext::new(
                    request.run_id(),
                    request.stage_id().clone(),
                    request.stage_kind(),
                    request.role(),
                    session
                        .pending_attention()
                        .map(PendingProviderAttention::attention_id)
                        .unwrap_or_default(),
                );
                Self::safe_eval_grants(store, &context, &denials)?.ok_or_else(|| {
                    ClaudeProviderError::UnsafePermission(
                        "eval permission is not an exact in-worktree Edit/Write".to_owned(),
                    )
                })?
            } else {
                denials
            };
            let response = session
                .pending_attention()
                .map(|pending| self.read_response(session.id(), pending.attention_id()))
                .transpose()?
                .flatten();
            command::resume(
                native,
                &denials,
                response.as_deref(),
                self.model.as_ref(),
                self.effort,
            )?
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
        // Terminal result records carry the invocation's authoritative usage;
        // it is emitted atomically ahead of the terminal signal so replay
        // cannot split usage from the outcome (mirrors Codex turn.completed).
        let mut terminal_usage = None;
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
                session_id: _,
                usage,
            } => {
                terminal_usage = usage;
                // Claude may repeat earlier denials in a resumed terminal result;
                // only requests not seen by an earlier invocation are unresolved.
                // Re-attempted requests carry fresh tool_use_ids and count as new.
                let unresolved = Self::new_denials(store, &session, denials)?;
                // Disposable eval never grants Bash; a denied call never ran, so
                // only mutation/question/unknown requests block a successful
                // eval terminal. Production keeps stage-kind conservative rules.
                let needs_user = !unresolved.is_empty()
                    && (!success
                        || unresolved.iter().any(|denial| {
                            if self.eval_auto_approve {
                                denial.requires_eval_terminal_attention()
                            } else {
                                denial.requires_terminal_attention(request.stage_kind())
                            }
                        }));
                if needs_user {
                    let attention_id = AttentionRequestId::new();
                    let summary = permission_summary(&unresolved);
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
                    ProviderSignal::NeedsUser {
                        kind: AttentionKind::Permission,
                        summary,
                        request_id: Some(attention_id),
                    }
                } else if success {
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
                } else {
                    session
                        .fail(Self::now())
                        .map_err(|error| ClaudeProviderError::Protocol(error.to_owned()))?;
                    commit = commit.with_session(ProviderSessionMutation::new(session, expected));
                    ProviderSignal::Failed(
                        error.unwrap_or_else(|| "Claude Code execution failed".to_owned()),
                    )
                }
            }
            ClaudeRecord::Ignored => return Ok(ProviderPoll::Checkpoint(commit)),
        };
        let signals = terminal_usage
            .map(ProviderSignal::Usage)
            .into_iter()
            .chain(std::iter::once(signal))
            .collect();
        Ok(ProviderPoll::Emission { signals, commit })
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

    /// Denials already surfaced by earlier invocations of the same native
    /// session. Derived from durable process output, so no extra state is
    /// needed and a crash between resume and next terminal cannot lose it.
    fn historical_denials(
        store: &SqliteStore,
        session: &ProviderSessionRecord,
    ) -> Result<Vec<PermissionDenial>, ClaudeProviderError> {
        if session.invocation() <= 1 {
            return Ok(Vec::new());
        }
        let mut historical = Vec::new();
        for process in store.list_managed_processes(session.run_id())? {
            if process.stage_id() != session.stage_id()
                || process.attempt() != session.attempt()
                || process.invocation() >= session.invocation()
            {
                continue;
            }
            let bytes = match std::fs::read(process.spec().stdout_path()) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let mut rest = bytes.as_slice();
            // Output past the terminal record was never validated; stop at the
            // first undecodable line instead of failing the current invocation.
            while let Ok(Some((record, consumed))) = first_record(rest) {
                if let ClaudeRecord::Result { denials, .. }
                | ClaudeRecord::NeedsUser { denials, .. } = record
                {
                    historical.extend(denials);
                }
                rest = &rest[consumed..];
            }
        }
        Ok(historical)
    }

    /// Splits cumulative denial history into requests not yet observed.
    fn new_denials(
        store: &SqliteStore,
        session: &ProviderSessionRecord,
        denials: Vec<PermissionDenial>,
    ) -> Result<Vec<PermissionDenial>, ClaudeProviderError> {
        let historical = Self::historical_denials(store, session)?;
        Ok(denials
            .into_iter()
            .filter(|denial| !historical.iter().any(|seen| seen.same_request(denial)))
            .collect())
    }

    fn pending_new_denials(
        store: &SqliteStore,
        session: &ProviderSessionRecord,
        pending: &PendingProviderAttention,
    ) -> Result<Vec<PermissionDenial>, ClaudeProviderError> {
        let denials = Self::read_pending_denials(store, pending)?;
        Self::new_denials(store, session, denials)
    }

    /// Exact safe Edit/Write subset a disposable eval may grant, or `None` when
    /// this attention needs a human.
    ///
    /// Policy: at least one exact Edit/Write inside the eval worktree, no
    /// mutation target outside it (a mixed request signals intent to escape,
    /// so the safe subset does not continue either), and no question. Bash
    /// and other denials neither veto nor get granted: they stay denied history.
    fn safe_eval_grants(
        store: &SqliteStore,
        context: &ProviderAttentionContext,
        denials: &[PermissionDenial],
    ) -> Result<Option<Vec<PermissionDenial>>, ClaudeProviderError> {
        if context.role() != Role::Implementer
            || !matches!(
                context.stage_kind(),
                StageKind::Implementation | StageKind::Fix
            )
        {
            return Ok(None);
        }
        let Some(workspace) = Self::eval_workspace_path(store, context.run_id())? else {
            return Ok(None);
        };
        if denials.iter().any(PermissionDenial::is_question) {
            return Ok(None);
        }
        let safe_edits = denials
            .iter()
            .filter(|denial| denial.is_safe_eval_edit(&workspace))
            .cloned()
            .collect::<Vec<_>>();
        let unsafe_mutation = denials
            .iter()
            .any(|denial| denial.is_mutating_tool() && !denial.is_safe_eval_edit(&workspace));
        if safe_edits.is_empty() || unsafe_mutation {
            return Ok(None);
        }
        Ok(Some(safe_edits))
    }

    fn safe_eval_permission(
        store: &SqliteStore,
        context: &ProviderAttentionContext,
        denials: &[PermissionDenial],
    ) -> Result<bool, ClaudeProviderError> {
        Ok(Self::safe_eval_grants(store, context, denials)?.is_some())
    }

    fn eval_workspace_path(
        store: &SqliteStore,
        run_id: crate::domain::RunId,
    ) -> Result<Option<PathBuf>, ClaudeProviderError> {
        let Some(workspace) = store.load_workspace(run_id)? else {
            return Ok(None);
        };
        if workspace.status() != WorkspaceStatus::Ready {
            return Ok(None);
        }
        let (Ok(worktree), Ok(source)) = (
            std::fs::canonicalize(workspace.worktree_path()),
            std::fs::canonicalize(workspace.source_repo_path()),
        ) else {
            return Ok(None);
        };
        if worktree == source || worktree.starts_with(&source) {
            return Ok(None);
        }
        Ok(Some(worktree))
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
    fn provider_id_for(&self, _request: &ProviderRequest) -> Result<ProviderId, ProviderError> {
        Ok(self.id.clone())
    }

    fn supports_role(&self, _role: Role) -> bool {
        true
    }

    fn keep_attached_for(&self, _request: &ProviderRequest) -> Result<bool, ProviderError> {
        Ok(true)
    }

    fn stage_attention_response(
        &mut self,
        store: &mut SqliteStore,
        context: &ProviderAttentionContext,
        response: Option<&str>,
    ) -> Result<(), ProviderError> {
        let result = (|| -> Result<(), ClaudeProviderError> {
            let session = store
                .list_provider_sessions(context.run_id())?
                .into_iter()
                .find(|session| {
                    session.stage_id() == context.stage_id()
                        && session.provider_id() == &self.id
                        && session
                            .pending_attention()
                            .is_some_and(|pending| pending.attention_id() == context.request_id())
                })
                .ok_or_else(|| {
                    ClaudeProviderError::Protocol(
                        "attention has no matching Claude provider session".to_owned(),
                    )
                })?;
            let pending = session
                .pending_attention()
                .expect("matched pending attention");
            let denials = Self::pending_new_denials(store, &session, pending)?;
            if self.eval_auto_approve
                && response.is_none()
                && !Self::safe_eval_permission(store, context, &denials)?
            {
                return Err(ClaudeProviderError::UnsafePermission(
                    "eval permission is not an exact in-worktree Edit/Write".to_owned(),
                ));
            }
            if denials
                .iter()
                .any(|denial| denial.tool_name == "AskUserQuestion")
            {
                self.write_response_once(
                    session.id(),
                    context.request_id(),
                    response.ok_or(ClaudeProviderError::QuestionResponseRequired)?,
                )?;
            }
            Ok(())
        })();
        result.map_err(|error| ProviderError::new(error.to_string()))
    }

    fn can_auto_resolve_attention(
        &mut self,
        store: &mut SqliteStore,
        context: &ProviderAttentionContext,
    ) -> Result<bool, ProviderError> {
        if !self.eval_auto_approve {
            return Ok(false);
        }
        let session = store
            .list_provider_sessions(context.run_id())
            .map_err(|error| ProviderError::new(error.to_string()))?
            .into_iter()
            .find(|session| {
                session.stage_id() == context.stage_id()
                    && session.provider_id() == &self.id
                    && session
                        .pending_attention()
                        .is_some_and(|pending| pending.attention_id() == context.request_id())
            });
        let Some(session) = session else {
            return Ok(false);
        };
        let pending = session.pending_attention().ok_or_else(|| {
            ProviderError::new("matched attention has no pending provider record")
        })?;
        let denials = Self::pending_new_denials(store, &session, pending)
            .map_err(|error| ProviderError::new(error.to_string()))?;
        Self::safe_eval_permission(store, context, &denials)
            .map_err(|error| ProviderError::new(error.to_string()))
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
    use std::io::SeekFrom;
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    use chrono::Utc;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::domain::{
        AttentionKind, ConfigSnapshotId, EventId, EventMetadata, Run, RunStatus, StageDefinition,
        WorkflowDefinition, WorkflowKind,
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
        first_result: Arc<Mutex<Option<String>>>,
        /// Terminal result record per invocation number; overrides defaults.
        scripted: Arc<Mutex<BTreeMap<u32, String>>>,
        /// Files written when an invocation starts, simulating an approved
        /// native Edit taking effect in the worktree.
        effects: Arc<Mutex<BTreeMap<u32, (PathBuf, String)>>>,
    }

    impl FixtureBackend {
        fn with_first_result(result: impl Into<String>) -> Self {
            Self {
                first_result: Arc::new(Mutex::new(Some(result.into()))),
                ..Self::default()
            }
        }

        fn set_first_result(&self, result: impl Into<String>) {
            *self.first_result.lock().unwrap() = Some(result.into());
        }

        fn set_result(&self, invocation: u32, result: impl Into<String>) {
            self.scripted
                .lock()
                .unwrap()
                .insert(invocation, result.into());
        }

        fn set_edit_effect(&self, invocation: u32, path: PathBuf, content: impl Into<String>) {
            self.effects
                .lock()
                .unwrap()
                .insert(invocation, (path, content.into()));
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
            if let Some((path, content)) = self.effects.lock().unwrap().get(&process.invocation()) {
                std::fs::write(path, content)?;
            }
            let scripted = self
                .scripted
                .lock()
                .unwrap()
                .get(&process.invocation())
                .cloned();
            let output = if let Some(result) = scripted {
                format!(
                    "{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"native-session-1\",\"model\":\"fixture-model\"}}\n{result}\n"
                )
            } else if process.invocation() == 1 {
                let result = self
                    .first_result
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| {
                        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\",\"session_id\":\"native-session-1\",\"permission_denials\":[{\"tool_name\":\"Write\",\"tool_input\":{\"file_path\":\"/tmp/fixture\"}}]}".to_owned()
                    });
                format!(
                    "{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"native-session-1\",\"model\":\"fixture-model\"}}\n{result}\n"
                )
            } else {
                concat!(
                    "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"native-session-1\",\"model\":\"fixture-model\"}\n",
                    "{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":3},\"content\":[]}}\n",
                    "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"# Completed\\nFixture result\",\"session_id\":\"native-session-1\",\"permission_denials\":[]}\n"
                )
                .to_owned()
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
    fn recovered_bash_denial_completes_and_persists_artifact() {
        let backend = FixtureBackend::with_first_result(
            json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": "# Plan mismatch\n```json\n{\"eval_outcome\":\"plan_mismatch\"}\n```",
                "session_id": "native-session-1",
                "permission_denials": [{"tool_name":"Bash","tool_input":{"command":"cargo test && cargo clippy"}}]
            })
            .to_string(),
        );
        let (temp, _database, run_id, mut store, provider) = fixture_with_backend(backend, false);
        let mut engine = WorkflowEngine::new(provider, "plan mismatch");
        loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::Finished {
                    run_status: RunStatus::Completed,
                } => break,
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("recovered denial stranded run: {status:?}"),
            }
        }
        assert_eq!(store.list_artifacts(run_id).unwrap().len(), 1);
        drop(temp);
    }

    #[test]
    fn blocked_edit_stays_needs_user_before_resolution() {
        let (_temp, _database, run_id, mut store, provider) = fixture();
        let mut engine = WorkflowEngine::new(provider, "blocked edit");
        let request = loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::NeedsUser { requests } => break requests[0],
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("unexpected status: {status:?}"),
            }
        };
        let loaded = store.load_run(run_id).unwrap();
        let attention = loaded
            .run
            .attention_requests()
            .iter()
            .find(|attention| attention.id() == request)
            .unwrap();
        assert_eq!(attention.kind(), AttentionKind::Permission);
        assert_eq!(
            store.load_run(run_id).unwrap().run.status(),
            RunStatus::NeedsUser
        );
        assert!(store.list_artifacts(run_id).unwrap().is_empty());
    }

    #[test]
    fn eval_auto_resolution_allows_only_exact_edit_inside_worktree() {
        let backend = FixtureBackend::default();
        let (_temp, _database, run_id, mut store, provider) =
            fixture_with_backend(backend.clone(), true);
        let workspace = store.load_workspace(run_id).unwrap().unwrap();
        let target = workspace.worktree_path().join("README.md");
        backend.set_first_result(
            json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": "blocked edit",
                "session_id": "native-session-1",
                "permission_denials": [{"tool_name":"Edit","tool_input":{"file_path":target}}]
            })
            .to_string(),
        );
        let mut engine = WorkflowEngine::new(provider, "safe edit");
        let request = loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::NeedsUser { requests } => break requests[0],
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("unexpected status: {status:?}"),
            }
        };
        assert!(
            engine
                .can_auto_resolve_attention(&mut store, run_id, request)
                .unwrap()
        );
        engine
            .resolve_attention(&mut store, run_id, request)
            .unwrap();
        loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::Finished {
                    run_status: RunStatus::Completed,
                } => break,
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("safe eval permission did not resume: {status:?}"),
            }
        }
        let processes = store.list_managed_processes(run_id).unwrap();
        let resumed = processes.last().unwrap();
        let expected = format!("Edit(/{}", target.display());
        assert!(
            resumed
                .spec()
                .argv()
                .iter()
                .any(|arg| { arg.to_string_lossy().starts_with(&expected) })
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one deterministic two-invocation sequence exercises usage, native breakdown, latency, and prompt-byte evidence together"
    )]
    fn terminal_usage_latency_and_prompt_bytes_are_observable_per_invocation() {
        let backend = FixtureBackend::default();
        let (_temp, database, run_id, mut store, provider) =
            fixture_with_backend(backend.clone(), true);
        let workspace = store.load_workspace(run_id).unwrap().unwrap();
        let target = workspace.worktree_path().join("README.md");
        // Invocation 1: real-shape terminal usage plus one safe Edit denial.
        backend.set_first_result(
            json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": "blocked edit",
                "session_id": "native-session-1",
                "permission_denials": [{"tool_name":"Edit","tool_input":{"file_path":target}}],
                "usage": {
                    "input_tokens": 8,
                    "output_tokens": 1221,
                    "cache_read_input_tokens": 153_292,
                    "cache_creation_input_tokens": 3638
                },
                "modelUsage": {
                    "claude-fable-5": {
                        "inputTokens": 8,
                        "outputTokens": 1221,
                        "cacheReadInputTokens": 153_292,
                        "cacheCreationInputTokens": 3638
                    }
                }
            })
            .to_string(),
        );
        // Invocation 2 (continuation after auto-approval): its own usage.
        backend.set_result(
            2,
            json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": "# Completed\napplied",
                "session_id": "native-session-1",
                "permission_denials": [{"tool_name":"Edit","tool_input":{"file_path":target}}],
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 40,
                    "cache_read_input_tokens": 160_000
                },
                "modelUsage": {
                    "claude-fable-5": {
                        "inputTokens": 3,
                        "outputTokens": 40,
                        "cacheReadInputTokens": 160_000
                    }
                }
            })
            .to_string(),
        );
        let mut engine = WorkflowEngine::new(provider, "safe edit");
        let request = loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::NeedsUser { requests } => break requests[0],
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("unexpected status: {status:?}"),
            }
        };
        engine
            .resolve_attention(&mut store, run_id, request)
            .unwrap();
        loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::Finished {
                    run_status: RunStatus::Completed,
                } => break,
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("continuation did not complete: {status:?}"),
            }
        }

        // Exactly one usage event per terminal result; replay-safe commit
        // means no duplicates despite the intermediate NeedsUser round trip.
        let events = store.load_events(run_id).unwrap();
        let usage_events = events
            .iter()
            .filter_map(|event| match event.event.kind() {
                crate::domain::DomainEventKind::ProviderUsageUpdated {
                    input_units,
                    output_units,
                    cache_read_units,
                    cache_write_units,
                    native_models,
                    ..
                } => Some((
                    *input_units,
                    *output_units,
                    *cache_read_units,
                    *cache_write_units,
                    native_models.clone(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(usage_events.len(), 2);
        assert_eq!(usage_events[0].0, 8);
        assert_eq!(usage_events[0].2, Some(153_292));
        assert_eq!(usage_events[1].1, 40);
        // Second invocation reported no cache-write: unavailable, not zero.
        assert_eq!(usage_events[1].3, None);

        let stage_id = crate::domain::StageId::new("implementation").unwrap();
        let evidence =
            crate::app::query::stage_execution_evidence(&mut store, run_id, &stage_id).unwrap();
        // Aggregate folds only the authoritative terminal usage totals; the
        // native per-model breakdown is merged separately and never summed in.
        assert_eq!(evidence.usage.input_units, 11);
        assert_eq!(evidence.usage.output_units, 1261);
        assert_eq!(evidence.usage.cache_read_units, Some(313_292));
        assert_eq!(evidence.usage.cache_write_units, Some(3638));
        assert_eq!(evidence.usage.reasoning_output_units, None);
        let native = evidence.native_model_usage.clone().unwrap();
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].model, "claude-fable-5");
        assert_eq!(native[0].input_units, 11);
        assert_eq!(native[0].output_units, 1261);
        assert_eq!(native[0].cache_read_units, Some(313_292));

        // Invocation telemetry: two persisted invocations; injected prompt
        // bytes equal the exact stdin bytes piped into each native process,
        // with initial and resume invocations independently attributable.
        let processes = store.list_managed_processes(run_id).unwrap();
        assert_eq!(processes.len(), 2);
        assert_eq!(evidence.invocation_count, 2);
        let stdin_sizes = processes
            .iter()
            .map(|process| {
                let path = process.spec().stdin_path().unwrap();
                std::fs::metadata(path).unwrap().len()
            })
            .collect::<Vec<_>>();
        let initial = std::fs::read_to_string(processes[0].spec().stdin_path().unwrap()).unwrap();
        assert!(initial.starts_with("# Polycode stage"));
        let resume = std::fs::read_to_string(processes[1].spec().stdin_path().unwrap()).unwrap();
        assert!(resume.contains("approved the exact pending permission"));
        assert_eq!(
            evidence.injected_prompt_bytes,
            Some(stdin_sizes.iter().sum::<u64>())
        );

        // Latency: provider execution span from first ProviderStarted to the
        // final terminal provider event, deterministic from persisted events.
        let expected = u64::try_from(
            evidence
                .finished_at
                .unwrap()
                .signed_duration_since(evidence.started_at.unwrap())
                .num_milliseconds(),
        )
        .unwrap();
        assert_eq!(evidence.latency_ms, Some(expected));
        let mut reopened = SqliteStore::open(&database).unwrap();
        let replayed =
            crate::app::query::stage_execution_evidence(&mut reopened, run_id, &stage_id).unwrap();
        assert_eq!(replayed, evidence);
    }

    #[test]
    fn eval_auto_resolution_rejects_edit_outside_worktree() {
        let backend = FixtureBackend::with_first_result(
            json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": "blocked edit",
                "session_id": "native-session-1",
                "permission_denials": [{"tool_name":"Edit","tool_input":{"file_path":"/tmp/other-repo/file"}}]
            })
            .to_string(),
        );
        let (_temp, _database, run_id, mut store, provider) = fixture_with_backend(backend, true);
        let mut engine = WorkflowEngine::new(provider, "escape");
        let request = loop {
            match engine.drive(&mut store, run_id).unwrap() {
                EngineStatus::NeedsUser { requests } => break requests[0],
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("unexpected status: {status:?}"),
            }
        };
        assert!(
            !engine
                .can_auto_resolve_attention(&mut store, run_id, request)
                .unwrap()
        );
        assert!(
            engine
                .resolve_attention(&mut store, run_id, request)
                .is_err()
        );
        assert_eq!(
            store.load_run(run_id).unwrap().run.status(),
            RunStatus::NeedsUser
        );
    }

    // ---- Real-shape regression fixtures (copied structurally from native
    // role_core_v3 Claude logs). Worktree paths are substituted at runtime.

    const REAL_QUALITY_PLANTED_BASH: &str =
        "cd \"WORKTREE\" && cargo test 2>&1 | tail -8; cargo clippy --all-targets 2>&1 | tail -30";
    const REAL_SPEC_CLEAN_BASH: &str =
        "cd \"WORKTREE\" && cat Cargo.toml && cargo build 2>&1 | tail -3";
    const REAL_QUALITY_CLEAN_BASH: &str = "cd \"WORKTREE\" && git ls-files && echo --- && for f in $(git ls-files); do echo \"=== $f\"; cat \"$f\"; done";
    const REAL_IMPLEMENTER_SED_BASH: &str = "cd \"WORKTREE\" && sed -i '' 's/    value + 2/    value * 2/' src/lib.rs && git diff && cargo test 2>&1 | tail -15";
    const REAL_IMPLEMENTER_TEST_BASH: &str = "cargo test 2>&1 | tail -20";

    fn success_result(result: &str, denials: &serde_json::Value) -> String {
        json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": result,
            "session_id": "native-session-1",
            "permission_denials": denials
        })
        .to_string()
    }

    fn bash_denial(id: &str, command: &str, worktree: &Path) -> serde_json::Value {
        json!({
            "tool_name": "Bash",
            "tool_use_id": id,
            "tool_input": {"command": command.replace("WORKTREE", &worktree.to_string_lossy())}
        })
    }

    fn edit_denial(tool: &str, id: &str, path: &Path) -> serde_json::Value {
        json!({
            "tool_name": tool,
            "tool_use_id": id,
            "tool_input": {"file_path": path, "old_string": "a", "new_string": "b"}
        })
    }

    fn drive_to_completion(
        engine: &mut WorkflowEngine<ClaudeProvider<FixtureBackend>>,
        store: &mut SqliteStore,
        run_id: crate::domain::RunId,
    ) {
        loop {
            match engine.drive(store, run_id).unwrap() {
                EngineStatus::Finished {
                    run_status: RunStatus::Completed,
                } => break,
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("run did not complete: {status:?}"),
            }
        }
    }

    fn drive_to_attention(
        engine: &mut WorkflowEngine<ClaudeProvider<FixtureBackend>>,
        store: &mut SqliteStore,
        run_id: crate::domain::RunId,
    ) -> AttentionRequestId {
        loop {
            match engine.drive(store, run_id).unwrap() {
                EngineStatus::NeedsUser { requests } => break requests[0],
                EngineStatus::Advanced { .. } | EngineStatus::WaitingForProvider { .. } => {}
                status => panic!("run did not need user: {status:?}"),
            }
        }
    }

    fn all_argv(store: &SqliteStore, run_id: crate::domain::RunId) -> Vec<String> {
        store
            .list_managed_processes(run_id)
            .unwrap()
            .iter()
            .flat_map(|process| {
                process
                    .spec()
                    .argv()
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn assert_no_bash_or_broad_grants(argv: &[String]) {
        assert!(
            !argv.iter().any(|arg| arg.starts_with("Bash(")),
            "Bash must never be granted: {argv:?}"
        );
        assert!(
            !argv
                .iter()
                .any(|arg| arg == "Edit" || arg == "Write" || arg.contains('*')),
            "broad/wildcard grants are forbidden: {argv:?}"
        );
        assert!(
            !argv
                .iter()
                .any(|arg| arg.contains("dangerously") || arg.contains("bypassPermissions")),
            "no bypass flags: {argv:?}"
        );
    }

    fn reviewer_completes_with_denied_bash_history(
        kind: StageKind,
        role: Role,
        command: &str,
        result: &str,
        eval_auto_approve: bool,
    ) {
        let backend = FixtureBackend::default();
        let (_temp, _database, run_id, mut store, provider) = fixture_with_workflow(
            backend.clone(),
            eval_auto_approve,
            review_workflow(kind, role),
        );
        let worktree = store
            .load_workspace(run_id)
            .unwrap()
            .unwrap()
            .worktree_path()
            .to_path_buf();
        backend.set_first_result(success_result(
            result,
            &json!([bash_denial(
                "toolu_01DXupP8fj8XEo2X61Wz722z",
                command,
                &worktree
            )]),
        ));
        let mut engine = WorkflowEngine::new(provider, "review");
        drive_to_completion(&mut engine, &mut store, run_id);
        assert_eq!(store.list_managed_processes(run_id).unwrap().len(), 1);
        let artifacts = store.list_artifacts(run_id).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert!(
            std::fs::read_to_string(artifacts[0].path())
                .unwrap()
                .contains(result.lines().next().unwrap())
        );
        assert_no_bash_or_broad_grants(&all_argv(&store, run_id));
    }

    #[test]
    fn real_quality_reviewer_recovered_cargo_diagnostic_completes_without_bash_grant() {
        for eval in [false, true] {
            reviewer_completes_with_denied_bash_history(
                StageKind::CodeQualityReview,
                Role::CodeQualityReviewer,
                REAL_QUALITY_PLANTED_BASH,
                "# Code Quality Review\n\n## Must fix\n\n- `src/lib.rs:28-42` — nested unwrap_or_default.\n",
                eval,
            );
        }
    }

    #[test]
    fn real_spec_reviewer_clean_review_completes_with_denied_build_history() {
        for eval in [false, true] {
            reviewer_completes_with_denied_bash_history(
                StageKind::SpecReview,
                Role::SpecReviewer,
                REAL_SPEC_CLEAN_BASH,
                "# Specification Review\n\n```json\n{\"findings\":[]}\n```\n",
                eval,
            );
        }
    }

    #[test]
    fn real_quality_clean_compound_shell_history_does_not_strand_review() {
        for eval in [false, true] {
            reviewer_completes_with_denied_bash_history(
                StageKind::CodeQualityReview,
                Role::CodeQualityReviewer,
                REAL_QUALITY_CLEAN_BASH,
                "# Code Quality Review\n\n```json\n{\"findings\":[]}\n```\n",
                eval,
            );
        }
    }

    #[test]
    fn reviewer_edit_denial_is_not_historical_success() {
        for tool in ["Edit", "Write"] {
            let backend = FixtureBackend::default();
            let (_temp, _database, run_id, mut store, provider) = fixture_with_workflow(
                backend.clone(),
                true,
                review_workflow(StageKind::CodeQualityReview, Role::CodeQualityReviewer),
            );
            let worktree = store
                .load_workspace(run_id)
                .unwrap()
                .unwrap()
                .worktree_path()
                .to_path_buf();
            backend.set_first_result(success_result(
                "# Code Quality Review\n",
                &json!([
                    bash_denial("toolu_bash", REAL_QUALITY_PLANTED_BASH, &worktree),
                    edit_denial(tool, "toolu_edit", &worktree.join("src/lib.rs")),
                ]),
            ));
            let mut engine = WorkflowEngine::new(provider, "review");
            let request = drive_to_attention(&mut engine, &mut store, run_id);
            assert!(
                !engine
                    .can_auto_resolve_attention(&mut store, run_id, request)
                    .unwrap()
            );
            assert_eq!(
                store.load_run(run_id).unwrap().run.status(),
                RunStatus::NeedsUser
            );
            assert!(store.list_artifacts(run_id).unwrap().is_empty());
        }
    }

    #[test]
    fn real_implementer_cumulative_history_grants_only_exact_edit_and_completes() {
        let backend = FixtureBackend::default();
        let (_temp, _database, run_id, mut store, provider) =
            fixture_with_backend(backend.clone(), true);
        let worktree = store
            .load_workspace(run_id)
            .unwrap()
            .unwrap()
            .worktree_path()
            .to_path_buf();
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(
            worktree.join("src/lib.rs"),
            "pub fn double(value: i32) -> i32 { value + 2 }\n",
        )
        .unwrap();
        let target = worktree.join("src/lib.rs");
        let first_history = json!([
            edit_denial("Edit", "toolu_01Porm8WSKehWnTGkMPEgZTz", &target),
            edit_denial("Write", "toolu_01YXn15DfzQyaiUK46xchHai", &target),
            bash_denial(
                "toolu_01XeeDkiuxT8FYX32WiuAaw1",
                REAL_IMPLEMENTER_SED_BASH,
                &worktree
            ),
            bash_denial(
                "toolu_01VMux5SVAkuf5HqmXcTe93A",
                REAL_IMPLEMENTER_TEST_BASH,
                &worktree
            ),
        ]);
        backend.set_first_result(success_result("Blocked. Report:\n", &first_history));
        // Resumed invocation: cumulative history repeats every earlier denial
        // (same tool_use_ids) plus one new diagnostic retry.
        let mut second_history = first_history.as_array().unwrap().clone();
        second_history.push(bash_denial(
            "toolu_01NEWcargoTestRetry",
            REAL_IMPLEMENTER_TEST_BASH,
            &worktree,
        ));
        backend.set_result(
            2,
            success_result(
                "# Completed\nApplied `value * 2`.\n",
                &serde_json::Value::Array(second_history),
            ),
        );
        let mut engine = WorkflowEngine::new(provider, "fix double");
        let request = drive_to_attention(&mut engine, &mut store, run_id);
        assert!(
            engine
                .can_auto_resolve_attention(&mut store, run_id, request)
                .unwrap()
        );
        engine
            .resolve_attention(&mut store, run_id, request)
            .unwrap();
        drive_to_completion(&mut engine, &mut store, run_id);

        let processes = store.list_managed_processes(run_id).unwrap();
        assert_eq!(processes.len(), 2);
        let resumed = processes
            .last()
            .unwrap()
            .spec()
            .argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            resumed
                .windows(2)
                .any(|pair| pair == ["--resume", "native-session-1"]),
            "{resumed:?}"
        );
        let expected_rule = format!("Edit(/{})", target.display());
        let rules = resumed
            .iter()
            .skip_while(|arg| *arg != "--allowedTools")
            .skip(1)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(rules, vec![expected_rule], "{resumed:?}");
        assert_no_bash_or_broad_grants(&resumed);
        let sessions = store.list_provider_sessions(run_id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].invocation(), 2);
        assert_eq!(sessions[0].status(), ProviderSessionStatus::Completed);
        assert_eq!(store.list_artifacts(run_id).unwrap().len(), 1);
    }

    const REAL_BUGFIX_INV1_BASH: &str = "sed -i '' 's/    value + 2/    value * 2/' src/lib.rs && cargo test 2>&1 | tail -20 && git diff";
    const REAL_BUGFIX_INV2_TEST_BASH: &str = "cargo test 2>&1 | tail -15";
    const REAL_BUGFIX_INV2_SUBAGENT_BASH: &str = "cargo test 2>&1 | tail -60";
    const BROKEN_LIB: &str = "pub fn double(value: i32) -> i32 {\n    value + 2\n}\n";
    const FIXED_LIB: &str = "pub fn double(value: i32) -> i32 {\n    value * 2\n}\n";

    /// Real `basic_bugfix` invocation-1 history (session 0614aad3…): exact Edit,
    /// exact Write, and one compound mutating Bash alternative.
    fn bugfix_invocation_one(worktree: &Path, target: &Path) -> serde_json::Value {
        json!([
            edit_denial("Edit", "toolu_01EGrNZfzW9jFYc3agZxCvYW", target),
            edit_denial("Write", "toolu_01KF1yG8dn2fVGu88Jpfv9BX", target),
            bash_denial(
                "toolu_0196jwV8idmpppL66Jrszret",
                REAL_BUGFIX_INV1_BASH,
                worktree
            ),
        ])
    }

    /// Real `basic_bugfix` invocation-2 history: re-attempted sed (new id, still
    /// denied, never executed), Claude's cargo test, subagent's cargo test.
    fn bugfix_invocation_two(worktree: &Path) -> Vec<serde_json::Value> {
        vec![
            bash_denial(
                "toolu_01SiSgSWADTsxsGnyCVJCzMZ",
                REAL_BUGFIX_INV1_BASH,
                worktree,
            ),
            bash_denial(
                "toolu_01Tzj2TyFaCU1zYTfpXVdMJk",
                REAL_BUGFIX_INV2_TEST_BASH,
                worktree,
            ),
            bash_denial(
                "toolu_011up88dJSQKrubWJ9Zc7RKq",
                REAL_BUGFIX_INV2_SUBAGENT_BASH,
                worktree,
            ),
        ]
    }

    fn bugfix_fixture(
        eval_auto_approve: bool,
    ) -> (
        FixtureBackend,
        TempDir,
        crate::domain::RunId,
        SqliteStore,
        ClaudeProvider<FixtureBackend>,
        PathBuf,
        PathBuf,
    ) {
        let backend = FixtureBackend::default();
        let (temp, _database, run_id, store, provider) =
            fixture_with_backend(backend.clone(), eval_auto_approve);
        let worktree = store
            .load_workspace(run_id)
            .unwrap()
            .unwrap()
            .worktree_path()
            .to_path_buf();
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        let target = worktree.join("src/lib.rs");
        std::fs::write(&target, BROKEN_LIB).unwrap();
        backend.set_first_result(success_result(
            "Blocked. Report:\n\n## Result: not applied — write permission denied\n",
            &bugfix_invocation_one(&worktree, &target),
        ));
        (backend, temp, run_id, store, provider, worktree, target)
    }

    fn worktree_status(worktree: &Path) -> Vec<String> {
        let output = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(worktree)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .map(ToOwned::to_owned)
            .collect()
    }

    #[test]
    fn real_basic_bugfix_sequence_completes_after_safe_edit_continuation() {
        let (backend, _temp, run_id, mut store, provider, worktree, target) = bugfix_fixture(true);
        backend.set_edit_effect(2, target.clone(), FIXED_LIB);
        backend.set_result(
            2,
            success_result(
                "## Result: fix applied; test run blocked\n",
                &serde_json::Value::Array(bugfix_invocation_two(&worktree)),
            ),
        );
        let mut engine = WorkflowEngine::new(provider, "fix double");
        let request = drive_to_attention(&mut engine, &mut store, run_id);
        assert!(
            engine
                .can_auto_resolve_attention(&mut store, run_id, request)
                .unwrap()
        );
        engine
            .resolve_attention(&mut store, run_id, request)
            .unwrap();
        drive_to_completion(&mut engine, &mut store, run_id);

        let processes = store.list_managed_processes(run_id).unwrap();
        assert_eq!(processes.len(), 2);
        let resumed = processes[1]
            .spec()
            .argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            resumed
                .windows(2)
                .any(|pair| pair == ["--resume", "native-session-1"]),
            "{resumed:?}"
        );
        let rules = resumed
            .iter()
            .skip_while(|arg| *arg != "--allowedTools")
            .skip(1)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(rules, vec![format!("Edit(/{})", target.display())]);
        assert_no_bash_or_broad_grants(&all_argv(&store, run_id));
        let session = store.list_provider_sessions(run_id).unwrap().pop().unwrap();
        assert_eq!(session.invocation(), 2);
        assert_eq!(session.status(), ProviderSessionStatus::Completed);
        assert_eq!(
            store.load_run(run_id).unwrap().run.status(),
            RunStatus::Completed
        );
        let artifacts = store.list_artifacts(run_id).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert!(
            std::fs::read_to_string(artifacts[0].path())
                .unwrap()
                .contains("fix applied")
        );
        // Trusted harness view: only the approved Edit reached the worktree;
        // the denied sed/cargo alternatives never executed.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), FIXED_LIB);
        assert_eq!(worktree_status(&worktree), vec!["?? src/lib.rs".to_owned()]);
    }

    #[test]
    fn continued_session_with_question_still_needs_user() {
        let (backend, _temp, run_id, mut store, provider, worktree, target) = bugfix_fixture(true);
        backend.set_edit_effect(2, target, FIXED_LIB);
        let mut history = bugfix_invocation_two(&worktree);
        history.push(json!({
            "tool_name":"AskUserQuestion","tool_use_id":"toolu_q",
            "tool_input":{"questions":[{"question":"Add tests too?"}]}
        }));
        backend.set_result(
            2,
            success_result("blocked on question", &serde_json::Value::Array(history)),
        );
        let mut engine = WorkflowEngine::new(provider, "fix double");
        let request = drive_to_attention(&mut engine, &mut store, run_id);
        engine
            .resolve_attention(&mut store, run_id, request)
            .unwrap();
        let second = drive_to_attention(&mut engine, &mut store, run_id);
        assert_ne!(second, request);
        assert!(
            !engine
                .can_auto_resolve_attention(&mut store, run_id, second)
                .unwrap()
        );
        assert!(store.list_artifacts(run_id).unwrap().is_empty());
    }

    #[test]
    fn continued_session_with_new_unfulfilled_edit_still_needs_user() {
        for outside in [false, true] {
            let (backend, _temp, run_id, mut store, provider, worktree, target) =
                bugfix_fixture(true);
            backend.set_edit_effect(2, target, FIXED_LIB);
            let new_target = if outside {
                PathBuf::from("/tmp/outside/lib.rs")
            } else {
                worktree.join("src/other.rs")
            };
            let mut history = bugfix_invocation_two(&worktree);
            history.push(edit_denial("Edit", "toolu_new_edit", &new_target));
            backend.set_result(
                2,
                success_result("partially applied", &serde_json::Value::Array(history)),
            );
            let mut engine = WorkflowEngine::new(provider, "fix double");
            let request = drive_to_attention(&mut engine, &mut store, run_id);
            engine
                .resolve_attention(&mut store, run_id, request)
                .unwrap();
            let second = drive_to_attention(&mut engine, &mut store, run_id);
            assert_ne!(second, request);
            assert_eq!(
                store.load_run(run_id).unwrap().run.status(),
                RunStatus::NeedsUser
            );
            assert!(store.list_artifacts(run_id).unwrap().is_empty());
            let resolvable = engine
                .can_auto_resolve_attention(&mut store, run_id, second)
                .unwrap();
            assert_eq!(resolvable, !outside);
            assert!(
                !all_argv(&store, run_id)
                    .iter()
                    .any(|arg| arg.contains("/tmp/outside"))
            );
        }
    }

    #[test]
    fn production_policy_keeps_reattempted_mutating_bash_conservative() {
        let (backend, _temp, run_id, mut store, provider, worktree, target) = bugfix_fixture(false);
        backend.set_edit_effect(2, target, FIXED_LIB);
        backend.set_result(
            2,
            success_result(
                "## Result: fix applied; test run blocked\n",
                &serde_json::Value::Array(bugfix_invocation_two(&worktree)),
            ),
        );
        let mut engine = WorkflowEngine::new(provider, "fix double");
        let request = drive_to_attention(&mut engine, &mut store, run_id);
        assert!(
            !engine
                .can_auto_resolve_attention(&mut store, run_id, request)
                .unwrap()
        );
        // Human approval in production grants exact rules and resumes.
        engine
            .resolve_attention(&mut store, run_id, request)
            .unwrap();
        let second = drive_to_attention(&mut engine, &mut store, run_id);
        assert_ne!(second, request);
        assert_eq!(
            store.load_run(run_id).unwrap().run.status(),
            RunStatus::NeedsUser
        );
        assert!(store.list_artifacts(run_id).unwrap().is_empty());
    }

    #[test]
    fn unsafe_edit_mixed_with_safe_edit_never_auto_resolves_or_grants_outside_path() {
        let backend = FixtureBackend::default();
        let (_temp, _database, run_id, mut store, provider) =
            fixture_with_backend(backend.clone(), true);
        let worktree = store
            .load_workspace(run_id)
            .unwrap()
            .unwrap()
            .worktree_path()
            .to_path_buf();
        backend.set_first_result(success_result(
            "blocked",
            &json!([
                edit_denial("Edit", "toolu_inside", &worktree.join("README.md")),
                edit_denial("Edit", "toolu_outside", Path::new("/tmp/outside")),
            ]),
        ));
        let mut engine = WorkflowEngine::new(provider, "escape");
        let request = drive_to_attention(&mut engine, &mut store, run_id);
        assert!(
            !engine
                .can_auto_resolve_attention(&mut store, run_id, request)
                .unwrap()
        );
        assert!(
            engine
                .resolve_attention(&mut store, run_id, request)
                .is_err()
        );
        assert_eq!(
            store.load_run(run_id).unwrap().run.status(),
            RunStatus::NeedsUser
        );
        assert!(
            !all_argv(&store, run_id)
                .iter()
                .any(|arg| arg.contains("/tmp/outside"))
        );
    }

    #[test]
    fn question_mixed_with_safe_edit_never_auto_resolves() {
        let backend = FixtureBackend::default();
        let (_temp, _database, run_id, mut store, provider) =
            fixture_with_backend(backend.clone(), true);
        let worktree = store
            .load_workspace(run_id)
            .unwrap()
            .unwrap()
            .worktree_path()
            .to_path_buf();
        backend.set_first_result(success_result(
            "blocked",
            &json!([
                edit_denial("Edit", "toolu_inside", &worktree.join("README.md")),
                {"tool_name":"AskUserQuestion","tool_use_id":"toolu_q","tool_input":{"questions":[{"question":"Which?"}]}}
            ]),
        ));
        let mut engine = WorkflowEngine::new(provider, "question");
        let request = drive_to_attention(&mut engine, &mut store, run_id);
        assert!(
            !engine
                .can_auto_resolve_attention(&mut store, run_id, request)
                .unwrap()
        );
        assert!(
            engine
                .resolve_attention(&mut store, run_id, request)
                .is_err()
        );
    }

    /// Bash-only attempted mutation: production stays conservative; disposable
    /// eval completes with an unchanged repository and lets trusted scoring
    /// produce a normal FAIL (not `InfrastructureFailure`).
    #[test]
    fn implementer_mutating_bash_only_is_conservative_in_production_and_scored_in_eval() {
        // Production: NeedsUser, unresolved.
        let (backend, _temp, run_id, mut store, provider, worktree, _target) =
            bugfix_fixture(false);
        backend.set_first_result(success_result(
            "Blocked.",
            &json!([bash_denial(
                "toolu_sed",
                REAL_IMPLEMENTER_SED_BASH,
                &worktree
            )]),
        ));
        let mut engine = WorkflowEngine::new(provider, "sed only");
        let request = drive_to_attention(&mut engine, &mut store, run_id);
        assert!(
            !engine
                .can_auto_resolve_attention(&mut store, run_id, request)
                .unwrap()
        );
        assert_eq!(
            store.load_run(run_id).unwrap().run.status(),
            RunStatus::NeedsUser
        );
        assert!(store.list_artifacts(run_id).unwrap().is_empty());

        // Eval: provider Completed, no Bash granted, repository untouched.
        let (backend, _temp, run_id, mut store, provider, worktree, target) = bugfix_fixture(true);
        backend.set_first_result(success_result(
            "Blocked.",
            &json!([bash_denial(
                "toolu_sed",
                REAL_IMPLEMENTER_SED_BASH,
                &worktree
            )]),
        ));
        let mut engine = WorkflowEngine::new(provider, "sed only");
        drive_to_completion(&mut engine, &mut store, run_id);
        assert_eq!(store.list_managed_processes(run_id).unwrap().len(), 1);
        assert_no_bash_or_broad_grants(&all_argv(&store, run_id));
        let artifacts = store.list_artifacts(run_id).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), BROKEN_LIB);

        // Trusted harness view: expected diff absent → normal benchmark FAIL.
        let suite = crate::eval::EvalSuite::load("role_core_v3").unwrap();
        let case = suite
            .cases()
            .iter()
            .find(|case| case.id == "implementer_basic_bugfix")
            .unwrap();
        let artifact = std::fs::read_to_string(artifacts[0].path()).unwrap();
        let scored = crate::eval::score(
            case,
            crate::eval::ScoreInput {
                artifact: Some(&artifact),
                diff: "",
                validation_pass: Some(false),
            },
        )
        .unwrap();
        assert!(!scored.passed);
    }

    /// Exact real `invalid_plan_stop` sequence: denied initial diagnostic Bash,
    /// denied Explore-subagent Bash with `for`/`$(...)`, successful terminal,
    /// no Edit/Write/question, empty diff.
    #[test]
    fn real_invalid_plan_stop_sequence_completes_in_eval_and_stays_conservative_in_production() {
        const INITIAL: &str = "cd \"WORKTREE\" && git ls-files | head -100 && echo \"--- grep ---\" && grep -rniE \"ConfigRegistry|config_registry|config-registry\" --exclude-dir=.git . ; echo \"exit=$?\"";
        const EXPLORE: &str = "cd \"WORKTREE\" && for f in .gitignore Cargo.toml src/lib.rs; do echo \"=== $f ($(wc -l < \"$f\") lines) ===\"; cat \"$f\"; done; git status --porcelain";
        for eval in [true, false] {
            let backend = FixtureBackend::default();
            let (_temp, _database, run_id, mut store, provider) =
                fixture_with_backend(backend.clone(), eval);
            let worktree = store
                .load_workspace(run_id)
                .unwrap()
                .unwrap()
                .worktree_path()
                .to_path_buf();
            backend.set_first_result(success_result(
                "## Result: plan mismatch\n```json\n{\"eval_outcome\":\"plan_mismatch\"}\n```\n",
                &json!([
                    bash_denial("toolu_01BHjWDwh3nEk9rmuS32HDdM", INITIAL, &worktree),
                    bash_denial("toolu_01ExploreForLoop", EXPLORE, &worktree),
                ]),
            ));
            let mut engine = WorkflowEngine::new(provider, "plan mismatch");
            if eval {
                drive_to_completion(&mut engine, &mut store, run_id);
                assert_eq!(store.list_managed_processes(run_id).unwrap().len(), 1);
                let artifacts = store.list_artifacts(run_id).unwrap();
                assert_eq!(artifacts.len(), 1);
                assert!(
                    std::fs::read_to_string(artifacts[0].path())
                        .unwrap()
                        .contains("plan_mismatch")
                );
                assert!(worktree_status(&worktree).is_empty());
                assert_no_bash_or_broad_grants(&all_argv(&store, run_id));
            } else {
                let request = drive_to_attention(&mut engine, &mut store, run_id);
                assert!(
                    !engine
                        .can_auto_resolve_attention(&mut store, run_id, request)
                        .unwrap()
                );
                assert_eq!(
                    store.load_run(run_id).unwrap().run.status(),
                    RunStatus::NeedsUser
                );
                assert!(store.list_artifacts(run_id).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn real_implementer_invalid_plan_stop_diagnostic_wrapper_completes() {
        let backend = FixtureBackend::default();
        let (_temp, _database, run_id, mut store, provider) =
            fixture_with_backend(backend.clone(), true);
        let worktree = store
            .load_workspace(run_id)
            .unwrap()
            .unwrap()
            .worktree_path()
            .to_path_buf();
        backend.set_first_result(success_result(
            "## Result: plan mismatch\n```json\n{\"eval_outcome\":\"plan_mismatch\"}\n```\n",
            &json!([bash_denial(
                "toolu_01BHjWDwh3nEk9rmuS32HDdM",
                "cd \"WORKTREE\" && git ls-files | head -100 && echo \"---grep---\" && grep -rniE \"ConfigRegistry|config_registry|config-registry\" --exclude-dir=.git . ; echo \"grep exit: $?\"",
                &worktree
            )]),
        ));
        let mut engine = WorkflowEngine::new(provider, "plan mismatch");
        drive_to_completion(&mut engine, &mut store, run_id);
        assert_eq!(store.list_managed_processes(run_id).unwrap().len(), 1);
        assert_eq!(store.list_artifacts(run_id).unwrap().len(), 1);
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
        fixture_with_backend(FixtureBackend::default(), false)
    }

    fn fixture_with_backend(
        backend: FixtureBackend,
        eval_auto_approve: bool,
    ) -> (
        TempDir,
        PathBuf,
        crate::domain::RunId,
        SqliteStore,
        ClaudeProvider<FixtureBackend>,
    ) {
        fixture_with_workflow(
            backend,
            eval_auto_approve,
            WorkflowDefinition::built_in(WorkflowKind::Fast),
        )
    }

    /// Single read-only review stage so reviewer terminal semantics can be
    /// exercised without fake dependency stages.
    fn review_workflow(kind: StageKind, role: Role) -> WorkflowDefinition {
        WorkflowDefinition::new(
            WorkflowKind::Review,
            vec![StageDefinition::new(
                crate::domain::StageId::new("review").unwrap(),
                kind,
                role,
                vec![],
            )],
        )
        .unwrap()
    }

    fn fixture_with_workflow(
        backend: FixtureBackend,
        eval_auto_approve: bool,
        workflow: WorkflowDefinition,
    ) -> (
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
        let run = Run::new(run_id, workflow, config_id.clone(), created_at);
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
            effort: EffortSetting::NativeDefault,
            manager: ProcessManager::new(&process_root, backend),
            artifact_root: process_root,
            eval_auto_approve,
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
