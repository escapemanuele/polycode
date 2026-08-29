use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};

use crate::app::{AppError, RunService, RuntimeProviderFactory};
use crate::domain::{StageId, StageStatus};
use crate::update::{InstallSource, UpdateInfo};

use super::input::{Intent, map_key, map_text_key};
use super::render;
use super::state::{Overlay, Screen, TuiState, UiMessageKind};
use super::terminal::TerminalSession;
use super::worker::{Worker, WorkerCommand, WorkerResult, WorkerSuccess};

const EVENT_POLL: Duration = Duration::from_millis(100);
const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

/// One completed background update check: the newer release when there is
/// one, and how this executable was installed.
type UpdateOutcome = (Option<UpdateInfo>, Option<InstallSource>);

/// One completed installation attempt.
type InstallOutcome = Result<String, String>;

pub(crate) struct TuiApp {
    state: TuiState,
    reader: RunService<RuntimeProviderFactory>,
    worker: Worker,
    /// Delivers the background update check exactly once. Startup never waits
    /// on it, and a dropped sender simply means no update will be offered.
    update: Receiver<UpdateOutcome>,
    /// Present only while an installation this session started is running.
    installing: Option<Receiver<InstallOutcome>>,
    last_refresh: Instant,
}

impl TuiApp {
    pub(crate) fn new(repository: &Path) -> Result<Self, crate::app::AppError> {
        let reader = RunService::from_environment(RuntimeProviderFactory)?;
        let actions = RunService::from_environment(RuntimeProviderFactory)?;
        Ok(Self {
            state: TuiState::new(repository),
            reader,
            worker: Worker::spawn(actions),
            update: spawn_update_check(),
            installing: None,
            last_refresh: Instant::now()
                .checked_sub(REFRESH_INTERVAL)
                .unwrap_or_else(Instant::now),
        })
    }

    pub(crate) fn run(mut self, terminal: &mut TerminalSession) -> anyhow::Result<()> {
        self.refresh();
        while !self.state.quit {
            self.receive_worker_results();
            self.receive_update();
            self.receive_install();
            self.state.clear_expired_message(Instant::now());
            if self.last_refresh.elapsed() >= REFRESH_INTERVAL {
                self.refresh();
            }
            terminal
                .terminal_mut()
                .draw(|frame| render::render(frame, &self.state))?;
            if event::poll(EVENT_POLL)? {
                self.handle_event(event::read()?);
            }
        }
        Ok(())
    }

    /// Absorbs the background check without ever blocking, then opens the
    /// prompt only when the interface has nothing more important to say.
    fn receive_update(&mut self) {
        match self.update.try_recv() {
            Ok((info, source)) => {
                self.state.update = info;
                self.state.update_install = source;
            }
            // Empty means still checking; disconnected means the check
            // finished or failed and will never speak again. Neither is worth
            // telling the user about.
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
        if self.state.update_prompt_is_due() {
            self.state.overlay = Some(Overlay::Update);
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                let text_mode = self.state.screen == Screen::NewRun
                    && matches!(self.state.new_run.focus, 0 | 2)
                    || self.state.overlay == Some(Overlay::Attention);
                let intent = if text_mode {
                    map_text_key(key)
                } else {
                    map_key(key)
                };
                self.handle_intent(intent);
            }
            Event::Paste(text) => self.handle_paste(&text),
            Event::Key(_)
            | Event::Resize(_, _)
            | Event::FocusGained
            | Event::FocusLost
            | Event::Mouse(_) => {}
        }
    }

