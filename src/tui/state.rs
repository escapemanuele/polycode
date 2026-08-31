use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use super::mascot::{self, MascotState};
use super::motion::{self, MotionFrame};
use super::worker::{ActionKind, WorkerCommand};
use crate::app::{
    ArtifactSummary, ArtifactView, ExecutionSelection, ProcessLogView, QuiescentState, RunDetails,
    RunDiffPreview, RunListItem, StageExecutionEvidence, UniformProvider,
};
use crate::domain::{AttentionRequestId, EffortSetting, RunId, StageId, WorkflowKind};

/// How long POD reacts to a change before settling into the new state. Long
/// enough to be seen, short enough that it is over before it is in the way.
const REACTION: Duration = Duration::from_millis(600);

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

/// How many agents this interface will have working at one time.
///
/// Each one holds a managed worktree, a terminal session and a provider
/// process for as long as it runs, so the ceiling is about the machine, not
/// about the domain: enough to keep several pieces of work moving at once,
/// short of handing the whole machine over to a fleet nobody is watching. The
/// command line is unaffected — a separate process has its own ceiling.
pub(crate) const CONCURRENT_AGENTS: usize = 4;

/// Why a dispatched action was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Another action is already working on the same run.
    RunIsHeld(ActionKind),
    /// This interface already has as many agents at work as it will run.
    AtCapacity,
}

impl Refusal {
    /// What the user is told, in terms of what they can do about it.
    pub(crate) fn reason(self) -> String {
        match self {
            Self::RunIsHeld(holder) => {
                format!("Already {} — wait for it to finish", holder.label())
            }
            Self::AtCapacity => format!(
                "{CONCURRENT_AGENTS} agents are already working — wait for one to finish, or stop one"
            ),
        }
    }
}

/// One action this session dispatched and has not yet heard back about.
///
/// The run it targets is what makes it a claim rather than a global lock: an
/// action holds the run it is working on and nothing else. A start holds no
/// run at all, because the run it creates does not exist until it reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InFlightAction {
    pub action: ActionKind,
    pub run_id: Option<RunId>,
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

/// One artifact's opening line, and the artifact it was read from.
///
/// The source identity is what keeps a half-second refresh cheap: the file is
/// reread only when the stage, its attempt, or its size changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StageHeadline {
    pub stage_id: StageId,
    pub attempt: u32,
    pub content_size: u64,
    /// The quoted line, or `None` for an artifact that carries no prose to
    /// quote — remembered rather than forgotten, so an artifact with nothing
    /// to say is not reread twice a second for as long as it stays selected.
    pub text: Option<String>,
    /// Whether the artifact stated this as its own bottom line, or whether it
    /// is the artifact's opening paragraph standing in for one.
    pub contracted: bool,
}

impl StageHeadline {
    /// Whether this quote still belongs to the artifact in front of the
    /// operator. A new attempt, a rewritten file, or a different stage all
    /// mean the panel is holding someone else's words.
    pub(crate) fn describes(&self, stage_id: &StageId, artifact: &ArtifactSummary) -> bool {
        self.stage_id == *stage_id
            && self.attempt == artifact.attempt
            && self.content_size == artifact.content_size
    }
}

