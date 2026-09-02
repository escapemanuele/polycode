use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};

use crate::app::{AppError, ArtifactSummary, RunListItem, RunService, RuntimeProviderFactory};
use crate::domain::{RunId, RunStatus, StageId, StageStatus};
use crate::update::{InstallSource, UpdateInfo};

use super::bottom_line;
use super::input::{Intent, map_key, map_text_key};
use super::motion;
use super::render;
use super::state::{Overlay, RetryRouteChoice, Screen, StageHeadline, TuiState, UiMessageKind};
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
    /// When this session started drawing. Only the blink phase reads it,
    /// so POD's breathing is tied to the session rather than to whichever
    /// redraw happened to come first.
    started: Instant,
    /// Runs the store said were `Running` on this session's first refresh.
    /// Their drivers died with the instance that started them, so this one
    /// resumes each of them; `None` until that first look is taken.
    orphaned: Option<Vec<RunId>>,
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
            started: Instant::now(),
            orphaned: None,
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
            self.state.motion_phase = motion::active_phase(self.started.elapsed());
            self.state.settle_reaction(Instant::now());
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
                    || matches!(
                        self.state.overlay,
                        Some(Overlay::Attention | Overlay::Continue)
                    );
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
                    self.state.focus_blocking_failure();
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
            Intent::Stop => self.stop(),
            Intent::Retry => self.retry(),
            Intent::Attention => self.open_attention(),
            Intent::Artifact => self.open_artifact(),
            Intent::Logs => self.open_logs(),
            Intent::Diff => self.open_diff(),
            Intent::Apply => self.open_apply_confirmation(),
            Intent::Publish => self.open_publish_confirmation(),
            Intent::Fix => self.request_fix(),
            Intent::Continue => self.open_continue(),
            Intent::FollowUps => self.open_follow_ups(),
            Intent::Discard => self.state.overlay = Some(Overlay::DiscardConfirm),
            Intent::Hide if self.state.screen == Screen::Runs => self.toggle_selected_hidden(),
            Intent::ShowHidden if self.state.screen == Screen::Runs => {
                self.state.show_hidden = !self.state.show_hidden;
                self.refresh();
            }
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
            Overlay::Continue => self.handle_continue_intent(intent),
            Overlay::FollowUps => self.handle_follow_ups_intent(intent),
            Overlay::RetryRoute => self.handle_retry_route_intent(intent),
            Overlay::ApplyConfirm if intent == Intent::Enter => {
                if let Some(run_id) = self.state.selected_run {
                    self.dispatch(WorkerCommand::ApplyRun { run_id });
                    self.state.overlay = None;
                }
            }
            Overlay::PublishConfirm if intent == Intent::Enter => {
                if let Some(run_id) = self.state.selected_run {
                    self.dispatch(WorkerCommand::PublishRun { run_id });
                    self.state.overlay = None;
                }
            }
            Overlay::DiscardConfirm if intent == Intent::Enter => {
                if let Some(run_id) = self.state.selected_run {
                    self.dispatch(WorkerCommand::DiscardRun { run_id });
                    self.state.overlay = None;
                }
            }
            Overlay::Help
            | Overlay::ApplyConfirm
            | Overlay::PublishConfirm
            | Overlay::DiscardConfirm
            | Overlay::Update => {}
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
                let response = (!self.state.attention_response.text().is_empty())
                    .then(|| self.state.attention_response.text().to_owned());
                self.submit_attention(response);
            }
            Intent::Skip => {
                self.submit_attention(Some(crate::app::SKIP_ATTENTION_RESPONSE.to_owned()));
            }
            _ => {}
        }
    }

    /// Resolves the selected attention with `response` (`None` approves a
    /// permission as asked) and closes the overlay.
    fn submit_attention(&mut self, response: Option<String>) {
        let Some(run_id) = self.state.selected_run else {
            return;
        };
        let Some(attention_id) = self.state.selected_attention_id() else {
            self.state
                .set_error("No pending attention request selected");
            return;
        };
        self.dispatch(WorkerCommand::ResolveAttention {
            run_id,
            attention_id,
            response,
        });
        self.state.overlay = None;
    }

    fn handle_new_run_intent(&mut self, intent: Intent) {
        match intent {
            Intent::Escape => self.back(),
            Intent::Tab => self.state.new_run.next_field(),
            Intent::BackTab => self.state.new_run.previous_field(),
            Intent::Left if matches!(self.state.new_run.focus, 1 | 3 | 4) => {
                self.state.new_run.cycle_value(false);
            }
            Intent::Right if matches!(self.state.new_run.focus, 1 | 3 | 4) => {
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
        } else if self.state.overlay == Some(Overlay::Continue) {
            self.state.continue_instruction.paste(text);
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
        if self.dispatch(WorkerCommand::StartRun {
            workflow: self.state.new_run.workflow,
            task,
            repository: PathBuf::from(repository),
            selection: self.state.new_run.execution.selection(),
            effort: self.state.new_run.effort.into(),
        }) {
            // The task belongs to the run that was just dispatched; leaving it
            // in the composer makes the next run open on another run's words.
            // Repository, workflow, execution and effort stay: those are the
            // operator's standing preferences, not this run's content.
            self.state.new_run.task = super::state::TextField::default();
            self.state.new_run.focus = 0;
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

    /// Stop is non-destructive — it preserves the run, its workspace, and its
    /// results — so it dispatches directly rather than through a confirmation
    /// overlay, which this interface reserves for apply and discard. States the
    /// domain cannot interrupt are refused here rather than dispatched.
    fn stop(&mut self) {
        let Some(run_id) = self.state.selected_run else {
            return;
        };
        if !self.state.run_is_stoppable() {
            self.state
                .notify(UiMessageKind::Info, stop_unavailable_reason(&self.state));
            return;
        }
        self.dispatch(WorkerCommand::StopRun { run_id });
    }

    /// Opens the `[t]` chooser for the selected failed stage. Retrying is
    /// two keys rather than one because the question "where?" is the one
    /// worth asking when a provider just ran out of quota: the stage's
    /// configured provider is the first row, so Enter alone keeps today's
    /// behaviour.
    fn retry(&mut self) {
        if self.state.selected_run.is_none() {
            return;
        }
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
        self.state.retry_route_choice = RetryRouteChoice::Configured;
        self.state.overlay = Some(Overlay::RetryRoute);
    }

    fn handle_retry_route_intent(&mut self, intent: Intent) {
        match intent {
            Intent::Down => {
                self.state.retry_route_choice = self.state.retry_route_choice.next();
            }
            Intent::Up => {
                self.state.retry_route_choice = self.state.retry_route_choice.previous();
            }
            Intent::Enter => {
                let Some(run_id) = self.state.selected_run else {
                    self.state.overlay = None;
                    return;
                };
                let Some(stage_id) = self
                    .state
                    .details
                    .as_ref()
                    .and_then(|details| details.stages.get(self.state.selected_stage_index))
                    .map(|stage| stage.id.clone())
                else {
                    self.state.overlay = None;
                    return;
                };
                self.dispatch(WorkerCommand::RetryStage {
                    run_id,
                    stage_id,
                    route: self.state.retry_route_choice.route(),
                });
                self.state.overlay = None;
            }
            _ => {}
        }
    }

    /// Sends a completed run back for one remediation cycle.
    ///
    /// Deliberately not gated on the decision's own words. The verdict is
    /// prose written for a person; the operator read it and pressed the key,
    /// and that is the whole signal. Refusing here for a run the domain would
    /// decline only avoids dispatching a doomed action; it does not weaken
    /// that guard.
    fn request_fix(&mut self) {
        let Some(run_id) = self.state.selected_run else {
            return;
        };
        if self.state.run_can_be_fixed() {
            self.state.fix_when_finished.remove(&run_id);
            self.dispatch(WorkerCommand::RequestFix { run_id });
            return;
        }
        if self.state.run_can_book_a_fix() {
            if self.state.fix_when_finished.remove(&run_id) {
                self.state.notify(
                    UiMessageKind::Info,
                    "Fix cancelled — this run will rest when it finishes.",
                );
            } else {
                self.state.fix_when_finished.insert(run_id);
                self.state.notify(
                    UiMessageKind::Info,
                    "Fix booked — it starts on its own once this run reaches its verdict.",
                );
            }
            return;
        }
        self.state
            .notify(UiMessageKind::Info, fix_unavailable_reason(&self.state));
    }

    /// Opens the free-text "how should the agent continue?" prompt.
    ///
    /// Same eligibility as `[f]` Fix, and the same reason for not gating on
    /// the decision's own words: the operator read the verdict and pressed
    /// the key, and that is the whole signal. Refusing here for a run the
    /// domain would decline only avoids dispatching a doomed action.
    fn open_continue(&mut self) {
        if !self.state.run_can_be_continued() {
            self.state.notify(
                UiMessageKind::Info,
                continue_unavailable_reason(&self.state),
            );
            return;
        }
        self.state.continue_instruction = super::state::TextField::default();
        self.state.overlay = Some(Overlay::Continue);
    }

    fn handle_continue_intent(&mut self, intent: Intent) {
        match intent {
            Intent::Left => self.state.continue_instruction.left(),
            Intent::Right => self.state.continue_instruction.right(),
            Intent::Home => self.state.continue_instruction.home(),
            Intent::End => self.state.continue_instruction.end(),
            Intent::Backspace => self.state.continue_instruction.backspace(),
            Intent::Delete => self.state.continue_instruction.delete(),
            Intent::Character(character) => self.state.continue_instruction.insert(character),
            Intent::Enter => {
                let Some(run_id) = self.state.selected_run else {
                    return;
                };
                let instruction = self.state.continue_instruction.text().trim().to_owned();
                if instruction.is_empty() {
                    self.state.set_error("Instruction cannot be empty");
                    return;
                }
                self.dispatch(WorkerCommand::RequestContinue {
                    run_id,
                    instruction,
                });
                self.state.overlay = None;
            }
            _ => {}
        }
    }

    /// Opens the "work on follow-ups" chooser, pre-filled from the decision
    /// artifact's own `## Follow-ups` section.
    ///
    /// Eligibility is exactly `[c]` Continue's, plus one more thing a verdict
    /// may simply not have written: a Follow-ups section to work from.
    /// Absence is explained rather than silent, the same as every other
    /// refusal in this interface.
    fn open_follow_ups(&mut self) {
        if !self.state.run_can_be_continued() {
            self.state.notify(
                UiMessageKind::Info,
                continue_unavailable_reason(&self.state),
            );
            return;
        }
        let Some(run_id) = self.state.selected_run else {
            return;
        };
        let Some(decision_id) = self.state.details.as_ref().and_then(|details| {
            details
                .stages
                .iter()
                .rev()
                .find(|stage| stage.kind == crate::domain::StageKind::Decision)
                .map(|stage| stage.id.clone())
        }) else {
            return;
        };
        let text = match self.reader.read_artifact(run_id, &decision_id) {
            Ok(artifact) => super::follow_ups::extract(&artifact.text),
            Err(error) => {
                self.state.set_error(error.to_string());
                return;
            }
        };
        let Some(text) = text else {
            self.state.notify(
                UiMessageKind::Info,
                "This decision has no Follow-ups section to work from.",
            );
            return;
        };
        self.state.follow_ups_text = Some(text);
        self.state.follow_ups_as_new_run = false;
        self.state.overlay = Some(Overlay::FollowUps);
    }

    fn handle_follow_ups_intent(&mut self, intent: Intent) {
        match intent {
            Intent::Up | Intent::Down => {
                self.state.follow_ups_as_new_run = !self.state.follow_ups_as_new_run;
            }
            Intent::Enter => {
                let Some(text) = self.state.follow_ups_text.clone() else {
                    self.state.overlay = None;
                    return;
                };
                if self.state.follow_ups_as_new_run {
                    let repository = self
                        .state
                        .details
                        .as_ref()
                        .and_then(|details| details.repository.clone());
                    self.state.new_run.task = super::state::TextField::new(text);
                    if let Some(repository) = repository {
                        self.state.new_run.repository =
                            super::state::TextField::new(repository.display().to_string());
                    }
                    self.state.new_run.focus = 0;
                    self.state.overlay = None;
                    self.state.screen = Screen::NewRun;
                } else {
                    let Some(run_id) = self.state.selected_run else {
                        self.state.overlay = None;
                        return;
                    };
                    self.dispatch(WorkerCommand::RequestContinue {
                        run_id,
                        instruction: text,
                    });
                    self.state.overlay = None;
                }
            }
            _ => {}
        }
    }

    /// Starts the fixes the operator booked while their runs were still working.
    ///
    /// Driven from the listing rather than the detail panel, because a booked
    /// run is usually not the one being watched and waiting for the operator to
    /// select it would defeat the point of booking it. A booking the interface
    /// cannot honour yet — the run is held by another action, or this many
    /// agents are already at work — stays booked and says nothing; the next
    /// refresh is half a second away, and an explanation twice a second is not
    /// worth reading.
    fn start_booked_fixes(&mut self) {
        if self.state.fix_when_finished.is_empty() {
            return;
        }
        let ready = self
            .state
            .runs
            .iter()
            .filter(|item| {
                item.status == crate::domain::RunStatus::Completed
                    && self.state.fix_when_finished.contains(&item.id)
            })
            .map(|item| item.id)
            .collect::<Vec<_>>();
        for run_id in ready {
            let command = WorkerCommand::RequestFix { run_id };
            if self.state.action_refusal(&command).is_some() {
                continue;
            }
            self.state.fix_when_finished.remove(&run_id);
            self.dispatch(command);
        }
    }

    /// Resumes runs a dead instance left behind.
    ///
    /// A running run normally has the action driving it in this process, and
    /// nothing else ever reads its provider processes. A row that is already
    /// `Running` on this session's first refresh therefore has no driver
    /// anywhere — its instance died mid-run — and would sit "running" forever,
    /// even after its processes finish. Resuming reconciles what those
    /// processes wrote and drives the run on. Only runs from that first look
    /// are ever swept: anything that turns `Running` later was started here.
    /// Later sweeps retry an orphan the agent ceiling deferred, and drop one
    /// that moved on without us — resolved from the CLI, or finished.
    fn recover_orphaned_runs(&mut self, runs: &[RunListItem]) {
        let pending = if let Some(pending) = self.orphaned.as_mut() {
            pending.retain(|id| {
                runs.iter()
                    .any(|run| run.id == *id && run.status == RunStatus::Running)
            });
            pending.clone()
        } else {
            let found: Vec<RunId> = runs
                .iter()
                .filter(|run| run.status == RunStatus::Running)
                .map(|run| run.id)
                .collect();
            self.orphaned = Some(found.clone());
            found
        };
        let mut reconnected = 0_usize;
        for run_id in pending {
            let command = WorkerCommand::ResumeRun { run_id };
            if self.state.action_refusal(&command).is_some() {
                continue;
            }
            if let Some(pending) = self.orphaned.as_mut() {
                pending.retain(|id| *id != run_id);
            }
            if self.dispatch(command) {
                reconnected += 1;
            }
        }
        // After the dispatches: starting an action clears the message line.
        if reconnected > 0 {
            let noun = if reconnected == 1 { "run" } else { "runs" };
            self.state.notify(
                UiMessageKind::Info,
                format!("Resuming {reconnected} {noun} left running by a previous session"),
            );
        }
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

    /// Publish shares apply's gate — a completed branch run — because both
    /// transport the same delta; they differ only in where it lands.
    fn open_publish_confirmation(&mut self) {
        let Some(run_id) = self.state.selected_run else {
            return;
        };
        if !self.state.run_is_applyable() {
            self.state
                .notify(UiMessageKind::Info, apply_unavailable_reason(&self.state));
            return;
        }
        match self.reader.preview_run_diff(run_id) {
            Ok(diff) => {
                self.state.diff = Some(diff);
                self.state.overlay = Some(Overlay::PublishConfirm);
            }
            Err(error) => self.state.set_error(error.to_string()),
        }
    }

    fn selected_stage_identity(&self) -> Option<(crate::domain::RunId, crate::domain::StageId)> {
        Some((self.state.selected_run?, self.state.selected_stage.clone()?))
    }

    /// Starts one action, unless another action already holds its run.
    ///
    /// Returns whether the command was dispatched, so a caller that navigates
    /// on success can tell a started action from a refused one.
    fn dispatch(&mut self, command: WorkerCommand) -> bool {
        if let Some(refusal) = self.state.action_refusal(&command) {
            self.state.set_error(refusal.reason());
            return false;
        }
        self.state.begin_action(command.kind(), command.run_id());
        self.worker.send(command);
        self.state.message = None;
        true
    }

    fn receive_worker_results(&mut self) {
        while let Some(result) = self.worker.try_recv() {
            self.handle_worker_result(result);
        }
    }

    fn handle_worker_result(&mut self, result: WorkerResult) {
        self.state.settle_action(result.action, result.run_id);
        match result.result {
            Ok(success) => {
                let run_id = success.report().details.id;
                // An action finishing is not a reason to move the user. Only
                // the run they are already looking at — or the first run this
                // session has, when they are looking at nothing — opens on its
                // own; anything else running in the background reports through
                // the message line and the refreshed list, leaving the screen
                // where the user put it.
                if self
                    .state
                    .selected_run
                    .is_none_or(|selected| selected == run_id)
                {
                    self.state.selected_run = Some(run_id);
                    self.state.replace_details(success.report().details.clone());
                    self.state.quiescent = Some(success.report().outcome.clone());
                    self.state.screen = Screen::RunDetail;
                }
                let message = match success {
                    WorkerSuccess::Applied(outcome, _) => format!("Apply finished: {outcome:?}"),
                    WorkerSuccess::Published(receipt, _) => match receipt.pull_request {
                        crate::workspace::PullRequestStatus::Created(url) => {
                            format!("Pull request created: {url}")
                        }
                        crate::workspace::PullRequestStatus::AlreadyExists(url) => {
                            format!("Branch pushed; pull request already open: {url}")
                        }
                        crate::workspace::PullRequestStatus::Unavailable(reason) => {
                            format!("Branch {} pushed. {reason}", receipt.branch)
                        }
                    },
                    WorkerSuccess::Execution(_) => {
                        format!("{} finished for {run_id}", result.action.label())
                    }
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

    /// Hides the selected run from the default list, or unhides it when the
    /// list is showing hidden runs. Synchronous on purpose: it is a single
    /// metadata update with no processes or workspaces involved, exactly like
    /// the settling writes `list_runs` already performs on this thread.
    fn toggle_selected_hidden(&mut self) {
        let Some(run) = self.state.runs.get(self.state.selected_run_index) else {
            return;
        };
        let (run_id, hidden) = (run.id, run.hidden);
        if let Err(error) = self.reader.set_run_hidden(run_id, !hidden) {
            self.state.set_error(error.to_string());
            return;
        }
        self.refresh();
    }

    fn refresh(&mut self) {
        match self.reader.list_runs() {
            Ok(runs) => {
                self.recover_orphaned_runs(&runs);
                let (visible, hidden_count) = if self.state.show_hidden {
                    (runs, 0)
                } else {
                    let hidden = runs.iter().filter(|run| run.hidden).count();
                    (runs.into_iter().filter(|run| !run.hidden).collect(), hidden)
                };
                self.state.hidden_count = hidden_count;
                self.state.replace_runs(visible);
                self.start_booked_fixes();
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
            Err(error) => {
                // The panel must never keep narrating a different run than the
                // one selected. When the selected run cannot be inspected, its
                // own last details may stand, but another run's are dropped —
                // otherwise two list rows appear to share one run.
                if self
                    .state
                    .details
                    .as_ref()
                    .is_some_and(|details| details.id != run_id)
                {
                    self.state.details = None;
                }
                self.state.set_error(error.to_string());
            }
        }
        if let Ok(artifacts) = self.reader.list_artifacts(run_id) {
            let complete: Vec<_> = artifacts
                .into_iter()
                .filter(|artifact| artifact.status == crate::domain::ArtifactStatus::Complete)
                .collect();
            self.state.stages_with_artifacts = complete
                .iter()
                .map(|artifact| artifact.stage_id.clone())
                .collect();
            self.refresh_headline(run_id, &complete);
        }
        self.refresh_evidence();
    }

    /// The selected stage's own opening line, for the panel that shows it
    /// before the artifact is opened.
    ///
    /// The artifact is read only when the selection lands on a different
    /// artifact than the one already quoted, so the half-second refresh does
    /// not reread and rehash the same file forever. An artifact that fails
    /// its integrity check quotes nothing; opening it is what reports that.
    fn refresh_headline(&mut self, run_id: crate::domain::RunId, artifacts: &[ArtifactSummary]) {
        let Some((_, stage_id)) = self.selected_stage_identity() else {
            self.state.headline = None;
            return;
        };
        let Some(source) = artifacts
            .iter()
            .filter(|artifact| artifact.stage_id == stage_id)
            .max_by_key(|artifact| artifact.attempt)
        else {
            self.state.headline = None;
            return;
        };
        if self
            .state
            .headline
            .as_ref()
            .is_some_and(|headline| headline.describes(&stage_id, source))
        {
            return;
        }
        let Ok(artifact) = self.reader.read_artifact(run_id, &stage_id) else {
            self.state.headline = None;
            return;
        };
        let opening = bottom_line::extract(&artifact.text);
        self.state.headline = Some(StageHeadline {
            stage_id,
            attempt: source.attempt,
            content_size: source.content_size,
            contracted: opening.as_ref().is_some_and(|opening| opening.contracted),
            text: opening.map(|opening| opening.text),
        });
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

/// Explains, in operational language, why a fix is not offered yet.
fn fix_unavailable_reason(state: &TuiState) -> String {
    let Some(details) = state.details.as_ref() else {
        return "Select a run before asking for a fix.".to_owned();
    };
    if details.status != crate::domain::RunStatus::Completed {
        return format!(
            "{} A fix answers a decision, so the run has to reach one first.",
            apply_unavailable_reason(state)
        );
    }
    "This workflow has no decision stage, so there is no verdict to answer.".to_owned()
}

/// Explains, in operational language, why Continue/Work on follow-ups is not
/// offered yet. Same shape as [`fix_unavailable_reason`]: both cycles need a
/// completed run that reached a decision.
fn continue_unavailable_reason(state: &TuiState) -> String {
    let Some(details) = state.details.as_ref() else {
        return "Select a run before continuing it.".to_owned();
    };
    if details.status != crate::domain::RunStatus::Completed {
        return format!(
            "{} Continue answers a decision, so the run has to reach one first.",
            apply_unavailable_reason(state)
        );
    }
    "This workflow has no decision stage, so there is nothing to continue.".to_owned()
}

/// Explains, in operational language, why apply is not offered yet.
/// Why Stop is unavailable, in the run's own terms.
fn stop_unavailable_reason(state: &TuiState) -> String {
    let Some(details) = state.details.as_ref() else {
        return "No run selected.".to_owned();
    };
    match details.status {
        crate::domain::RunStatus::Interrupted => "Run is already stopped — [r] resumes it.",
        crate::domain::RunStatus::Paused => "Run is already suspended — [r] resumes it.",
        crate::domain::RunStatus::Completed | crate::domain::RunStatus::Applied => {
            "Run has already finished."
        }
        crate::domain::RunStatus::Failed => "Run already stopped on a failure.",
        crate::domain::RunStatus::Discarded => "This run was discarded.",
        _ => "Run is not executing yet.",
    }
    .to_owned()
}

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
    use crate::app::{RunDetails, RunListItem, StageSummary};
    use crate::domain::{EffortSetting, Role, RunId, RunStatus, StageId, StageKind, WorkflowKind};
    use crate::tui::state::CONCURRENT_AGENTS;
    use crate::tui::worker::ActionKind;

    /// A `TuiApp` wired to an empty temporary store: enough to drive intents
    /// without touching the user's data directory or any provider.
    /// Ctrl-S in the attention overlay is one keypress: it resolves the
    /// selected permission with the canned decline and closes the overlay,
    /// with nothing typed.
    #[test]
    fn ctrl_s_in_the_attention_overlay_skips_the_selected_permission() {
        let mut run = details(RunStatus::NeedsUser, WorkflowKind::Deep);
        run.attention = vec![crate::app::AttentionSummary {
            id: crate::domain::AttentionRequestId::from_u128(1),
            stage_id: StageId::new("implementation").unwrap(),
            kind: crate::domain::AttentionKind::Permission,
            summary: "Claude Code requests permission for: Bash".to_owned(),
        }];
        let (mut app, _fixture) = app_with(run);
        app.state.overlay = Some(Overlay::Attention);
        app.handle_intent(Intent::Skip);
        assert_eq!(app.state.overlay, None);
        assert!(
            app.state
                .in_flight
                .iter()
                .any(|action| action.action == ActionKind::ResolveAttention),
            "skip must dispatch a resolution: {:?}",
            app.state.in_flight
        );
        assert!(app.state.attention_response.text().is_empty());
    }

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
            started: Instant::now(),
            orphaned: None,
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
            // A review is prepared detached, and adopts a branch only when it
            // is sent back to fix what it found. The fixture says so, because
            // that is what decides whether apply is offered.
            workspace_mode: Some(if workflow == WorkflowKind::Review {
                crate::workspace::WorkspaceMode::Detached
            } else {
                crate::workspace::WorkspaceMode::Branch
            }),
            workspace_status: Some(crate::workspace::WorkspaceStatus::Ready),
            base_commit: Some("abc1234".to_owned()),
            profile: "recommended".to_owned(),
            profile_version: "recommended_v2".to_owned(),
            // A run can only be fixed if its configuration routes the roles a
            // fix adds and states their effort, so the fixture carries both.
            routes: [Role::Implementer, Role::EngineeringLead]
                .into_iter()
                .map(|role| crate::app::RouteSummary {
                    role,
                    configured_provider: "codex".to_owned(),
                    configured_model: None,
                    reason: "test".to_owned(),
                    requested_effort: Some(EffortSetting::NativeDefault),
                })
                .collect(),
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
                observed_effort: None,
                configured_model: None,
                route_overridden: false,
                actual_provider: None,
                actual_model: None,
                provider_session_record: None,
                native_session: None,
                provider_session_status: None,
                process_status: None,
                started_at: None,
                finished_at: None,
                waiting: None,
                failure_reason: None,
                blocking: false,
            }],
            attention: Vec::new(),
            usage: crate::app::RunUsage::default(),
            started_at: None,
            finished_at: None,
            failure_reason: None,
            image_generations: Vec::new(),
        }
    }

    /// The same run, having reached a verdict a person can reject.
    fn decided(mut details: RunDetails) -> RunDetails {
        let mut decision = details.stages[0].clone();
        decision.id = StageId::new("decision").unwrap();
        decision.kind = StageKind::Decision;
        decision.role = Role::EngineeringLead;
        decision.status = StageStatus::Completed;
        details.stages.push(decision);
        details
    }

    /// The same run as configuration sealed before fix-cycle routing wrote it:
    /// a verdict to answer, and no route for the role that would answer it.
    fn unroutable(mut details: RunDetails) -> RunDetails {
        details
            .routes
            .retain(|route| route.role != Role::Implementer);
        details
    }

    /// A panel narrating run A while run B is selected makes two list rows
    /// appear to share one run. When the selected run cannot be inspected,
    /// another run's details are dropped rather than left standing.
    #[test]
    fn anothers_details_are_dropped_when_the_selected_run_cannot_be_inspected() {
        let (mut app, _fixture) = app_with(details(RunStatus::Completed, WorkflowKind::Review));
        app.state.selected_run = Some(RunId::from_u128(8));
        app.refresh_selected();
        assert!(
            app.state.details.is_none(),
            "run 7's details must not stand in for unreadable run 8"
        );
        assert!(app.state.message.is_some());
    }

    /// The selected run's own last details may stand while it is unreadable:
    /// they are stale, but they are its own story, and dropping them would
    /// blank the panel on every transient read failure.
    #[test]
    fn a_runs_own_details_survive_its_inspect_failure() {
        let (mut app, _fixture) = app_with(details(RunStatus::Completed, WorkflowKind::Review));
        app.refresh_selected();
        assert!(app.state.details.is_some());
        assert_eq!(app.state.details.as_ref().unwrap().id, RunId::from_u128(7));
    }

    /// "Fix it" answers a decision, so it is offered exactly where one exists
    /// and the run has finished reaching it.
    #[test]
    fn fix_is_dispatched_only_for_a_completed_run_that_reached_a_decision() {
        for (label, run) in [
            (
                "a rejected standard run is exactly what fix is for",
                decided(details(RunStatus::Completed, WorkflowKind::Standard)),
            ),
            (
                "a review reaches a verdict too, and may be sent back to act on it",
                decided(details(RunStatus::Completed, WorkflowKind::Review)),
            ),
        ] {
            let (mut app, _fixture) = app_with(run);
            app.handle_intent(Intent::Fix);
            assert_eq!(
                app.state.busy_label().as_deref(),
                Some("fixing run"),
                "{label}"
            );
        }

        for (label, run) in [
            (
                "already applied",
                decided(details(RunStatus::Applied, WorkflowKind::Standard)),
            ),
            (
                "no decision stage to answer",
                details(RunStatus::Completed, WorkflowKind::Fast),
            ),
            (
                "configuration sealed before fix-cycle routing",
                unroutable(decided(details(RunStatus::Completed, WorkflowKind::Review))),
            ),
        ] {
            let (mut app, _fixture) = app_with(run);
            app.handle_intent(Intent::Fix);
            assert!(app.state.in_flight.is_empty(), "fix dispatched for {label}");
            // A refusal explains itself rather than doing nothing visible.
            assert!(
                app.state.message.is_some(),
                "no explanation offered for {label}"
            );
        }
    }

    /// The operator usually knows they want a fix long before the verdict
    /// lands. Booking it means they say so once, and stop watching for the run
    /// to end.
    #[test]
    fn a_fix_booked_while_a_run_works_starts_itself_when_the_run_finishes() {
        let run_id = RunId::from_u128(7);
        let (mut app, _fixture) =
            app_with(decided(details(RunStatus::Running, WorkflowKind::Review)));

        app.handle_intent(Intent::Fix);
        assert!(
            app.state.in_flight.is_empty(),
            "booking starts nothing while the run is still working"
        );
        assert!(app.state.fix_when_finished.contains(&run_id));

        app.handle_intent(Intent::Fix);
        assert!(
            !app.state.fix_when_finished.contains(&run_id),
            "the same key cancels a booking the operator changed their mind about"
        );

        app.handle_intent(Intent::Fix);
        app.state.runs = vec![RunListItem {
            id: run_id,
            workflow: WorkflowKind::Review,
            status: RunStatus::Completed,
            task_summary: "Add OAuth provider support".to_owned(),
            repository: Some(std::path::PathBuf::from("/repo")),
            updated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            hidden: false,
        }];
        app.start_booked_fixes();

        assert_eq!(app.state.busy_label().as_deref(), Some("fixing run"));
        assert!(
            app.state.fix_when_finished.is_empty(),
            "a booking is spent once it is honoured, not replayed every refresh"
        );
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
            // Publish shares apply's gate, so both intents refuse identically.
            for intent in [Intent::Apply, Intent::Publish] {
                let (mut app, _fixture) = app_with(details(status, workflow));
                app.handle_intent(intent);
                assert_eq!(
                    app.state.overlay, None,
                    "{intent:?} confirmation must not open for {status:?}/{workflow:?}"
                );
                assert!(
                    app.state.in_flight.is_empty(),
                    "no worker command dispatched for {status:?}/{workflow:?}"
                );
                let message = app.state.message.as_ref().expect("user is told why");
                assert_eq!(message.kind, UiMessageKind::Info, "refusal is not an error");
            }
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
            crate::app::DevelopmentFakeProviderFactory::new(fixture.path().join("runs")),
        )
        .start_run(
            WorkflowKind::Standard,
            "mission deck apply gate",
            repo,
            Some(crate::app::ExecutionSelection::Uniform(
                crate::app::UniformProvider::Fake,
            )),
            EffortSetting::NativeDefault,
            &crate::app::ImageGenerationPlan::disabled(),
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
            started: Instant::now(),
            orphaned: None,
        };
        assert!(app.state.run_is_applyable());
        app.handle_intent(Intent::Apply);
        assert_eq!(app.state.overlay, Some(Overlay::ApplyConfirm));
        assert!(
            app.state.in_flight.is_empty(),
            "opening the confirmation still dispatches nothing"
        );

        // Publish rides the same gate and diff preview, behind its own
        // confirmation.
        app.handle_intent(Intent::Escape);
        assert_eq!(app.state.overlay, None);
        app.handle_intent(Intent::Publish);
        assert_eq!(app.state.overlay, Some(Overlay::PublishConfirm));
        assert!(
            app.state.in_flight.is_empty(),
            "opening the publish confirmation still dispatches nothing"
        );
    }

    /// Continue and Work on follow-ups share Fix's own eligibility: a
    /// completed run that reached a decision whose sealed configuration can
    /// route the cycle it would grow.
    #[test]
    fn continue_and_follow_ups_share_fixs_own_eligibility() {
        for (label, run) in [
            (
                "a rejected standard run",
                decided(details(RunStatus::Completed, WorkflowKind::Standard)),
            ),
            (
                "a decided review",
                decided(details(RunStatus::Completed, WorkflowKind::Review)),
            ),
        ] {
            let (app, _fixture) = app_with(run);
            assert!(app.state.run_can_be_continued(), "{label}");
        }

        for (label, run) in [
            (
                "still running",
                details(RunStatus::Running, WorkflowKind::Standard),
            ),
            (
                "no decision stage",
                details(RunStatus::Completed, WorkflowKind::Fast),
            ),
            (
                "configuration sealed before continue-cycle routing",
                unroutable(decided(details(RunStatus::Completed, WorkflowKind::Review))),
            ),
        ] {
            let (app, _fixture) = app_with(run);
            assert!(!app.state.run_can_be_continued(), "{label}");
        }
    }

    /// `[c]` opens the overlay only where eligible, and explains itself
    /// otherwise — the same shape as Fix's own refusal.
    #[test]
    fn continue_overlay_opens_only_for_a_completed_run_that_reached_a_decision() {
        let (mut app, _fixture) = app_with(decided(details(
            RunStatus::Completed,
            WorkflowKind::Standard,
        )));
        app.handle_intent(Intent::Continue);
        assert_eq!(app.state.overlay, Some(Overlay::Continue));
        assert!(app.state.continue_instruction.text().is_empty());

        let (mut app, _fixture) = app_with(details(RunStatus::Completed, WorkflowKind::Fast));
        app.handle_intent(Intent::Continue);
        assert_eq!(
            app.state.overlay, None,
            "no decision stage to continue past"
        );
        assert!(app.state.message.is_some(), "the refusal explains itself");
    }

    /// Typed text becomes the dispatched instruction; a blank one is
    /// refused without closing the overlay; Escape cancels without
    /// dispatching anything.
    #[test]
    fn continue_overlay_input_flow_submits_or_cancels() {
        let (mut app, _fixture) = app_with(decided(details(
            RunStatus::Completed,
            WorkflowKind::Standard,
        )));
        app.handle_intent(Intent::Continue);
        for character in "add tests".chars() {
            app.handle_intent(Intent::Character(character));
        }
        app.handle_intent(Intent::Enter);
        assert_eq!(app.state.overlay, None);
        assert_eq!(app.state.busy_label().as_deref(), Some("continuing run"));

        let (mut app, _fixture) = app_with(decided(details(
            RunStatus::Completed,
            WorkflowKind::Standard,
        )));
        app.handle_intent(Intent::Continue);
        app.handle_intent(Intent::Enter);
        assert_eq!(
            app.state.overlay,
            Some(Overlay::Continue),
            "a blank instruction is refused, and the overlay stays open to fix it"
        );
        assert!(app.state.message.is_some());

        let (mut app, _fixture) = app_with(decided(details(
            RunStatus::Completed,
            WorkflowKind::Standard,
        )));
        app.handle_intent(Intent::Continue);
        app.handle_intent(Intent::Character('x'));
        app.handle_intent(Intent::Escape);
        assert_eq!(app.state.overlay, None);
        assert!(
            app.state.in_flight.is_empty(),
            "Escape cancels without dispatching"
        );
    }

    /// Builds a real completed Standard run through a real git repository and
    /// the Fake provider, mirroring
    /// `apply_opens_confirmation_for_a_real_completed_branch_run`'s own
    /// setup. Follow-ups extraction reads an actual artifact file through the
    /// store, so it needs a genuine run rather than the lightweight
    /// `RunDetails` fixtures the eligibility tests use.
    fn real_completed_standard_run() -> (TempDir, PathBuf, PathBuf, PathBuf, RunDetails) {
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
            crate::app::DevelopmentFakeProviderFactory::new(fixture.path().join("runs")),
        )
        .start_run(
            WorkflowKind::Standard,
            "follow-ups fixture",
            &repo,
            Some(crate::app::ExecutionSelection::Uniform(
                crate::app::UniformProvider::Fake,
            )),
            EffortSetting::NativeDefault,
            &crate::app::ImageGenerationPlan::disabled(),
        )
        .unwrap();
        assert_eq!(report.details.status, RunStatus::Completed);
        (fixture, database, worktrees, repo, report.details)
    }

    fn tui_app_over(
        fixture_path: &std::path::Path,
        database: PathBuf,
        worktrees: PathBuf,
        details: RunDetails,
    ) -> TuiApp {
        let mut state = TuiState::new(fixture_path);
        state.screen = Screen::RunDetail;
        state.selected_run = Some(details.id);
        state.replace_details(details);
        TuiApp {
            state,
            reader: RunService::new(database.clone(), worktrees.clone(), RuntimeProviderFactory),
            worker: Worker::spawn(RunService::new(database, worktrees, RuntimeProviderFactory)),
            update: mpsc::channel().1,
            installing: None,
            last_refresh: Instant::now(),
            started: Instant::now(),
            orphaned: None,
        }
    }

    /// Persists one Markdown artifact directly through the store, the same
    /// way `artifact_read_revalidates_hash_and_does_not_mutate_run` in
    /// `run_service.rs` does: real content behind a real artifact record,
    /// without needing a provider to have written it.
    fn insert_markdown_artifact(
        fixture_path: &std::path::Path,
        database: &std::path::Path,
        run_id: RunId,
        stage_id: &StageId,
        content: &str,
    ) {
        let artifact_path = fixture_path.join(format!("{stage_id}.md"));
        std::fs::write(&artifact_path, content).unwrap();
        let created_at: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
        let metadata = crate::domain::ArtifactMetadata::new(
            crate::domain::ArtifactId::new(),
            run_id,
            stage_id.clone(),
            crate::domain::ArtifactKind::Decision,
            Role::EngineeringLead,
            crate::domain::ArtifactStatus::Complete,
            created_at,
        )
        .with_provider(crate::domain::ProviderId::new("fake").unwrap(), None);
        let bytes = content.as_bytes();
        let artifact = crate::providers::ArtifactRecord::new(
            metadata,
            1,
            artifact_path,
            sha256_hex(bytes),
            u64::try_from(bytes.len()).unwrap(),
            created_at,
        )
        .unwrap();
        let mut store = crate::store::SqliteStore::open(database).unwrap();
        store.insert_artifact(&artifact).unwrap();
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;

        let mut hash = String::with_capacity(64);
        for byte in Sha256::digest(bytes) {
            write!(hash, "{byte:02x}").expect("writing to String cannot fail");
        }
        hash
    }

    fn decision_stage_id(details: &RunDetails) -> StageId {
        details
            .stages
            .iter()
            .rev()
            .find(|stage| stage.kind == StageKind::Decision)
            .expect("Standard workflow reaches a decision")
            .id
            .clone()
    }

    /// The whole `[w]` path, end to end: the decision's own `## Follow-ups`
    /// section is extracted verbatim and offered back either to this run or
    /// to a new one, exactly the choice the spec describes.
    #[test]
    fn follow_ups_offers_working_in_this_run_or_starting_a_new_one() {
        let (fixture, database, worktrees, repo, details) = real_completed_standard_run();
        let decision_id = decision_stage_id(&details);
        insert_markdown_artifact(
            fixture.path(),
            &database,
            details.id,
            &decision_id,
            "# Decision\n\n## Verdict\n\nApproved.\n\n## Follow-ups\n- Add integration coverage\n- Generalize the helper\n",
        );

        // Default choice: continue in this run.
        let mut app = tui_app_over(
            fixture.path(),
            database.clone(),
            worktrees.clone(),
            details.clone(),
        );
        app.handle_intent(Intent::FollowUps);
        assert_eq!(app.state.overlay, Some(Overlay::FollowUps));
        assert_eq!(
            app.state.follow_ups_text.as_deref(),
            Some("- Add integration coverage\n- Generalize the helper")
        );
        app.handle_intent(Intent::Enter);
        assert_eq!(app.state.overlay, None);
        assert_eq!(app.state.busy_label().as_deref(), Some("continuing run"));

        // Toggled choice: hand the same text to a new run's composer, with
        // the repository the run itself was created against — canonicalized
        // by discovery, which is why this compares against the run's own
        // recorded repository rather than the pre-canonicalization path the
        // fixture built it from.
        let expected_repository = details
            .repository
            .clone()
            .expect("a started run always records its repository");
        assert_eq!(expected_repository.file_name(), repo.file_name());
        let mut app = tui_app_over(fixture.path(), database, worktrees, details);
        app.handle_intent(Intent::FollowUps);
        app.handle_intent(Intent::Up);
        app.handle_intent(Intent::Enter);
        assert_eq!(app.state.overlay, None);
        assert_eq!(app.state.screen, Screen::NewRun);
        assert_eq!(
            app.state.new_run.task.text(),
            "- Add integration coverage\n- Generalize the helper"
        );
        assert_eq!(
            app.state.new_run.repository.text(),
            expected_repository.display().to_string()
        );
    }

    /// A decision that wrote no Follow-ups section explains why, rather than
    /// opening an empty chooser.
    #[test]
    fn follow_ups_explains_when_the_decision_wrote_no_such_section() {
        let (fixture, database, worktrees, _repo, details) = real_completed_standard_run();
        let decision_id = decision_stage_id(&details);
        insert_markdown_artifact(
            fixture.path(),
            &database,
            details.id,
            &decision_id,
            "# Decision\n\n## Verdict\n\nApproved.\n",
        );

        let mut app = tui_app_over(fixture.path(), database, worktrees, details);
        app.handle_intent(Intent::FollowUps);
        assert_eq!(app.state.overlay, None);
        assert!(
            app.state
                .message
                .as_ref()
                .is_some_and(|message| message.text.contains("Follow-ups")),
            "the refusal names what is missing"
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
        assert!(app.state.in_flight.is_empty());
    }

    #[test]
    fn stop_dispatches_only_for_an_executing_run_and_never_discards() {
        for status in [RunStatus::Running, RunStatus::NeedsUser] {
            let (mut app, _fixture) = app_with(details(status, WorkflowKind::Standard));
            app.handle_intent(Intent::Stop);
            assert!(
                !app.state.in_flight.is_empty(),
                "{status:?} is stoppable, so the action dispatches"
            );
            assert_eq!(
                app.state.overlay, None,
                "{status:?}: stop is non-destructive and opens no confirmation"
            );
        }

        for status in [
            RunStatus::Completed,
            RunStatus::Applied,
            RunStatus::Discarded,
            RunStatus::Failed,
            RunStatus::Interrupted,
            RunStatus::Paused,
        ] {
            let (mut app, _fixture) = app_with(details(status, WorkflowKind::Standard));
            app.handle_intent(Intent::Stop);
            assert!(
                app.state.in_flight.is_empty(),
                "{status:?} must not dispatch a stop"
            );
            assert!(
                app.state.message.is_some(),
                "{status:?}: the refusal explains itself"
            );
            assert_eq!(app.state.overlay, None);
        }
    }

    /// The same run under a different identity, for the cases that need two.
    fn other_run(mut details: RunDetails, id: RunId) -> RunDetails {
        details.id = id;
        details
    }

    fn start_command() -> WorkerCommand {
        WorkerCommand::StartRun {
            workflow: WorkflowKind::Standard,
            task: "a second piece of work".to_owned(),
            repository: std::path::PathBuf::from("/repo"),
            selection: crate::app::ExecutionSelection::Uniform(crate::app::UniformProvider::Fake),
            effort: crate::app::EffortRequest::ProfileDefault,
        }
    }

    /// The regression this gate was rebuilt around: one busy run used to lock
    /// the whole interface, so starting a second run — the thing the command
    /// line could always do — was refused for as long as the first one ran.
    #[test]
    fn work_on_one_run_never_holds_back_another_run_or_a_new_one() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        let first = RunId::from_u128(7);
        let second = RunId::from_u128(8);

        assert!(app.dispatch(WorkerCommand::ResumeRun { run_id: first }));
        assert!(
            app.dispatch(WorkerCommand::ResumeRun { run_id: second }),
            "another run's work is none of the first run's business"
        );
        assert!(
            app.dispatch(start_command()),
            "a new run holds no existing run, so nothing can hold it back"
        );
        assert_eq!(app.state.in_flight.len(), 3, "all three are working");
    }

    /// Every choice field in the composer answers to the arrow keys. Effort
    /// is the fourth field; it must cycle like workflow and execution do,
    /// not fall through to the text editor that ignores it.
    #[test]
    fn arrow_keys_cycle_the_composer_effort_field() {
        use crate::tui::state::EFFORT_CHOICES;
        let (mut app, _fixture) = app_with(details(RunStatus::Completed, WorkflowKind::Standard));
        app.state.screen = Screen::NewRun;
        app.state.new_run.focus = 4;
        let start = app.state.new_run.effort;
        app.handle_intent(Intent::Right);
        let next = app.state.new_run.effort;
        assert_ne!(next, start, "Right must move Effort to another choice");
        assert_eq!(
            next,
            EFFORT_CHOICES[(EFFORT_CHOICES.iter().position(|c| *c == start).unwrap() + 1)
                % EFFORT_CHOICES.len()],
            "Right steps forward through EFFORT_CHOICES"
        );
        app.handle_intent(Intent::Left);
        assert_eq!(
            app.state.new_run.effort, start,
            "Left steps back to where it was"
        );
    }

    /// The composer's task belongs to the run it starts; reopening the
    /// composer must not offer a finished run's words as the next draft.
    #[test]
    fn a_dispatched_task_does_not_linger_in_the_composer() {
        let (mut app, _fixture) = app_with(details(RunStatus::Completed, WorkflowKind::Standard));
        app.state.screen = Screen::NewRun;
        app.state.new_run.task =
            crate::tui::state::TextField::new("ship the follow-ups".to_owned());
        app.state.new_run.focus = 3;
        app.handle_intent(Intent::Enter);
        assert_eq!(app.state.busy_label().as_deref(), Some("starting run"));
        assert_eq!(
            app.state.new_run.task.text(),
            "",
            "the dispatched task must not become the next run's draft"
        );
        assert_eq!(
            app.state.new_run.focus, 0,
            "an empty composer starts at its task"
        );
        assert!(
            !app.state.new_run.repository.text().is_empty(),
            "the repository is a standing preference, not run content, and survives"
        );
    }

    /// Two actions on one run would race each other over the same durable
    /// state, so that pairing — and only that pairing — is still refused.
    #[test]
    fn a_second_action_on_the_same_run_is_refused_and_names_the_holder() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        let run_id = RunId::from_u128(7);

        assert!(app.dispatch(WorkerCommand::ResumeRun { run_id }));
        assert!(!app.dispatch(WorkerCommand::ApplyRun { run_id }));
        let message = app
            .state
            .message
            .as_ref()
            .expect("the refusal explains itself");
        assert!(
            message.text.contains("resuming run"),
            "the refusal names what is holding the run: {:?}",
            message.text
        );
        assert_eq!(app.state.in_flight.len(), 1, "nothing extra was started");
    }

    /// Concurrency is not unlimited. Each agent at work holds a worktree, a
    /// terminal session and a provider process, so the interface stops handing
    /// out more of them than it means to run at once.
    #[test]
    fn agents_stop_being_started_at_the_ceiling() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        for index in 0..CONCURRENT_AGENTS {
            let run_id = RunId::from_u128(100 + index as u128);
            assert!(
                app.dispatch(WorkerCommand::ResumeRun { run_id }),
                "agent {index} is within the ceiling"
            );
        }

        assert!(
            !app.dispatch(start_command()),
            "one more agent than the ceiling is refused"
        );
        let message = app
            .state
            .message
            .as_ref()
            .expect("the refusal explains itself");
        assert!(
            message.text.contains("already working"),
            "the refusal says why: {:?}",
            message.text
        );

        // The ceiling counts agents, not actions: the work that only touches
        // the store and the checkout still goes out, and stop most of all —
        // it is how the user gets back under the ceiling.
        assert!(app.dispatch(WorkerCommand::StopRun {
            run_id: RunId::from_u128(100)
        }));
        assert!(app.dispatch(WorkerCommand::DiscardRun {
            run_id: RunId::from_u128(200)
        }));
    }

    /// Stop is the way out of a run that has stopped making progress, so the
    /// action it is interrupting must never be the reason it cannot run.
    #[test]
    fn stop_is_never_held_back_by_the_action_it_interrupts() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        let run_id = RunId::from_u128(7);
        assert!(app.dispatch(WorkerCommand::ResumeRun { run_id }));

        app.handle_intent(Intent::Stop);

        assert_eq!(
            app.state.in_flight.len(),
            2,
            "the stop went out alongside the resume holding the run"
        );
        assert!(
            app.state
                .in_flight
                .iter()
                .any(|entry| entry.action == ActionKind::Stop),
            "the stop is one of them"
        );
    }

    /// Several runs at once means results arrive while the user is reading
    /// something else. A finished background run reports; it does not grab
    /// the screen the user chose.
    #[test]
    fn a_background_run_finishing_never_moves_the_user() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        app.state.screen = Screen::Logs;
        let finished = other_run(
            details(RunStatus::Completed, WorkflowKind::Standard),
            RunId::from_u128(8),
        );

        app.handle_worker_result(WorkerResult {
            action: ActionKind::Start,
            run_id: None,
            result: Ok(WorkerSuccess::Execution(crate::app::ExecutionReport {
                details: finished,
                committed_events: Vec::new(),
                outcome: crate::app::QuiescentState::Completed,
            })),
        });

        assert_eq!(
            app.state.screen,
            Screen::Logs,
            "the user stays where it was"
        );
        // The fixture store holds no runs, so the refresh that follows every
        // result clears the selection here. What matters is that the finished
        // run did not take it.
        assert_ne!(
            app.state.selected_run,
            Some(RunId::from_u128(8)),
            "a background run never selects itself"
        );
        let message = app.state.message.as_ref().expect("but it is announced");
        assert!(
            message.text.contains(&RunId::from_u128(8).to_string()),
            "the announcement says which run finished: {:?}",
            message.text
        );
    }

    /// The run on screen is the one the user is waiting on, so its result
    /// still opens by itself.
    #[test]
    fn the_selected_run_finishing_still_opens_its_result() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        app.state.screen = Screen::Runs;

        app.handle_worker_result(WorkerResult {
            action: ActionKind::Resume,
            run_id: Some(RunId::from_u128(7)),
            result: Ok(WorkerSuccess::Execution(crate::app::ExecutionReport {
                details: details(RunStatus::Completed, WorkflowKind::Standard),
                committed_events: Vec::new(),
                outcome: crate::app::QuiescentState::Completed,
            })),
        });

        assert_eq!(app.state.screen, Screen::RunDetail);
        assert_eq!(
            app.state.quiescent,
            Some(crate::app::QuiescentState::Completed)
        );
        assert!(
            app.state.in_flight.is_empty(),
            "the finished action stopped holding its run"
        );
    }

    /// One label reads as itself; several would crowd out the run identity
    /// the header exists to show, so they are counted instead.
    #[test]
    fn the_header_names_one_action_and_counts_more() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        assert_eq!(app.state.busy_label(), None, "nothing to say when idle");

        assert!(app.dispatch(WorkerCommand::ResumeRun {
            run_id: RunId::from_u128(7)
        }));
        assert_eq!(app.state.busy_label().as_deref(), Some("resuming run"));

        assert!(app.dispatch(WorkerCommand::ResumeRun {
            run_id: RunId::from_u128(8)
        }));
        assert_eq!(
            app.state.busy_label().as_deref(),
            Some("2 actions in flight")
        );
    }

    #[test]
    fn quit_still_detaches_without_stopping_the_run() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        app.handle_intent(Intent::Quit);
        assert!(app.state.quit, "q still leaves the frontend");
        assert!(
            app.state.in_flight.is_empty(),
            "detaching never interrupts the run"
        );
    }

    #[test]
    fn stop_refusal_names_the_blocking_state() {
        for (status, expected) in [
            (RunStatus::Interrupted, "already stopped"),
            (RunStatus::Completed, "already finished"),
            (RunStatus::Discarded, "discarded"),
        ] {
            let (app, _fixture) = app_with(details(status, WorkflowKind::Standard));
            let reason = stop_unavailable_reason(&app.state);
            assert!(
                reason.contains(expected),
                "{status:?}: {reason:?} should mention {expected:?}"
            );
        }
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

    fn listed(id: u128, status: RunStatus) -> RunListItem {
        RunListItem {
            id: RunId::from_u128(id),
            workflow: WorkflowKind::Standard,
            status,
            task_summary: "task".to_owned(),
            repository: None,
            updated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            hidden: false,
        }
    }

    fn holds_resume_for(app: &TuiApp, id: u128) -> bool {
        app.state.in_flight.iter().any(|entry| {
            entry.action == ActionKind::Resume && entry.run_id == Some(RunId::from_u128(id))
        })
    }

    #[test]
    fn first_refresh_resumes_runs_left_running_by_a_dead_instance() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        let runs = vec![
            listed(1, RunStatus::Running),
            listed(2, RunStatus::Completed),
            listed(3, RunStatus::Paused),
        ];

        app.recover_orphaned_runs(&runs);

        assert!(holds_resume_for(&app, 1), "the orphan gets a driver");
        assert!(!holds_resume_for(&app, 2), "finished runs are left alone");
        assert!(!holds_resume_for(&app, 3), "paused is the user's decision");
        assert!(
            app.state
                .message
                .as_ref()
                .is_some_and(|message| message.text.contains("previous session")),
            "the user hears why an agent started on its own"
        );
    }

    #[test]
    fn runs_turning_running_after_the_first_look_are_never_swept() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        app.recover_orphaned_runs(&[]);

        // A run started by this session shows up Running on a later refresh
        // while its own start action still has no run id to hold it with.
        app.recover_orphaned_runs(&[listed(4, RunStatus::Running)]);

        assert!(
            !holds_resume_for(&app, 4),
            "its driver lives here; a second one would race it"
        );
    }

    #[test]
    fn an_orphan_deferred_by_the_agent_ceiling_is_retried_until_it_starts() {
        let (mut app, _fixture) = app_with(details(RunStatus::Running, WorkflowKind::Standard));
        for _ in 0..CONCURRENT_AGENTS {
            app.state.begin_action(ActionKind::Start, None);
        }
        let runs = vec![listed(5, RunStatus::Running)];

        app.recover_orphaned_runs(&runs);
        assert!(!holds_resume_for(&app, 5), "no capacity yet");

        app.state.settle_action(ActionKind::Start, None);
        app.recover_orphaned_runs(&runs);
        assert!(holds_resume_for(&app, 5), "resumed once capacity frees up");

        app.recover_orphaned_runs(&runs);
        assert_eq!(
            app.state
                .in_flight
                .iter()
                .filter(|entry| entry.run_id == Some(RunId::from_u128(5)))
                .count(),
            1,
            "an orphan is resumed exactly once"
        );
    }

    /// `t` asks where the failed stage goes before retrying it. The first
    /// row is the configured provider, so Enter alone is the plain retry;
    /// the other rows record an override for this stage only.
    #[test]
    fn retry_opens_the_route_chooser_and_dispatches_the_chosen_provider() {
        let mut failed = details(RunStatus::Failed, WorkflowKind::Fast);
        failed.stages[0].status = StageStatus::Failed;
        let (mut app, _fixture) = app_with(failed);

        app.handle_intent(Intent::Retry);
        assert_eq!(app.state.overlay, Some(Overlay::RetryRoute));
        assert_eq!(app.state.retry_route_choice, RetryRouteChoice::Configured);
        assert!(
            app.state.in_flight.is_empty(),
            "choosing dispatches nothing"
        );

        app.handle_intent(Intent::Down);
        assert_eq!(app.state.retry_route_choice, RetryRouteChoice::Claude);
        assert_eq!(
            app.state.retry_route_choice.route(),
            Some(crate::app::RetryRoute::new(
                crate::app::UniformProvider::Claude,
                None
            ))
        );
        app.handle_intent(Intent::Up);
        assert_eq!(app.state.retry_route_choice, RetryRouteChoice::Configured);
        assert_eq!(app.state.retry_route_choice.route(), None);
        app.handle_intent(Intent::Up);
        assert_eq!(app.state.retry_route_choice, RetryRouteChoice::Codex);

        app.handle_intent(Intent::Enter);
        assert_eq!(app.state.overlay, None);
        assert!(
            app.state
                .in_flight
                .iter()
                .any(|action| action.action == super::super::worker::ActionKind::Retry),
            "Enter dispatches the retry"
        );

        // A stage that is not failed has nothing to retry, so no chooser.
        let mut running = details(RunStatus::Running, WorkflowKind::Fast);
        running.stages[0].status = StageStatus::Running;
        let (mut app, _fixture) = app_with(running);
        app.handle_intent(Intent::Retry);
        assert_eq!(app.state.overlay, None);
    }
}