    fn handle_intent(&mut self, intent: Intent) {
        if intent == Intent::Quit {
            self.state.quit = true;
            return;
        }
        if let Some(overlay) = self.state.overlay {
            self.handle_overlay_intent(overlay, intent);
            return;
        }
        if self.state.screen == Screen::NewRun {
            self.handle_new_run_intent(intent);
            return;
        }
        match intent {
            Intent::Up => self.move_selection(false),
            Intent::Down => self.move_selection(true),
            Intent::PageUp => self.state.scroll = self.state.scroll.saturating_sub(10),
            Intent::PageDown => self.state.scroll = self.state.scroll.saturating_add(10),
            Intent::Home if is_viewer(self.state.screen) => self.state.scroll = 0,
            Intent::End if is_viewer(self.state.screen) => self.state.scroll = u16::MAX,
            Intent::Enter if self.state.screen == Screen::Runs => {
                if self.state.selected_run.is_some() {
                    self.state.screen = Screen::RunDetail;
                    self.refresh_selected();
                }
            }
            // Enter on a stage opens its primary result; open_artifact keeps
            // expected absence informational, so this never mutates anything.
            Intent::Enter if self.state.screen == Screen::RunDetail => self.open_artifact(),
            Intent::Escape => self.back(),
            Intent::NewRun => {
                self.state.screen = Screen::NewRun;
                self.state.overlay = None;
                self.state.message = None;
            }
            Intent::Runs => {
                self.state.screen = Screen::Runs;
                self.state.overlay = None;
                self.state.scroll = 0;
            }
            Intent::Resume => self.resume(),
            Intent::Retry => self.retry(),
            Intent::Attention => self.open_attention(),
            Intent::Artifact => self.open_artifact(),
            Intent::Logs => self.open_logs(),
            Intent::Diff => self.open_diff(),
            Intent::Apply => self.open_apply_confirmation(),
            Intent::Discard => self.state.overlay = Some(Overlay::DiscardConfirm),
            Intent::DismissMessage => self.state.dismiss_message(),
            Intent::ToggleRaw if self.state.screen == Screen::Artifact => {
                self.state.artifact_raw = !self.state.artifact_raw;
            }
            Intent::TechnicalDetails if self.state.screen == Screen::RunDetail => {
                self.state.technical = !self.state.technical;
                self.refresh_evidence();
            }
            Intent::Help => self.state.overlay = Some(Overlay::Help),
            _ => {}
        }
    }

    fn handle_overlay_intent(&mut self, overlay: Overlay, intent: Intent) {
        if overlay == Overlay::Update {
            self.handle_update_intent(intent);
            return;
        }
        if matches!(intent, Intent::Escape | Intent::Help) {
            self.state.overlay = None;
            return;
        }
        match overlay {
            Overlay::Attention => self.handle_attention_intent(intent),
            Overlay::ApplyConfirm if intent == Intent::Enter => {
                if let Some(run_id) = self.state.selected_run {
                    self.dispatch(WorkerCommand::ApplyRun { run_id });
                    self.state.overlay = None;
                }
            }
            Overlay::DiscardConfirm if intent == Intent::Enter => {
                if let Some(run_id) = self.state.selected_run {
                    self.dispatch(WorkerCommand::DiscardRun { run_id });
                    self.state.overlay = None;
                }
            }
            Overlay::Help | Overlay::ApplyConfirm | Overlay::DiscardConfirm | Overlay::Update => {}
        }
    }

    /// Any answer closes the prompt for the lifetime of this process; the
    /// 24-hour cache decides whether the next process asks again. Only an
    /// explicit Yes on an installation Polycode owns starts an install.
    fn handle_update_intent(&mut self, intent: Intent) {
        match intent {
            Intent::Up | Intent::Down => {
                if self.state.update_is_installable() {
                    self.state.update_install_selected = !self.state.update_install_selected;
                }
            }
            Intent::Enter => {
                let install =
                    self.state.update_is_installable() && self.state.update_install_selected;
                self.state.update_dismissed = true;
                self.state.overlay = None;
                if install {
                    self.begin_update_install();
                }
            }
            Intent::Escape | Intent::Help => {
                self.state.update_dismissed = true;
                self.state.overlay = None;
            }
            _ => {}
        }
    }

    /// Runs the installation off the render loop so the interface stays
    /// responsive, and reports its outcome as an ordinary notification.
    fn begin_update_install(&mut self) {
        let Some(info) = self.state.update.clone() else {
            return;
        };
        self.state
            .notify(UiMessageKind::Info, "Installing update…".to_owned());
        self.installing = Some(spawn_update_install(info));
    }