/// Whether a run's workflow reaches a verdict a fix could answer.
fn has_decision(details: &RunDetails) -> bool {
    details
        .stages
        .iter()
        .any(|stage| stage.kind == crate::domain::StageKind::Decision)
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
    /// What the selected stage's artifact says about itself, in one line.
    /// Absent until a stage with a verified artifact is selected.
    pub headline: Option<StageHeadline>,
    pub logs: Option<ProcessLogView>,
    pub diff: Option<RunDiffPreview>,
    pub scroll: u16,
    pub attention_index: usize,
    pub attention_response: TextField,
    pub new_run: NewRunForm,
    /// Every dispatched action still waiting on its outcome. A list rather
    /// than a single slot: this interface can be working on several runs at
    /// once, and one run's action is no reason to refuse another's.
    pub in_flight: Vec<InFlightAction>,
    /// Runs the operator has already decided to send back for a fix, chosen
    /// while the run was still working. Held here rather than in the store
    /// because nothing has happened yet: this is an intention about a run, not
    /// a fact about one, and the interface is the only thing that can act on
    /// it. A run that finishes while Polycode is closed simply waits to be
    /// asked again.
    pub fix_when_finished: HashSet<RunId>,
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
    /// The state POD last showed, and what it was showing it for. Keyed by
    /// run and stage so a change in the world can be told apart from the user
    /// looking somewhere else: arrowing onto a failed stage is not an event.
    mascot_seen: Option<(RunId, Option<StageId>, MascotState)>,
    /// When the current reaction ends. Ephemeral by construction — it is a
    /// deadline, never a durable state, and nothing persists it.
    reaction_until: Option<Instant>,
    /// Whether a reaction is playing in the frame being drawn. Owned by the
    /// loop for the same reason as `motion_phase`.
    pub reacting: bool,
    /// Which blink frame the loop is on. Owned by the loop rather than
    /// read from the clock here, so a drawn frame stays a pure function of
    /// state and no test has to wait for one.
    pub motion_phase: u8,
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
            headline: None,
            logs: None,
            diff: None,
            scroll: 0,
            attention_index: 0,
            attention_response: TextField::default(),
            new_run: NewRunForm::new(repository),
            in_flight: Vec::new(),
            message: None,
            quiescent: None,
            update: None,
            update_install: None,
            update_dismissed: false,
            update_install_selected: true,
            fix_when_finished: HashSet::new(),
            mascot_seen: None,
            reaction_until: None,
            reacting: false,
            motion_phase: 0,
            quit: false,
        }
    }

    /// What this frame may do: the surface's ceiling, lowered by whatever
    /// the user asked for. Nothing that draws gets to decide this itself.
    pub(crate) fn motion_frame(&self) -> MotionFrame {
        MotionFrame::new(
            motion::allowance(self.screen, self.overlay, motion::motion_setting()),
            self.motion_phase,
            self.reacting,
        )
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
        self.note_mascot(Instant::now());
    }

    /// Starts a reaction when the state POD is showing changed underneath it.
    ///
    /// The identity is part of the comparison on purpose: POD reacts to the
    /// world moving, never to the user moving. Selecting a different stage
    /// shows a different face, and that is not something happening.
    fn note_mascot(&mut self, now: Instant) {
        let Some(details) = self.details.as_ref() else {
            return;
        };
        let seen = (
            details.id,
            self.selected_stage.clone(),
            mascot::mascot_state(
                Some(details.status),
                details
                    .stages
                    .get(self.selected_stage_index)
                    .map(|stage| stage.status),
            ),
        );
        if let Some(previous) = self.mascot_seen.as_ref()
            && previous.0 == seen.0
            && previous.1 == seen.1
            && previous.2 != seen.2
        {
            self.reaction_until = now.checked_add(REACTION);
        }
        self.mascot_seen = Some(seen);
    }

    /// Ends a reaction that has run its course. Called once per frame, next
    /// to the message expiry, so a reaction can never outlive its window.
    pub(crate) fn settle_reaction(&mut self, now: Instant) {
        self.reacting = self.reaction_until.is_some_and(|until| now < until);
        if !self.reacting {
            self.reaction_until = None;
        }
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

    /// Records that an action has been dispatched and is now working.
    pub(crate) fn begin_action(&mut self, action: ActionKind, run_id: Option<RunId>) {
        self.in_flight.push(InFlightAction { action, run_id });
    }

    /// Drops one finished action from the in-flight set.
    ///
    /// Exactly one entry, never every match: two stops on the same run are two
    /// separate pieces of work, and the second is still running when the first
    /// reports back.
    pub(crate) fn settle_action(&mut self, action: ActionKind, run_id: Option<RunId>) {
        if let Some(index) = self
            .in_flight
            .iter()
            .position(|entry| entry.action == action && entry.run_id == run_id)
        {
            self.in_flight.remove(index);
        }
    }

    /// Why `command` cannot start right now, if it cannot.
    ///
    /// Actions are held against the run they target, so work on one run never
    /// refuses work on another. Stop is the deliberate exception to every rule
    /// here: it is the only way out of a run that has stopped making progress,
    /// so it is always allowed, including against the very action it is
    /// interrupting. The application layer is built for exactly that, retrying
    /// the stop until it wins the concurrency race with whoever is still
    /// driving the run.
    pub(crate) fn action_refusal(&self, command: &WorkerCommand) -> Option<Refusal> {
        let kind = command.kind();
        if kind == ActionKind::Stop {
            return None;
        }
        if let Some(target) = command.run_id()
            && let Some(holder) = self
                .in_flight
                .iter()
                .find(|entry| entry.run_id == Some(target))
        {
            return Some(Refusal::RunIsHeld(holder.action));
        }
        if kind.drives_a_provider() && self.agents_at_work() >= CONCURRENT_AGENTS {
            return Some(Refusal::AtCapacity);
        }
        None
    }

    /// How many in-flight actions currently have an agent working.
    pub(crate) fn agents_at_work(&self) -> usize {
        self.in_flight
            .iter()
            .filter(|entry| entry.action.drives_a_provider())
            .count()
    }

    /// What the header says about work in flight, if anything is.
    ///
    /// One action names itself; several are counted, because listing them
    /// would crowd out the run identity the header exists to show.
    pub(crate) fn busy_label(&self) -> Option<String> {
        match self.in_flight.as_slice() {
            [] => None,
            [only] => Some(only.action.label().to_owned()),
            many => Some(format!("{} actions in flight", many.len())),
        }
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
            && self.in_flight.is_empty()
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
                && details.workspace_mode == Some(crate::workspace::WorkspaceMode::Branch)
        })
    }

    /// Whether this run can be sent back to fix its own result.
    ///
    /// A completed run and a decision for the fix to answer. A workflow that
    /// never decides has no verdict to remediate, so offering the action there
    /// would only produce a refusal.
    ///
    /// Deliberately not apply's answer any more. A review reaches a verdict
    /// like any other workflow, and being unable to *transfer* changes is not
    /// the same as having nothing to fix — asking is what gives its workspace
    /// the branch apply will later want.
    pub(crate) fn run_can_be_fixed(&self) -> bool {
        self.details.as_ref().is_some_and(|details| {
            details.status == crate::domain::RunStatus::Completed && has_decision(details)
        })
    }

    /// Whether a fix can be *booked* on the selected run: it is still working,
    /// and its workflow will reach a decision for that fix to answer.
    ///
    /// Booking exists because the operator already knows what they want while
    /// the run is still going, and the alternative is watching for it to end.
    pub(crate) fn run_can_book_a_fix(&self) -> bool {
        self.details.as_ref().is_some_and(|details| {
            matches!(
                details.status,
                crate::domain::RunStatus::Running | crate::domain::RunStatus::NeedsUser
            ) && has_decision(details)
        })
    }

    /// Whether the selected run is already booked for a fix.
    pub(crate) fn fix_is_booked(&self) -> bool {
        self.selected_run
            .is_some_and(|run_id| self.fix_when_finished.contains(&run_id))
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

    /// The seam the renderer actually goes through. Rendering a reading
    /// screen cannot prove this on its own — none of them draws anything
    /// that moves today, so such a test would pass however the ceiling was
    /// wired. This asserts the frame those screens hand out, which is the
    /// thing a future moving element would ask.
    #[test]
    fn the_frame_a_reading_surface_hands_out_never_moves() {
        for screen in [Screen::Artifact, Screen::Logs, Screen::Diff, Screen::NewRun] {
            let mut state = TuiState::new(std::path::Path::new("/repo"));
            state.screen = screen;
            state.motion_phase = 1;
            assert_eq!(
                state.motion_frame().active_phase(),
                0,
                "{screen:?} handed the renderer a moving frame"
            );
        }
    }

    #[test]
    fn an_open_overlay_hands_out_a_still_frame_over_any_screen() {
        for overlay in [
            Overlay::Help,
            Overlay::Attention,
            Overlay::ApplyConfirm,
            Overlay::DiscardConfirm,
            Overlay::Update,
        ] {
            let mut state = TuiState::new(std::path::Path::new("/repo"));
            state.screen = Screen::RunDetail;
            state.overlay = Some(overlay);
            state.motion_phase = 1;
            assert_eq!(
                state.motion_frame().active_phase(),
                0,
                "POD kept breathing behind {overlay:?}"
            );
        }
    }

    /// And the operating surface still moves, so the two tests above are
    /// about the ceiling rather than about motion never working at all.
    #[test]
    fn an_operating_surface_hands_out_the_moving_frame() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.screen = Screen::RunDetail;
        state.motion_phase = 1;
        assert_eq!(state.motion_frame().active_phase(), 1);
    }

    /// The panel refreshes twice a second and an artifact is up to a megabyte
    /// of Markdown that has to be read and rehashed to be trusted. So the
    /// quote is kept until the artifact behind it is genuinely a different
    /// one — and a retry, which rewrites the same stage, counts as different.
    #[test]
    fn a_quote_is_kept_for_its_own_artifact_and_dropped_for_any_other() {
        let stage_id = StageId::new("quality_review").unwrap();
        let artifact = ArtifactSummary {
            stage_id: stage_id.clone(),
            kind: crate::domain::ArtifactKind::CodeQualityReview,
            status: crate::domain::ArtifactStatus::Complete,
            attempt: 1,
            provider: Some("claude".to_owned()),
            model: None,
            content_size: 4_096,
            created_at: Utc.timestamp_opt(0, 0).single().unwrap(),
        };
        let headline = StageHeadline {
            stage_id: stage_id.clone(),
            attempt: 1,
            content_size: 4_096,
            text: Some("Two screens, one mechanism, two endings.".to_owned()),
            contracted: true,
        };

        assert!(headline.describes(&stage_id, &artifact));

        let retried = ArtifactSummary {
            attempt: 2,
            ..artifact.clone()
        };
        assert!(
            !headline.describes(&stage_id, &retried),
            "a retry rewrote it"
        );

        let rewritten = ArtifactSummary {
            content_size: 5_000,
            ..artifact.clone()
        };
        assert!(!headline.describes(&stage_id, &rewritten));

        let elsewhere = StageId::new("spec_review").unwrap();
        assert!(
            !headline.describes(&elsewhere, &artifact),
            "another stage's artifact never inherits this quote"
        );
    }
}
