use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};

use crate::app::{AppError, RunService, RuntimeProviderFactory};
use crate::domain::{StageId, StageStatus};

use super::input::{Intent, map_key, map_text_key};
use super::render;
use super::state::{Overlay, Screen, TuiState, UiMessageKind};
use super::terminal::TerminalSession;
use super::worker::{Worker, WorkerCommand, WorkerResult, WorkerSuccess};

const EVENT_POLL: Duration = Duration::from_millis(100);
const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

pub(crate) struct TuiApp {
    state: TuiState,
    reader: RunService<RuntimeProviderFactory>,
    worker: Worker,
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
            last_refresh: Instant::now()
                .checked_sub(REFRESH_INTERVAL)
                .unwrap_or_else(Instant::now),
        })
    }

    pub(crate) fn run(mut self, terminal: &mut TerminalSession) -> anyhow::Result<()> {
        self.refresh();
        while !self.state.quit {
            self.receive_worker_results();
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
            Intent::Help => self.state.overlay = Some(Overlay::Help),
            _ => {}
        }
    }

    fn handle_overlay_intent(&mut self, overlay: Overlay, intent: Intent) {
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
            Overlay::Help | Overlay::ApplyConfirm | Overlay::DiscardConfirm => {}
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

#[cfg(test)]
mod tests {
    use super::*;

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