    /// Absorbs an installation result without ever blocking the loop.
    fn receive_install(&mut self) {
        let Some(receiver) = self.installing.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(notice)) => {
                self.state.set_message(notice);
                self.installing = None;
            }
            Ok(Err(error)) => {
                // A failed install is recoverable: the existing executable is
                // untouched and the interface carries on normally.
                self.state
                    .set_error(format!("Update not installed: {error}"));
                self.installing = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.installing = None,
        }
    }

    fn handle_attention_intent(&mut self, intent: Intent) {
        let length = self
            .state
            .details
            .as_ref()
            .map_or(0, |details| details.attention.len());
        match intent {
            Intent::Up => self.state.attention_index = self.state.attention_index.saturating_sub(1),
            Intent::Down if length > 0 => {
                self.state.attention_index = (self.state.attention_index + 1).min(length - 1);
            }
            Intent::Left => self.state.attention_response.left(),
            Intent::Right => self.state.attention_response.right(),
            Intent::Home => self.state.attention_response.home(),
            Intent::End => self.state.attention_response.end(),
            Intent::Backspace => self.state.attention_response.backspace(),
            Intent::Delete => self.state.attention_response.delete(),
            Intent::Character(character) => self.state.attention_response.insert(character),
            Intent::Enter => {
                let Some(run_id) = self.state.selected_run else {
                    return;
                };
                let Some(attention_id) = self.state.selected_attention_id() else {
                    self.state
                        .set_error("No pending attention request selected");
                    return;
                };
                let response = (!self.state.attention_response.text().is_empty())
                    .then(|| self.state.attention_response.text().to_owned());
                self.dispatch(WorkerCommand::ResolveAttention {
                    run_id,
                    attention_id,
                    response,
                });
                self.state.overlay = None;
            }
            _ => {}
        }
    }

    fn handle_new_run_intent(&mut self, intent: Intent) {
        match intent {
            Intent::Escape => self.back(),
            Intent::Tab => self.state.new_run.next_field(),
            Intent::BackTab => self.state.new_run.previous_field(),
            Intent::Left if matches!(self.state.new_run.focus, 1 | 3) => {
                self.state.new_run.cycle_value(false);
            }
            Intent::Right if matches!(self.state.new_run.focus, 1 | 3) => {
                self.state.new_run.cycle_value(true);
            }
            Intent::Left => self.edit_text(super::state::TextField::left),
            Intent::Right => self.edit_text(super::state::TextField::right),
            Intent::Home => self.edit_text(super::state::TextField::home),
            Intent::End => self.edit_text(super::state::TextField::end),
            Intent::Backspace => self.edit_text(super::state::TextField::backspace),
            Intent::Delete => self.edit_text(super::state::TextField::delete),
            Intent::Character(character) => self.edit_text(|field| field.insert(character)),
            Intent::Enter => self.submit_new_run(),
            _ => {}
        }
    }

    fn edit_text(&mut self, edit: impl FnOnce(&mut super::state::TextField)) {
        if let Some(field) = self.state.new_run.active_text_mut() {
            edit(field);
        }
    }

    fn handle_paste(&mut self, text: &str) {
        if self.state.overlay == Some(Overlay::Attention) {
            self.state.attention_response.paste(text);
        } else if self.state.screen == Screen::NewRun {
            self.edit_text(|field| field.paste(text));
        }
    }

    fn submit_new_run(&mut self) {
        let task = self.state.new_run.task.text().trim().to_owned();
        let repository = self.state.new_run.repository.text().trim().to_owned();
        if task.is_empty() {
            self.state.set_error("Task cannot be empty");
            return;
        }
        if repository.is_empty() {
            self.state.set_error("Repository cannot be empty");
            return;
        }
        self.dispatch(WorkerCommand::StartRun {
            workflow: self.state.new_run.workflow,
            task,
            repository: PathBuf::from(repository),
            selection: self.state.new_run.execution.selection(),
            effort: self.state.new_run.effort.setting(),
        });
        if self.state.worker_busy.is_some() {
            self.state.screen = Screen::Runs;
        }
    }

    fn move_selection(&mut self, forward: bool) {
        match self.state.screen {
            Screen::Runs => {
                self.state.move_run(forward);
                self.refresh_selected();
            }
            Screen::RunDetail => self.state.move_stage(forward),
            Screen::Artifact | Screen::Logs | Screen::Diff => {
                self.state.scroll = if forward {
                    self.state.scroll.saturating_add(1)
                } else {
                    self.state.scroll.saturating_sub(1)
                };
            }
            Screen::NewRun => {}
        }
    }

    fn back(&mut self) {
        self.state.overlay = None;
        self.state.scroll = 0;
        self.state.screen = match self.state.screen {
            Screen::Runs | Screen::RunDetail | Screen::NewRun => Screen::Runs,
            Screen::Artifact | Screen::Logs | Screen::Diff => Screen::RunDetail,
        };
    }

    fn resume(&mut self) {
        if let Some(run_id) = self.state.selected_run {
            self.dispatch(WorkerCommand::ResumeRun { run_id });
        }
    }

    fn retry(&mut self) {
        let Some(run_id) = self.state.selected_run else {
            return;
        };
        let Some(details) = self.state.details.as_ref() else {
            return;
        };
        let Some(stage) = details.stages.get(self.state.selected_stage_index) else {
            return;
        };
        if stage.status != StageStatus::Failed {
            self.state.set_error("Selected stage is not failed");
            return;
        }
        self.dispatch(WorkerCommand::RetryStage {
            run_id,
            stage_id: stage.id.clone(),
        });
    }

    fn open_attention(&mut self) {
        if self
            .state
            .details
            .as_ref()
            .is_none_or(|details| details.attention.is_empty())
        {
            self.state
                .set_error("Selected run has no pending attention");
            return;
        }
        self.state.attention_index = 0;
        self.state.attention_response = super::state::TextField::default();
        self.state.overlay = Some(Overlay::Attention);
    }

    fn open_artifact(&mut self) {
        let Some((run_id, stage_id)) = self.selected_stage_identity() else {
            return;
        };
        match self.reader.read_artifact(run_id, &stage_id) {
            Ok(artifact) => {
                self.state.artifact = Some(artifact);
                self.state.artifact_raw = false;
                self.state.scroll = 0;
                self.state.screen = Screen::Artifact;
            }
            Err(AppError::ArtifactNotFound { .. }) => {
                let status = self
                    .state
                    .details
                    .as_ref()
                    .and_then(|details| details.stages.get(self.state.selected_stage_index))
                    .map(|stage| stage.status);
                let (kind, text) = artifact_unavailable_feedback(status, &stage_id);
                self.state.notify(kind, text);
            }
            Err(error) => self.state.set_error(error.to_string()),
        }
    }

    fn open_logs(&mut self) {
        let Some((run_id, stage_id)) = self.selected_stage_identity() else {
            return;
        };
        match self.reader.read_process_log_tail(run_id, &stage_id) {
            Ok(logs) => {
                self.state.logs = Some(logs);
                self.state.scroll = 0;
                self.state.screen = Screen::Logs;
            }
            Err(error) => self.state.set_error(error.to_string()),
        }
    }

    fn open_diff(&mut self) {
        let Some(run_id) = self.state.selected_run else {
            return;
        };
        match self.reader.preview_run_diff(run_id) {
            Ok(diff) => {
                self.state.diff = Some(diff);
                self.state.scroll = 0;
                self.state.screen = Screen::Diff;
            }
            Err(error) => self.state.set_error(error.to_string()),
        }
    }

    fn open_apply_confirmation(&mut self) {
        let Some(run_id) = self.state.selected_run else {
            return;
        };
        // The workspace layer rejects apply outside the completed branch-run
        // state; refusing here keeps the TUI from dispatching an action the
        // domain will decline, without weakening that guard.
        if !self.state.run_is_applyable() {
            self.state
                .notify(UiMessageKind::Info, apply_unavailable_reason(&self.state));
            return;
        }
        match self.reader.preview_run_diff(run_id) {
            Ok(diff) => {
                self.state.diff = Some(diff);
                self.state.overlay = Some(Overlay::ApplyConfirm);
            }
            Err(error) => self.state.set_error(error.to_string()),
        }
    }

    fn selected_stage_identity(&self) -> Option<(crate::domain::RunId, crate::domain::StageId)> {
        Some((self.state.selected_run?, self.state.selected_stage.clone()?))
    }

    fn dispatch(&mut self, command: WorkerCommand) {
        if self.state.worker_busy.is_some() {
            self.state
                .set_error("Another application action is already running");
            return;
        }
        let label = command.kind().label().to_owned();
        match self.worker.send(command) {
            Ok(()) => {
                self.state.worker_busy = Some(label);
                self.state.message = None;
            }
            Err(error) => self.state.set_error(error),
        }
    }

    fn receive_worker_results(&mut self) {
        loop {
            match self.worker.try_recv() {
                Ok(Some(result)) => self.handle_worker_result(result),
                Ok(None) => break,
                Err(error) => {
                    self.state.worker_busy = None;
                    self.state.set_error(error);
                    break;
                }
            }
        }
    }

    fn handle_worker_result(&mut self, result: WorkerResult) {
        self.state.worker_busy = None;
        match result.result {
            Ok(success) => {
                let run_id = success.report().details.id;
                self.state.selected_run = Some(run_id);
                self.state.replace_details(success.report().details.clone());
                self.state.quiescent = Some(success.report().outcome.clone());
                self.state.screen = Screen::RunDetail;
                let message = match success {
                    WorkerSuccess::Applied(outcome, _) => format!("Apply finished: {outcome:?}"),
                    WorkerSuccess::Execution(_) => format!("{} finished", result.action.label()),
                };
                self.state.set_message(message);
            }
            Err(error) => {
                let scope = result
                    .run_id
                    .map_or(String::new(), |run_id| format!(" for {run_id}"));
                self.state
                    .set_error(format!("{} failed{scope}: {error}", result.action.label()));
            }
        }
        self.refresh();
    }

    fn refresh(&mut self) {
        match self.reader.list_runs() {
            Ok(runs) => {
                self.state.replace_runs(runs);
                self.refresh_selected();
            }
            Err(error) => self.state.set_error(error.to_string()),
        }
        if self.state.screen == Screen::Logs {
            if let Some((run_id, stage_id)) = self.selected_stage_identity() {
                if let Ok(logs) = self.reader.read_process_log_tail(run_id, &stage_id) {
                    self.state.logs = Some(logs);
                }
            }
        }
        self.last_refresh = Instant::now();
    }

    fn refresh_selected(&mut self) {
        let Some(run_id) = self.state.selected_run else {
            return;
        };
        match self.reader.inspect_run(run_id) {
            Ok(details) => self.state.replace_details(details),
            Err(error) => self.state.set_error(error.to_string()),
        }
        if let Ok(artifacts) = self.reader.list_artifacts(run_id) {
            self.state.stages_with_artifacts = artifacts
                .into_iter()
                .filter(|artifact| artifact.status == crate::domain::ArtifactStatus::Complete)
                .map(|artifact| artifact.stage_id)
                .collect();
        }
        self.refresh_evidence();
    }

    /// Per-stage diagnostic evidence is only needed by the technical view, so
    /// it is fetched only while that view is open.
    fn refresh_evidence(&mut self) {
        if !self.state.technical || self.state.screen != Screen::RunDetail {
            self.state.evidence = None;
            return;
        }
        let Some((run_id, stage_id)) = self.selected_stage_identity() else {
            self.state.evidence = None;
            return;
        };
        self.state.evidence = self.reader.stage_execution_evidence(run_id, &stage_id).ok();
    }
}

