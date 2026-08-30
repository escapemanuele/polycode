//! POD — the Polycode Operator Droid.
//!
//! One high-definition pixel companion with a single stable silhouette,
//! rendered compositionally: fixed shell + state expression (antenna, eyes,
//! mouth) + optional activity accent + one compact label. State comes from
//! canonical `RunStatus`/`StageStatus` and activity from the typed `Role`;
//! nothing is inferred from prose. Every variant occupies exactly
//! `MASCOT_WIDTH` × `MASCOT_HEIGHT` cells so surrounding layout never
//! shifts. Art uses only ASCII plus the single-cell block elements
//! █ ▄ ▀ ▌ ▐ (see `GLYPH_WHITELIST`); no emoji, no ambiguous-width glyphs.
//!
//! POD may breathe, but only where the surface permits it and only while the
//! domain considers the work active: the caller hands in a [`MotionFrame`],
//! and a still frame draws exactly the art POD drew before motion existed.
//! The breathing reinforces a state written elsewhere in words and glyphs —
//! it is never the evidence for it.

use super::motion::MotionFrame;
use super::theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::domain::{Role, RunStatus, StageStatus};

/// Every variant renders exactly this many terminal cells per art row.
pub(crate) const MASCOT_WIDTH: u16 = 16;

/// Six art rows plus one label row, for every variant.
pub(crate) const MASCOT_HEIGHT: u16 = 7;

/// The only non-ASCII glyphs POD may use: single-cell block elements with
/// predictable width in the terminals Ratatui targets. Enforced by the
/// footprint test, which rejects anything outside ASCII plus this set.
#[cfg(test)]
const GLYPH_WHITELIST: [char; 5] = ['█', '▄', '▀', '▌', '▐'];

/// Overall mood, projected from canonical run/stage state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MascotState {
    Idle,
    Running,
    Waiting,
    NeedsUser,
    Completed,
    Failed,
}

/// Current responsibility, projected from the typed stage role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MascotActivity {
    Architecture,
    Implementation,
    QualityReview,
    SpecReview,
    Decision,
}

/// Maps canonical state to POD's mood. The selected stage wins when present;
/// the run status is the fallback. A pending/ready stage inside a running
/// run reads as Waiting (its turn has not come), otherwise Idle.
pub(crate) const fn mascot_state(
    run: Option<RunStatus>,
    stage: Option<StageStatus>,
) -> MascotState {
    match stage {
        Some(StageStatus::Running) => MascotState::Running,
        Some(StageStatus::NeedsUser) => MascotState::NeedsUser,
        Some(StageStatus::Completed) => MascotState::Completed,
        Some(StageStatus::Failed) => MascotState::Failed,
        Some(StageStatus::Paused | StageStatus::Interrupted) => MascotState::Waiting,
        Some(StageStatus::Pending | StageStatus::Ready) => match run {
            Some(RunStatus::Running) => MascotState::Waiting,
            _ => MascotState::Idle,
        },
        Some(StageStatus::Skipped) | None => match run {
            Some(RunStatus::Running) => MascotState::Running,
            Some(RunStatus::NeedsUser) => MascotState::NeedsUser,
            Some(RunStatus::Failed) => MascotState::Failed,
            Some(RunStatus::Completed | RunStatus::Applied) => MascotState::Completed,
            Some(RunStatus::Paused | RunStatus::Interrupted) => MascotState::Waiting,
            Some(
                RunStatus::Created | RunStatus::Preparing | RunStatus::Ready | RunStatus::Discarded,
            )
            | None => MascotState::Idle,
        },
    }
}

/// Maps the semantic responsibility to POD's activity. Researcher maps to
/// Architecture (both are planning/thinking work; no separate taxonomy for
/// one role) and legacy Reviewer to `QualityReview`.
pub(crate) const fn mascot_activity(role: Role) -> MascotActivity {
    match role {
        Role::Architect | Role::Researcher => MascotActivity::Architecture,
        Role::Implementer => MascotActivity::Implementation,
        Role::CodeQualityReviewer | Role::Reviewer => MascotActivity::QualityReview,
        Role::SpecReviewer => MascotActivity::SpecReview,
        Role::EngineeringLead => MascotActivity::Decision,
    }
}

