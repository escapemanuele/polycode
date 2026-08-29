use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::app::{
    ArtifactView, ExecutionSelection, ProcessLogView, QuiescentState, RunDetails, RunDiffPreview,
    RunListItem, StageExecutionEvidence, UniformProvider,
};
use crate::domain::{AttentionRequestId, EffortSetting, RunId, StageId, WorkflowKind};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Screen {
    #[default]
    Runs,
    RunDetail,
    Artifact,
    Logs,
    Diff,
    NewRun,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Overlay {
    Help,
    Attention,
    ApplyConfirm,
    DiscardConfirm,
    /// Application-level software update. Deliberately the lowest-priority
    /// overlay: run attention always outranks it.
    Update,
}

/// Presentation-level notification severity. TUI-only; durable state that
/// needs user action stays in `RunDetails`/attention and is rendered from
/// there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UiMessageKind {
    Info,
    Success,
    Warning,
    Error,
}

impl UiMessageKind {
    pub(crate) const fn ttl(self) -> Duration {
        match self {
            Self::Info | Self::Success => Duration::from_secs(4),
            Self::Warning => Duration::from_secs(6),
            Self::Error => Duration::from_secs(8),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UiMessage {
    pub text: String,
    pub kind: UiMessageKind,
    pub expires_at: Option<Instant>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TextField {
    text: String,
    cursor: usize,
}

impl TextField {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) const fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn insert(&mut self, value: char) {
        let byte = byte_index(&self.text, self.cursor);
        self.text.insert(byte, value);
        self.cursor += 1;
    }

    pub(crate) fn paste(&mut self, value: &str) {
        for character in value
            .chars()
            .filter(|character| !matches!(character, '\r' | '\n'))
        {
            self.insert(character);
        }
    }

    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = byte_index(&self.text, self.cursor);
        self.cursor -= 1;
        let start = byte_index(&self.text, self.cursor);
        self.text.replace_range(start..end, "");
    }

    pub(crate) fn delete(&mut self) {
        if self.cursor == self.text.chars().count() {
            return;
        }
        let start = byte_index(&self.text, self.cursor);
        let end = byte_index(&self.text, self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    pub(crate) fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(crate) fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    pub(crate) const fn home(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }
}

fn byte_index(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map_or(text.len(), |(index, _)| index)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionChoice {
    Recommended,
    Claude,
    Codex,
    Fake,
}

impl ExecutionChoice {
    pub(crate) const ALL: [Self; 4] = [Self::Recommended, Self::Claude, Self::Codex, Self::Fake];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Recommended => "Recommended",
            Self::Claude => "Claude only",
            Self::Codex => "Codex only",
            Self::Fake => "Fake",
        }
    }

    pub(crate) const fn selection(self) -> ExecutionSelection {
        match self {
            Self::Recommended => ExecutionSelection::Recommended,
            Self::Claude => ExecutionSelection::Uniform(UniformProvider::Claude),
            Self::Codex => ExecutionSelection::Uniform(UniformProvider::Codex),
            Self::Fake => ExecutionSelection::Uniform(UniformProvider::Fake),
        }
    }
}

/// Requested native-runtime effort choice for the new-run composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffortChoice {
    NativeDefault,
    Low,
    Medium,
    High,
}

impl EffortChoice {
    pub(crate) const ALL: [Self; 4] = [Self::NativeDefault, Self::Low, Self::Medium, Self::High];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NativeDefault => "Native default",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    pub(crate) const fn setting(self) -> EffortSetting {
        match self {
            Self::NativeDefault => EffortSetting::NativeDefault,
            Self::Low => EffortSetting::LOW,
            Self::Medium => EffortSetting::MEDIUM,
            Self::High => EffortSetting::HIGH,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewRunForm {
    pub task: TextField,
    pub repository: TextField,
    pub workflow: WorkflowKind,
    pub execution: ExecutionChoice,
    pub effort: EffortChoice,
    pub focus: usize,
}

impl NewRunForm {
    pub(crate) fn new(repository: &std::path::Path) -> Self {
        Self {
            task: TextField::default(),
            repository: TextField::new(repository.display().to_string()),
            workflow: WorkflowKind::Standard,
            execution: ExecutionChoice::Recommended,
            effort: EffortChoice::NativeDefault,
            focus: 0,
        }
    }

    pub(crate) fn next_field(&mut self) {
        self.focus = (self.focus + 1) % 5;
    }

    pub(crate) fn previous_field(&mut self) {
        self.focus = self.focus.checked_sub(1).unwrap_or(4);
    }

    pub(crate) fn cycle_value(&mut self, forward: bool) {
        match self.focus {
            1 => {
                let choices = [
                    WorkflowKind::Fast,
                    WorkflowKind::Standard,
                    WorkflowKind::Deep,
                    WorkflowKind::Review,
                ];
                let index = choices
                    .iter()
                    .position(|choice| *choice == self.workflow)
                    .unwrap_or(1);
                let next = cycle(index, choices.len(), forward);
                self.workflow = choices[next];
            }
            3 => {
                let index = ExecutionChoice::ALL
                    .iter()
                    .position(|choice| *choice == self.execution)
                    .unwrap_or(0);
                self.execution =
                    ExecutionChoice::ALL[cycle(index, ExecutionChoice::ALL.len(), forward)];
            }
            4 => {
                let index = EffortChoice::ALL
                    .iter()
                    .position(|choice| *choice == self.effort)
                    .unwrap_or(0);
                self.effort = EffortChoice::ALL[cycle(index, EffortChoice::ALL.len(), forward)];
            }
            _ => {}
        }
    }

    pub(crate) fn active_text_mut(&mut self) -> Option<&mut TextField> {
        match self.focus {
            0 => Some(&mut self.task),
            2 => Some(&mut self.repository),
            _ => None,
        }
    }
}

const fn cycle(index: usize, length: usize, forward: bool) -> usize {
    if forward {
        (index + 1) % length
    } else if index == 0 {
        length - 1
    } else {
        index - 1
    }
}

#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent presentation toggles, each owned by one key; grouping them would not simplify any caller"
)]
pub(crate) struct TuiState {
    pub screen: Screen,
    pub overlay: Option<Overlay>,
    pub runs: Vec<RunListItem>,
    pub selected_run: Option<RunId>,
    pub selected_run_index: usize,
    pub details: Option<RunDetails>,
    pub selected_stage: Option<StageId>,
    pub selected_stage_index: usize,
    pub artifact: Option<ArtifactView>,
    pub artifact_raw: bool,
    /// Operational view by default; `i` reveals the diagnostic rows.
    pub technical: bool,
    pub evidence: Option<StageExecutionEvidence>,
    pub stages_with_artifacts: HashSet<StageId>,
    pub logs: Option<ProcessLogView>,
    pub diff: Option<RunDiffPreview>,
    pub scroll: u16,
    pub attention_index: usize,
    pub attention_response: TextField,
    pub new_run: NewRunForm,
    pub worker_busy: Option<String>,
    pub message: Option<UiMessage>,
    pub quiescent: Option<QuiescentState>,
    /// Newer official release, once a background check has concluded. Absent
    /// while the check is in flight, disabled, or inconclusive.
    pub update: Option<crate::update::UpdateInfo>,
    /// How this executable was installed, which decides whether Polycode may
    /// offer to install the update itself.
    pub update_install: Option<crate::update::InstallSource>,
    /// Set once the user answers the prompt; it never reopens in this process.
    pub update_dismissed: bool,
    /// Which answer the update prompt has highlighted. Installing is the
    /// default only because the prompt appears solely when installing is
    /// possible and nothing more urgent is happening.
    pub update_install_selected: bool,
    pub quit: bool,
}

impl TuiState {
    pub(crate) fn new(repository: &Path) -> Self {
        Self {
            screen: Screen::Runs,
            overlay: None,
            runs: Vec::new(),
            selected_run: None,
            selected_run_index: 0,
            details: None,
            selected_stage: None,
            selected_stage_index: 0,
            artifact: None,
            artifact_raw: false,
            technical: false,
            evidence: None,
            stages_with_artifacts: HashSet::new(),
            logs: None,
            diff: None,
            scroll: 0,
            attention_index: 0,
            attention_response: TextField::default(),
            new_run: NewRunForm::new(repository),
            worker_busy: None,
            message: None,
            quiescent: None,
            update: None,
            update_install: None,
            update_dismissed: false,
            update_install_selected: true,
            quit: false,
        }
    }

    pub(crate) fn replace_runs(&mut self, runs: Vec<RunListItem>) {
        let previous_index = self.selected_run_index;
        let previous_id = self.selected_run;
        self.runs = runs;
        if self.runs.is_empty() {
            self.selected_run = None;
            self.selected_run_index = 0;
            self.details = None;
            return;
        }
        self.selected_run_index = previous_id
            .and_then(|id| self.runs.iter().position(|run| run.id == id))
            .unwrap_or_else(|| previous_index.min(self.runs.len() - 1));
        self.selected_run = Some(self.runs[self.selected_run_index].id);
    }

    pub(crate) fn replace_details(&mut self, details: RunDetails) {
        let previous_id = self.selected_stage.as_ref();
        self.selected_stage_index = previous_id
            .and_then(|id| details.stages.iter().position(|stage| &stage.id == id))
            .unwrap_or_else(|| {
                self.selected_stage_index
                    .min(details.stages.len().saturating_sub(1))
            });
        self.selected_stage = details
            .stages
            .get(self.selected_stage_index)
            .map(|stage| stage.id.clone());
        self.details = Some(details);
        self.attention_index = self.details.as_ref().map_or(0, |details| {
            self.attention_index
                .min(details.attention.len().saturating_sub(1))
        });
    }

    pub(crate) fn move_run(&mut self, forward: bool) {
        if self.runs.is_empty() {
            return;
        }
        self.selected_run_index = if forward {
            (self.selected_run_index + 1).min(self.runs.len() - 1)
        } else {
            self.selected_run_index.saturating_sub(1)
        };
        self.selected_run = Some(self.runs[self.selected_run_index].id);
    }

    pub(crate) fn move_stage(&mut self, forward: bool) {
        let length = self
            .details
            .as_ref()
            .map_or(0, |details| details.stages.len());
        if length == 0 {
            return;
        }
        self.selected_stage_index = if forward {
            (self.selected_stage_index + 1).min(length - 1)
        } else {
            self.selected_stage_index.saturating_sub(1)
        };
        self.selected_stage = self
            .details
            .as_ref()
            .and_then(|details| details.stages.get(self.selected_stage_index))
            .map(|stage| stage.id.clone());
    }

    pub(crate) fn selected_attention_id(&self) -> Option<AttentionRequestId> {
        self.details
            .as_ref()
            .and_then(|details| details.attention.get(self.attention_index))
            .map(|attention| attention.id)
    }

    /// Whether apply/discard review actions are offered for the selected run.
    ///
    /// Mirrors the workspace invariant (`WorkspaceManager::apply`): only a
    /// `Completed` run whose workflow produced a branch workspace can be
    /// applied. The application layer stays the final guard; this only stops
    /// the TUI advertising and dispatching an action the domain will reject.
    /// Whether the update prompt may open right now.
    ///
    /// Software update is the lowest-priority interaction in the interface: it
    /// waits for the Runs screen, never covers another overlay, and never
    /// competes with a run that needs the user or an action in flight.
    pub(crate) fn update_prompt_is_due(&self) -> bool {
        self.update.is_some()
            && !self.update_dismissed
            && self.overlay.is_none()
            && self.screen == Screen::Runs
            && self.worker_busy.is_none()
            && !self.runs.iter().any(|run| {
                matches!(
                    run.status,
                    crate::domain::RunStatus::NeedsUser | crate::domain::RunStatus::Failed
                )
            })
    }

    /// Whether Stop is offered for the selected run.
    ///
    /// Mirrors the domain's interrupt transition, which is only valid from a
    /// run that is actually executing or waiting on the user. This only stops
    /// the TUI advertising an action the domain would refuse; the application
    /// layer stays the guard.
    pub(crate) fn run_is_stoppable(&self) -> bool {
        self.details.as_ref().is_some_and(|details| {
            matches!(
                details.status,
                crate::domain::RunStatus::Running | crate::domain::RunStatus::NeedsUser
            )
        })
    }

    /// Whether Polycode may install the pending update itself.
    pub(crate) fn update_is_installable(&self) -> bool {
        self.update_install
            .is_some_and(|source| source.strategy().is_automatic())
    }

    pub(crate) fn run_is_applyable(&self) -> bool {
        self.details.as_ref().is_some_and(|details| {
            details.status == crate::domain::RunStatus::Completed
                && details.workflow != WorkflowKind::Review
        })
    }

    pub(crate) fn notify(&mut self, kind: UiMessageKind, text: impl Into<String>) {
        self.notify_at(kind, text, Instant::now());
    }

    pub(crate) fn notify_at(&mut self, kind: UiMessageKind, text: impl Into<String>, now: Instant) {
        self.message = Some(UiMessage {
            text: text.into(),
            kind,
            expires_at: now.checked_add(kind.ttl()),
        });
    }

    pub(crate) fn set_error(&mut self, error: impl Into<String>) {
        self.notify(UiMessageKind::Error, error);
    }

    pub(crate) fn set_message(&mut self, message: impl Into<String>) {
        self.notify(UiMessageKind::Success, message);
    }

    pub(crate) fn clear_expired_message(&mut self, now: Instant) {
        if self
            .message
            .as_ref()
            .and_then(|message| message.expires_at)
            .is_some_and(|expires_at| now >= expires_at)
        {
            self.message = None;
        }
    }

    pub(crate) fn dismiss_message(&mut self) {
        self.message = None;
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::domain::RunStatus;

    fn run(id: u128, task: &str) -> RunListItem {
        RunListItem {
            id: RunId::from_u128(id),
            workflow: WorkflowKind::Standard,
            status: RunStatus::Running,
            task_summary: task.to_owned(),
            repository: None,
            updated_at: Utc
                .with_ymd_and_hms(2026, 8, 17, 12, 0, 0)
                .single()
                .unwrap(),
        }
    }

    #[test]
    fn selection_follows_run_id_across_reorder_and_uses_nearest_fallback() {
        let mut state = TuiState::new(Path::new("/repo"));
        state.replace_runs(vec![run(1, "one"), run(2, "two"), run(3, "three")]);
        state.move_run(true);
        assert_eq!(state.selected_run, Some(RunId::from_u128(2)));

        state.replace_runs(vec![run(3, "three"), run(2, "two"), run(1, "one")]);
        assert_eq!(state.selected_run_index, 1);
        assert_eq!(state.selected_run, Some(RunId::from_u128(2)));

        state.replace_runs(vec![run(3, "three"), run(1, "one")]);
        assert_eq!(state.selected_run_index, 1);
        assert_eq!(state.selected_run, Some(RunId::from_u128(1)));
    }

    #[test]
    fn text_field_edits_unicode_by_character() {
        let mut field = TextField::new("caffè");
        field.left();
        field.backspace();
        field.insert('é');
        assert_eq!(field.text(), "caféè");
        field.home();
        field.delete();
        assert_eq!(field.text(), "aféè");
    }

    #[test]
    fn message_kinds_have_distinct_escalating_lifetimes() {
        assert_eq!(UiMessageKind::Info.ttl(), UiMessageKind::Success.ttl());
        assert!(UiMessageKind::Warning.ttl() > UiMessageKind::Info.ttl());
        assert!(UiMessageKind::Error.ttl() > UiMessageKind::Warning.ttl());
    }

    #[test]
    fn messages_expire_deterministically_without_sleeping() {
        let mut state = TuiState::new(Path::new("/repo"));
        let now = Instant::now();
        state.notify_at(UiMessageKind::Info, "saved", now);
        state.clear_expired_message(now + Duration::from_secs(3));
        assert!(state.message.is_some(), "info survives before its TTL");
        state.clear_expired_message(now + Duration::from_secs(4));
        assert!(state.message.is_none(), "info expires at its TTL");

        state.notify_at(UiMessageKind::Error, "boom", now);
        state.clear_expired_message(now + Duration::from_secs(7));
        assert!(state.message.is_some(), "errors linger longer than info");
        state.clear_expired_message(now + Duration::from_secs(8));
        assert!(state.message.is_none(), "errors still expire");
    }

    #[test]
    fn manual_dismiss_clears_message_immediately() {
        let mut state = TuiState::new(Path::new("/repo"));
        state.set_error("failure");
        assert_eq!(state.message.as_ref().unwrap().kind, UiMessageKind::Error);
        state.dismiss_message();
        assert!(state.message.is_none());
    }

    #[test]
    fn composer_defaults_to_standard_recommended() {
        let form = NewRunForm::new(std::path::Path::new("/repo"));
        assert_eq!(form.workflow, WorkflowKind::Standard);
        assert_eq!(form.execution, ExecutionChoice::Recommended);
    }
}