/// Explains, in operational language, why apply is not offered yet.
fn apply_unavailable_reason(state: &TuiState) -> String {
    let Some(details) = state.details.as_ref() else {
        return "Select a run before applying.".to_owned();
    };
    if details.workflow == crate::domain::WorkflowKind::Review {
        return "Review runs make no workspace changes, so there is nothing to apply.".to_owned();
    }
    match details.status {
        crate::domain::RunStatus::Applied => "This run has already been applied.".to_owned(),
        crate::domain::RunStatus::Discarded => "This run was discarded.".to_owned(),
        crate::domain::RunStatus::NeedsUser => {
            "Run needs you — resolve the attention request before applying.".to_owned()
        }
        crate::domain::RunStatus::Failed => {
            "Run failed — retry the failed stage before applying.".to_owned()
        }
        _ => "Run is still working — Apply becomes available when it completes.".to_owned(),
    }
}

/// Presentation severity for "no verified artifact" keyed on typed stage
/// state: absence is expected while a stage has not completed, and only a
/// completed stage missing its artifact is a genuine error.
fn artifact_unavailable_feedback(
    status: Option<StageStatus>,
    stage_id: &StageId,
) -> (UiMessageKind, String) {
    match status {
        Some(StageStatus::Running) => (
            UiMessageKind::Info,
            format!("Artifact not available yet — {stage_id} is still running."),
        ),
        Some(StageStatus::Pending | StageStatus::Ready) => (
            UiMessageKind::Info,
            "Artifact not available yet — this stage has not completed.".to_owned(),
        ),
        Some(
            StageStatus::Failed
            | StageStatus::NeedsUser
            | StageStatus::Paused
            | StageStatus::Interrupted,
        ) => (
            UiMessageKind::Warning,
            "No verified artifact is available for this stage yet.".to_owned(),
        ),
        Some(StageStatus::Completed | StageStatus::Skipped) | None => (
            UiMessageKind::Error,
            format!("Stage {stage_id} completed but has no verified artifact."),
        ),
    }
}