/// Expression slots for one state at rest: antenna core (2 chars), left eye,
/// right eye (2 chars each), mouth (4 chars).
const fn resting_expression(
    state: MascotState,
) -> (&'static str, &'static str, &'static str, &'static str) {
    match state {
        MascotState::Idle => ("██", "██", "██", "▀▄▄▀"),
        MascotState::Running => ("██", "▀▀", "▀▀", " ▄▄ "),
        MascotState::Waiting => ("zz", "▄▄", "▄▄", " .. "),
        MascotState::NeedsUser => ("!!", "▐█", "█▌", " ██ "),
        MascotState::Completed => ("██", "▀▀", "▀▀", "▀▄▄▀"),
        MascotState::Failed => ("xx", "xx", "xx", "▄▀▀▄"),
    }
}

/// The expression POD wears this frame.
///
/// The repeating motion, and only that: while work is running POD blinks,
/// and the blink borrows the eye `Waiting` already wears one row lower, so
/// motion introduces no glyph the whitelist has not accepted. Every other
/// state is the same in every phase — a face that moves while nothing is
/// happening would be claiming something untrue.
const fn expression(
    state: MascotState,
    phase: u8,
    reacting: bool,
) -> (&'static str, &'static str, &'static str, &'static str) {
    let (antenna, eye_left, eye_right, mouth) = resting_expression(state);
    if reacting {
        // A reaction outranks a blink: something just happened, and POD is
        // looking at it rather than breathing. Wide eyes, except where the
        // state already wears them — then they narrow instead, so a reaction
        // is always visible without inventing a third eye.
        let (_, wide, _, _) = resting_expression(MascotState::Idle);
        let (_, narrow, _, _) = resting_expression(MascotState::Running);
        let eye = if str_eq(eye_left, wide) { narrow } else { wide };
        return (antenna, eye, eye, mouth);
    }
    if matches!(state, MascotState::Running) && phase == 1 {
        let (_, blink_left, blink_right, _) = resting_expression(MascotState::Waiting);
        return (antenna, blink_left, blink_right, mouth);
    }
    (antenna, eye_left, eye_right, mouth)
}

/// `str::eq` is not const, and this comparison has to happen in one.
const fn str_eq(one: &str, other: &str) -> bool {
    let (one, other) = (one.as_bytes(), other.as_bytes());
    if one.len() != other.len() {
        return false;
    }
    let mut index = 0;
    while index < one.len() {
        if one[index] != other[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// State label; for Running the activity label takes over (the state is
/// already obvious from color and the surrounding UI).
const fn state_label(state: MascotState) -> &'static str {
    match state {
        MascotState::Idle => "READY",
        MascotState::Running => "RUNNING",
        MascotState::Waiting => "WAITING",
        MascotState::NeedsUser => "NEEDS YOU",
        MascotState::Completed => "DONE",
        MascotState::Failed => "FAILED",
    }
}

pub(crate) const fn activity_label(activity: MascotActivity) -> &'static str {
    match activity {
        MascotActivity::Architecture => "THINKING",
        MascotActivity::Implementation => "BUILDING",
        MascotActivity::QualityReview => "INSPECTING",
        MascotActivity::SpecReview => "CHECKING",
        MascotActivity::Decision => "DECIDING",
    }
}

/// 3-char tool held at the base; shown only while Running (state overrides
/// activity everywhere else).
const fn activity_accent(activity: MascotActivity) -> &'static str {
    match activity {
        MascotActivity::Architecture => "[#]",
        MascotActivity::Implementation => "</>",
        MascotActivity::QualityReview => "(o)",
        MascotActivity::SpecReview => "[=]",
        MascotActivity::Decision => "[!]",
    }
}

fn state_style(state: MascotState) -> Style {
    match state {
        MascotState::Idle | MascotState::Running => Style::new().fg(theme::accent()),
        MascotState::Waiting => Style::new().fg(theme::muted_color()),
        MascotState::NeedsUser => Style::new().fg(theme::attention()),
        MascotState::Completed => Style::new().fg(theme::success()),
        MascotState::Failed => Style::new().fg(theme::danger()),
    }
}

fn accent_style(activity: MascotActivity) -> Style {
    match activity {
        MascotActivity::Architecture => Style::new().fg(theme::structure()),
        MascotActivity::Implementation
        | MascotActivity::QualityReview
        | MascotActivity::SpecReview
        | MascotActivity::Decision => Style::new().fg(theme::muted_color()),
    }
}

/// The label POD wears: state wins outright; only Running yields the line
/// to the current activity.
fn label(state: MascotState, activity: Option<MascotActivity>) -> &'static str {
    match (state, activity) {
        (MascotState::Running, Some(activity)) => activity_label(activity),
        _ => state_label(state),
    }
}

/// Renders POD: stable shell + state expression + optional accent + label.
/// Always `MASCOT_HEIGHT` rows; art rows are exactly `MASCOT_WIDTH` cells,
/// in every phase the frame can hand out.
pub(crate) fn mascot_lines(
    state: MascotState,
    activity: Option<MascotActivity>,
    motion: MotionFrame,
) -> Vec<Line<'static>> {
    let (antenna, eye_left, eye_right, mouth) =
        expression(state, motion.active_phase(), motion.is_reacting());
    let accent = match (state, activity) {
        (MascotState::Running, Some(activity)) => Some(activity),
        _ => None,
    };
    let style = state_style(state);
    let center = ratatui::layout::Alignment::Center;
    let art = [
        format!("      ▄{antenna}▄      "),
        "   ▄██████████▄ ".to_owned(),
        format!("  ██  {eye_left}  {eye_right}  ██"),
        format!("  ██   {mouth}   ██"),
        "   ▀██████████▀ ".to_owned(),
    ];
    let mut lines: Vec<Line<'static>> = art
        .into_iter()
        .map(|row| Line::from(Span::styled(row, style)).alignment(center))
        .collect();
    // Feet row carries the tool accent inside the fixed footprint.
    lines.push(
        Line::from(vec![
            Span::styled("    ▐██  ██▌ ", style),
            accent.map_or_else(
                || Span::styled("   ", style),
                |activity| Span::styled(activity_accent(activity), accent_style(activity)),
            ),
        ])
        .alignment(center),
    );
    lines.push(
        Line::from(Span::styled(
            label(state, activity),
            state_style(state).add_modifier(Modifier::BOLD),
        ))
        .alignment(center),
    );
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::motion::MotionAllowance;

    const ALL_STATES: [MascotState; 6] = [
        MascotState::Idle,
        MascotState::Running,
        MascotState::Waiting,
        MascotState::NeedsUser,
        MascotState::Completed,
        MascotState::Failed,
    ];
    const ALL_ACTIVITIES: [MascotActivity; 5] = [
        MascotActivity::Architecture,
        MascotActivity::Implementation,
        MascotActivity::QualityReview,
        MascotActivity::SpecReview,
        MascotActivity::Decision,
    ];

    /// Every phase a frame can hand out.
    const ALL_PHASES: [MotionFrame; 3] = [
        MotionFrame::still(),
        MotionFrame::new(MotionAllowance::ActiveStateAndTransitions, 1, false),
        MotionFrame::new(MotionAllowance::ActiveStateAndTransitions, 0, true),
    ];

    fn rows(state: MascotState, activity: Option<MascotActivity>) -> Vec<String> {
        rows_in(state, activity, MotionFrame::still())
    }

    fn rows_in(
        state: MascotState,
        activity: Option<MascotActivity>,
        motion: MotionFrame,
    ) -> Vec<String> {
        mascot_lines(state, activity, motion)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// Cell width under the whitelist: every permitted glyph is one cell,
    /// so char count equals rendered width. Panics on any glyph outside
    /// ASCII + the block whitelist.
    fn cell_width(row: &str) -> usize {
        row.chars()
            .map(|character| {
                assert!(
                    character.is_ascii() || GLYPH_WHITELIST.contains(&character),
                    "glyph {character:?} is outside the safe single-cell set"
                );
                1
            })
            .sum()
    }

    #[test]
    fn every_variant_keeps_the_same_footprint_and_safe_glyphs() {
        for state in ALL_STATES {
            for activity in std::iter::once(None).chain(ALL_ACTIVITIES.map(Some)) {
                for motion in ALL_PHASES {
                    let rows = rows_in(state, activity, motion);
                    assert_eq!(rows.len(), MASCOT_HEIGHT as usize);
                    for row in &rows[..6] {
                        assert_eq!(
                            cell_width(row),
                            MASCOT_WIDTH as usize,
                            "unstable footprint for {state:?}/{activity:?}/{motion:?}: {row:?}"
                        );
                    }
                    assert!(cell_width(&rows[6]) <= MASCOT_WIDTH as usize);
                }
            }
        }
    }

    #[test]
    fn state_mapping_prefers_selected_stage_over_run() {
        assert_eq!(mascot_state(None, None), MascotState::Idle);
        assert_eq!(
            mascot_state(Some(RunStatus::Completed), Some(StageStatus::Running)),
            MascotState::Running
        );
        assert_eq!(
            mascot_state(Some(RunStatus::Running), Some(StageStatus::NeedsUser)),
            MascotState::NeedsUser
        );
        assert_eq!(
            mascot_state(Some(RunStatus::Running), Some(StageStatus::Completed)),
            MascotState::Completed
        );
        assert_eq!(
            mascot_state(Some(RunStatus::Running), Some(StageStatus::Failed)),
            MascotState::Failed
        );
        assert_eq!(
            mascot_state(Some(RunStatus::Running), Some(StageStatus::Pending)),
            MascotState::Waiting
        );
        assert_eq!(
            mascot_state(Some(RunStatus::Completed), Some(StageStatus::Ready)),
            MascotState::Idle
        );
        assert_eq!(
            mascot_state(Some(RunStatus::Paused), None),
            MascotState::Waiting
        );
    }

    #[test]
    fn role_mapping_is_semantic() {
        assert_eq!(
            mascot_activity(Role::Architect),
            MascotActivity::Architecture
        );
        assert_eq!(
            mascot_activity(Role::Implementer),
            MascotActivity::Implementation
        );
        assert_eq!(
            mascot_activity(Role::CodeQualityReviewer),
            MascotActivity::QualityReview
        );
        assert_eq!(
            mascot_activity(Role::SpecReviewer),
            MascotActivity::SpecReview
        );
        assert_eq!(
            mascot_activity(Role::EngineeringLead),
            MascotActivity::Decision
        );
        assert_eq!(
            mascot_activity(Role::Researcher),
            MascotActivity::Architecture
        );
        assert_eq!(
            mascot_activity(Role::Reviewer),
            MascotActivity::QualityReview
        );
    }

    #[test]
    fn state_overrides_activity_in_label_and_accent() {
        let needs = rows(MascotState::NeedsUser, Some(MascotActivity::Implementation));
        assert_eq!(needs[6], "NEEDS YOU");
        assert!(!needs.join("\n").contains("</>"), "no accent when alert");

        let failed = rows(MascotState::Failed, Some(MascotActivity::Implementation));
        assert_eq!(failed[6], "FAILED");
        assert!(!failed.join("\n").contains("</>"));

        let done = rows(MascotState::Completed, Some(MascotActivity::QualityReview));
        assert_eq!(done[6], "DONE");
        assert!(!done.join("\n").contains("(o)"));

        let running = rows(MascotState::Running, Some(MascotActivity::Implementation));
        assert_eq!(running[6], "BUILDING");
        assert!(running.join("\n").contains("</>"), "accent while running");

        assert_eq!(rows(MascotState::Running, None)[6], "RUNNING");
        assert_eq!(rows(MascotState::Idle, None)[6], "READY");
        assert_eq!(rows(MascotState::Waiting, None)[6], "WAITING");
    }

    #[test]
    fn activity_labels_and_accents_are_distinct_and_compact() {
        let labels: Vec<_> = ALL_ACTIVITIES.iter().map(|a| activity_label(*a)).collect();
        let accents: Vec<_> = ALL_ACTIVITIES.iter().map(|a| activity_accent(*a)).collect();
        for (index, (label, accent)) in labels.iter().zip(&accents).enumerate() {
            assert!(label.len() <= 10);
            assert_eq!(accent.len(), 3);
            for (other_label, other_accent) in labels.iter().zip(&accents).skip(index + 1) {
                assert_ne!(label, other_label);
                assert_ne!(accent, other_accent);
            }
        }
    }

    /// Completed once used caret eyes and a slash mouth. Both kept the
    /// footprint the generic test checks, while rendering as detached marks
    /// above a mouth whose lower half merged into the filled shell below.
    /// Pinned as a relationship rather than as literal art: Completed borrows
    /// the eyes Running already uses and the mouth Idle already uses, so the
    /// three stay in one visual language and no glyph is duplicated here.
    #[test]
    fn the_completed_face_reuses_the_running_eyes_and_idle_mouth() {
        let (_, _, _, idle_mouth) = resting_expression(MascotState::Idle);
        let (_, running_left, running_right, _) = resting_expression(MascotState::Running);
        let (_, done_left, done_right, done_mouth) = resting_expression(MascotState::Completed);

        assert_eq!(done_left, running_left, "left eye follows Running");
        assert_eq!(done_right, running_right, "right eye follows Running");
        assert_eq!(done_mouth, idle_mouth, "mouth follows Idle");

        // The shapes that broke it, named so they cannot come back quietly.
        let face = [done_left, done_right, done_mouth].concat();
        assert!(
            !face.contains('^') && !face.contains('\\') && !face.contains('/'),
            "the eyes and mouth must stay block glyphs: {face:?}"
        );
    }

    /// The guarantee that lets every other surface stay as it was: a frame
    /// that may not move draws exactly the art POD drew before motion
    /// existed. Anything that moves has to come through the phase.
    #[test]
    fn a_still_frame_draws_the_resting_face_for_every_variant() {
        for state in ALL_STATES {
            for activity in std::iter::once(None).chain(ALL_ACTIVITIES.map(Some)) {
                let (antenna, eye_left, eye_right, mouth) = resting_expression(state);
                let drawn = rows_in(state, activity, MotionFrame::still());
                assert!(
                    drawn[0].contains(antenna)
                        && drawn[2].contains(eye_left)
                        && drawn[2].contains(eye_right)
                        && drawn[3].contains(mouth),
                    "a still frame changed {state:?}/{activity:?}: {drawn:?}"
                );
            }
        }
    }

    /// The repeating motion says one thing — Polycode considers this work
    /// active — so it may appear only on the state that carries that meaning.
    /// A face that moved while nothing was happening would be claiming
    /// progress that is not there. It is never evidence that the process
    /// behind the run is alive; see the contract in `motion`.
    #[test]
    fn only_running_moves_and_the_blink_stays_inside_the_footprint() {
        for state in ALL_STATES {
            let resting = rows_in(state, None, MotionFrame::still());
            let moving = rows_in(
                state,
                None,
                MotionFrame::new(MotionAllowance::ActiveStateAndTransitions, 1, false),
            );
            if matches!(state, MascotState::Running) {
                assert_ne!(resting, moving, "Running work has to look alive");
            } else {
                assert_eq!(
                    resting, moving,
                    "{state:?} is not working, so it holds still"
                );
            }
        }
    }

    /// The blink borrows an eye the file already draws, so motion can never
    /// be the thing that introduces a glyph nobody checked.
    #[test]
    fn the_blink_borrows_an_eye_that_already_exists() {
        let (_, blink_left, blink_right, _) = expression(MascotState::Running, 1, false);
        let (_, waiting_left, waiting_right, _) = resting_expression(MascotState::Waiting);
        assert_eq!(blink_left, waiting_left);
        assert_eq!(blink_right, waiting_right);

        let (running_antenna, _, _, running_mouth) = resting_expression(MascotState::Running);
        let (antenna, _, _, mouth) = expression(MascotState::Running, 1, false);
        assert_eq!(antenna, running_antenna, "a blink is eyes, not a new face");
        assert_eq!(mouth, running_mouth, "a blink is eyes, not a new face");
    }

    /// A reaction is POD noticing. It has to be visible from every state,
    /// including the ones whose resting eyes are already wide, and it has to
    /// borrow eyes the file already draws.
    #[test]
    fn a_reaction_is_visible_from_every_state_and_invents_no_eye() {
        let (_, wide, _, _) = resting_expression(MascotState::Idle);
        let (_, narrow, _, _) = resting_expression(MascotState::Running);
        for state in ALL_STATES {
            let resting = rows_in(state, None, MotionFrame::still());
            let reacting = rows_in(
                state,
                None,
                MotionFrame::new(MotionAllowance::ActiveStateAndTransitions, 0, true),
            );
            assert_ne!(
                resting, reacting,
                "{state:?} showed no sign of having noticed anything"
            );
            let (_, eye_left, eye_right, _) = expression(state, 0, true);
            assert_eq!(eye_left, eye_right, "a reaction keeps POD's face symmetric");
            assert!(
                eye_left == wide || eye_left == narrow,
                "{state:?} reacted with an eye that exists nowhere else: {eye_left:?}"
            );
        }
    }

    /// Something just happened outranks still working: a run that finishes
    /// mid-blink shows that it finished.
    #[test]
    fn a_reaction_outranks_a_blink() {
        let blinking = expression(MascotState::Running, 1, false);
        let reacting = expression(MascotState::Running, 1, true);
        assert_ne!(blinking, reacting);
        assert_eq!(reacting, expression(MascotState::Running, 0, true));
    }

    #[test]
    fn faces_differ_between_states_beyond_the_label() {
        let faces: Vec<Vec<String>> = ALL_STATES
            .iter()
            .map(|state| rows(*state, None)[..6].to_vec())
            .collect();
        for (index, face) in faces.iter().enumerate() {
            for other in faces.iter().skip(index + 1) {
                assert_ne!(face, other, "each state needs a distinct face");
            }
        }
    }
}