const fn is_viewer(screen: Screen) -> bool {
    matches!(screen, Screen::Artifact | Screen::Logs | Screen::Diff)
}

/// Downloads, verifies, and installs one release on a detached thread. Every
/// safety decision lives in the installer; this only moves it off the render
/// loop and turns the outcome into a message.
fn spawn_update_install(info: UpdateInfo) -> Receiver<InstallOutcome> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(install_release(&info).map_err(|error| error.to_string()));
    });
    receiver
}

/// Re-reads the release so the binary and its checksums come from one
/// consistent listing rather than from cached metadata.
fn install_release(info: &UpdateInfo) -> anyhow::Result<String> {
    use crate::update::ReleaseSource as _;
    let source = crate::update::GitHubReleases::new(
        crate::update::OFFICIAL_REPOSITORY,
        Duration::from_secs(10),
    );
    let release = source
        .latest_stable()?
        .filter(|release| release.version == info.available_version)
        .ok_or_else(|| anyhow::anyhow!("release {} is no longer published", info.tag))?;
    let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    let downloader = crate::update::HttpDownloader::new(Duration::from_secs(120));
    let installed = crate::update::install(&release, &std::env::current_exe()?, &downloader, now)?;
    // A binary that installed but did not register is reported as such; the
    // alternative is silently losing automatic updates.
    Ok(match installed.registration_warning() {
        Some(warning) => format!("{} {warning}", installed.restart_notice()),
        None => installed.restart_notice(),
    })
}

/// Runs one update check on a detached thread so startup never waits on the
/// network. Every failure — including an unresolvable data directory — simply
/// produces no update.
fn spawn_update_check() -> Receiver<UpdateOutcome> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok(service) = crate::update::UpdateService::from_environment() else {
            return;
        };
        let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
        // Background detection stays cache-aware: it is not a check the user
        // asked for, and it must never make starting the TUI wait on GitHub.
        let info = service.cached_status(now).available().cloned();
        // Install source is only interesting when an update exists.
        let source = info
            .as_ref()
            .and_then(|_| crate::update::detect_install_source().ok());
        let _ = sender.send((info, source));
    });
    receiver
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::app::{RunDetails, StageSummary, UsageSummary};
    use crate::domain::{EffortSetting, Role, RunId, RunStatus, StageId, StageKind, WorkflowKind};

    /// A `TuiApp` wired to an empty temporary store: enough to drive intents
    /// without touching the user's data directory or any provider.
    fn app_with(details: RunDetails) -> (TuiApp, TempDir) {
        let fixture = TempDir::new().unwrap();
        let database = fixture.path().join("polycode.db");
        let worktrees = fixture.path().join("worktrees");
        let mut state = TuiState::new(fixture.path());
        state.screen = Screen::RunDetail;
        state.selected_run = Some(details.id);
        state.replace_details(details);
        let app = TuiApp {
            state,
            reader: RunService::new(database.clone(), worktrees.clone(), RuntimeProviderFactory),
            worker: Worker::spawn(RunService::new(database, worktrees, RuntimeProviderFactory)),
            // Tests never check for updates: an immediately dropped sender
            // reads as "no update will ever arrive".
            update: mpsc::channel().1,
            installing: None,
            last_refresh: Instant::now(),
        };
        (app, fixture)
    }

    fn details(status: RunStatus, workflow: WorkflowKind) -> RunDetails {
        RunDetails {
            id: RunId::from_u128(7),
            task: Some("Add OAuth provider support".to_owned()),
            workflow,
            status,
            repository: Some(std::path::PathBuf::from("/repo")),
            workspace_status: Some(crate::workspace::WorkspaceStatus::Ready),
            base_commit: Some("abc1234".to_owned()),
            profile: "recommended".to_owned(),
            profile_version: "recommended_v2".to_owned(),
            routes: Vec::new(),
            revision: crate::store::RunRevision::initial(),
            created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            updated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            stages: vec![StageSummary {
                id: StageId::new("implementation").unwrap(),
                kind: StageKind::Implementation,
                role: Role::Implementer,
                status: StageStatus::Running,
                configured_provider: "codex".to_owned(),
                requested_effort: EffortSetting::NativeDefault,
                configured_model: None,
                actual_provider: None,
                actual_model: None,
                provider_session_record: None,
                native_session: None,
                provider_session_status: None,
                process_status: None,
                started_at: None,
                finished_at: None,
            }],
            attention: Vec::new(),
            usage: UsageSummary::default(),
            started_at: None,
            finished_at: None,
        }
    }

    #[test]
    fn apply_is_refused_for_every_non_applyable_run_state() {
        // The workspace layer rejects apply outside a completed branch run;
        // the TUI must not dispatch into that rejection. The confirmation
        // overlay is the only path to ApplyRun, so an overlay that never
        // opens is a command that never dispatches.
        for (status, workflow) in [
            (RunStatus::Running, WorkflowKind::Standard),
            (RunStatus::NeedsUser, WorkflowKind::Standard),
            (RunStatus::Paused, WorkflowKind::Standard),
            (RunStatus::Failed, WorkflowKind::Standard),
            (RunStatus::Applied, WorkflowKind::Standard),
            (RunStatus::Completed, WorkflowKind::Review),
        ] {
            let (mut app, _fixture) = app_with(details(status, workflow));
            app.handle_intent(Intent::Apply);
            assert_eq!(
                app.state.overlay, None,
                "apply confirmation must not open for {status:?}/{workflow:?}"
            );
            assert!(
                app.state.worker_busy.is_none(),
                "no worker command dispatched for {status:?}/{workflow:?}"
            );
            let message = app.state.message.as_ref().expect("user is told why");
            assert_eq!(message.kind, UiMessageKind::Info, "refusal is not an error");
        }
    }

    #[test]
    fn apply_opens_confirmation_for_a_real_completed_branch_run() {
        // Driven against a real committed fake run so the whole path runs:
        // eligibility gate, diff preview, then the confirmation overlay.
        let fixture = TempDir::new().unwrap();
        let repo = fixture.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for arguments in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(&arguments)
                    .current_dir(&repo)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        std::fs::write(repo.join("README.md"), "baseline\n").unwrap();
        for arguments in [vec!["add", "README.md"], vec!["commit", "-qm", "initial"]] {
            assert!(
                std::process::Command::new("git")
                    .args(&arguments)
                    .current_dir(&repo)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        let database = fixture.path().join("polycode.db");
        let worktrees = fixture.path().join("worktrees");
        let report = RunService::new(
            database.clone(),
            worktrees.clone(),
            crate::app::DevelopmentFakeProviderFactory,
        )
        .start_run(
            WorkflowKind::Standard,
            "mission deck apply gate",
            repo,
            Some(crate::app::ExecutionSelection::Uniform(
                crate::app::UniformProvider::Fake,
            )),
            EffortSetting::NativeDefault,
        )
        .unwrap();
        assert_eq!(report.details.status, RunStatus::Completed);

        let mut state = TuiState::new(fixture.path());
        state.screen = Screen::RunDetail;
        state.selected_run = Some(report.details.id);
        state.replace_details(report.details);
        let mut app = TuiApp {
            state,
            reader: RunService::new(database.clone(), worktrees.clone(), RuntimeProviderFactory),
            worker: Worker::spawn(RunService::new(database, worktrees, RuntimeProviderFactory)),
            // Tests never check for updates: an immediately dropped sender
            // reads as "no update will ever arrive".
            update: mpsc::channel().1,
            installing: None,
            last_refresh: Instant::now(),
        };
        assert!(app.state.run_is_applyable());
        app.handle_intent(Intent::Apply);
        assert_eq!(app.state.overlay, Some(Overlay::ApplyConfirm));
        assert!(
            app.state.worker_busy.is_none(),
            "opening the confirmation still dispatches nothing"
        );
    }

    #[test]
    fn refusal_wording_names_the_blocking_state() {
        let (app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        assert!(apply_unavailable_reason(&app.state).contains("still working"));

        let (app, _fixture) = app_with(details(RunStatus::NeedsUser, WorkflowKind::Standard));
        assert!(apply_unavailable_reason(&app.state).contains("resolve the attention"));

        let (app, _fixture) = app_with(details(RunStatus::Applied, WorkflowKind::Standard));
        assert!(apply_unavailable_reason(&app.state).contains("already been applied"));

        let (app, _fixture) = app_with(details(RunStatus::Completed, WorkflowKind::Review));
        assert!(apply_unavailable_reason(&app.state).contains("no workspace changes"));
    }

    fn update_info() -> crate::update::UpdateInfo {
        crate::update::UpdateInfo {
            current_version: semver::Version::parse("0.1.0").unwrap(),
            available_version: semver::Version::parse("0.2.0").unwrap(),
            tag: "v0.2.0".to_owned(),
            release_url: "https://example.invalid/r".to_owned(),
            published_at: None,
        }
    }

    #[test]
    fn declining_the_update_dismisses_it_without_installing() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        app.state.screen = Screen::Runs;
        app.state.update = Some(update_info());
        app.state.update_install = Some(crate::update::InstallSource::OfficialBinary);
        app.state.overlay = Some(Overlay::Update);

        // Moving the selection to No and confirming must not start anything.
        app.handle_intent(Intent::Down);
        assert!(!app.state.update_install_selected);
        app.handle_intent(Intent::Enter);
        assert_eq!(app.state.overlay, None);
        assert!(app.state.update_dismissed);
        assert!(
            app.installing.is_none(),
            "declining never starts an installation"
        );
        assert!(!app.state.update_prompt_is_due(), "it does not reopen");
    }

    #[test]
    fn escape_dismisses_the_update_for_the_session() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        app.state.screen = Screen::Runs;
        app.state.update = Some(update_info());
        app.state.update_install = Some(crate::update::InstallSource::OfficialBinary);
        app.state.overlay = Some(Overlay::Update);
        app.handle_intent(Intent::Escape);
        assert_eq!(app.state.overlay, None);
        assert!(app.state.update_dismissed);
        assert!(app.installing.is_none());
    }

    #[test]
    fn an_unsupported_installation_can_never_start_an_install() {
        for source in [
            crate::update::InstallSource::Source,
            crate::update::InstallSource::Cargo,
            crate::update::InstallSource::Homebrew,
            crate::update::InstallSource::Unknown,
        ] {
            let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
            app.state.screen = Screen::Runs;
            app.state.update = Some(update_info());
            app.state.update_install = Some(source);
            app.state.overlay = Some(Overlay::Update);
            // Even with the install answer selected, the strategy refuses.
            app.state.update_install_selected = true;
            app.handle_intent(Intent::Enter);
            assert!(
                app.installing.is_none(),
                "{source:?} must never begin an installation"
            );
            assert!(app.state.update_dismissed);
        }
    }

    #[test]
    fn the_update_prompt_does_not_hijack_run_keys() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        app.state.screen = Screen::Runs;
        app.state.update = Some(update_info());
        app.state.update_install = Some(crate::update::InstallSource::OfficialBinary);
        app.state.overlay = Some(Overlay::Update);
        // Run actions are inert while the prompt owns the keyboard.
        app.handle_intent(Intent::Apply);
        assert_eq!(app.state.overlay, Some(Overlay::Update));
        assert!(app.state.worker_busy.is_none());
    }

    #[test]
    fn technical_toggle_is_scoped_to_run_detail() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        assert!(!app.state.technical, "operational view is the default");
        app.handle_intent(Intent::TechnicalDetails);
        assert!(app.state.technical);
        app.handle_intent(Intent::TechnicalDetails);
        assert!(!app.state.technical);

        app.state.screen = Screen::Logs;
        app.handle_intent(Intent::TechnicalDetails);
        assert!(!app.state.technical, "viewers keep their own controls");
    }

    #[test]
    fn discard_confirmation_still_opens_for_any_run() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        app.handle_intent(Intent::Discard);
        assert_eq!(
            app.state.overlay,
            Some(Overlay::DiscardConfirm),
            "discard eligibility is unchanged by this milestone"
        );
    }

    #[test]
    fn viewer_identification_is_explicit() {
        assert!(is_viewer(Screen::Artifact));
        assert!(is_viewer(Screen::Logs));
        assert!(is_viewer(Screen::Diff));
        assert!(!is_viewer(Screen::RunDetail));
    }

    #[test]
    fn action_labels_are_stable_user_feedback() {
        assert_eq!(
            crate::tui::worker::ActionKind::Resume.label(),
            "resuming run"
        );
    }

    #[test]
    fn artifact_absence_severity_tracks_stage_state() {
        let stage_id = crate::domain::StageId::new("implementation").unwrap();
        let (kind, text) = artifact_unavailable_feedback(Some(StageStatus::Running), &stage_id);
        assert_eq!(kind, UiMessageKind::Info);
        assert!(text.contains("still running"));

        let (kind, _) = artifact_unavailable_feedback(Some(StageStatus::Pending), &stage_id);
        assert_eq!(kind, UiMessageKind::Info);

        let (kind, _) = artifact_unavailable_feedback(Some(StageStatus::Failed), &stage_id);
        assert_eq!(kind, UiMessageKind::Warning);
        let (kind, _) = artifact_unavailable_feedback(Some(StageStatus::NeedsUser), &stage_id);
        assert_eq!(kind, UiMessageKind::Warning);

        let (kind, text) = artifact_unavailable_feedback(Some(StageStatus::Completed), &stage_id);
        assert_eq!(
            kind,
            UiMessageKind::Error,
            "completed without artifact is real"
        );
        assert!(text.contains("completed but has no verified artifact"));
    }

    #[test]
    fn text_mode_allows_q_character() {
        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('q'),
            crossterm::event::KeyModifiers::NONE,
        );
        assert_eq!(map_text_key(key), Intent::Character('q'));
    }
}
