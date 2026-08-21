//! POD — the Polycode Operator Droid.
//!
//! One high-definition pixel companion with a single stable silhouette,
//! rendered compositionally: fixed shell + state expression (antenna, eyes,
//! mouth) + optional activity accent + one compact label. State comes from
//! canonical `RunStatus`/`StageStatus` and activity from the typed `Role`;
//! nothing is inferred from prose. Every variant occupies exactly
//! `MASCOT_WIDTH` × `MASCOT_HEIGHT` cells so surrounding layout never
//! shifts. Art uses only ASCII plus the single-cell block elements
//! █ ▄ ▀ ▌ ▐ (see `GLYPH_WHITELIST`); no emoji, no ambiguous-width glyphs,
//! no animation.

use ratatui::style::{Color, Modifier, Style};
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

/// Expression slots for one state: antenna core (2 chars), left eye,
/// right eye (2 chars each), mouth (4 chars).
const fn expression(
    state: MascotState,
) -> (&'static str, &'static str, &'static str, &'static str) {
    match state {
        MascotState::Idle => ("██", "██", "██", "▀▄▄▀"),
        MascotState::Running => ("██", "▀▀", "▀▀", " ▄▄ "),
        MascotState::Waiting => ("zz", "▄▄", "▄▄", " .. "),
        MascotState::NeedsUser => ("!!", "▐█", "█▌", " ██ "),
        MascotState::Completed => ("██", "^^", "^^", "\\▄▄/"),
        MascotState::Failed => ("xx", "xx", "xx", "▄▀▀▄"),
    }
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

const fn state_style(state: MascotState) -> Style {
    match state {
        MascotState::Idle | MascotState::Running => Style::new().fg(Color::Cyan),
        MascotState::Waiting => Style::new().fg(Color::DarkGray),
        MascotState::NeedsUser => Style::new().fg(Color::Yellow),
        MascotState::Completed => Style::new().fg(Color::Green),
        MascotState::Failed => Style::new().fg(Color::Red),
    }
}

const fn accent_style(activity: MascotActivity) -> Style {
    match activity {
        MascotActivity::Architecture => Style::new().fg(Color::Blue),
        MascotActivity::Implementation
        | MascotActivity::QualityReview
        | MascotActivity::SpecReview
        | MascotActivity::Decision => Style::new().fg(Color::DarkGray),
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
/// Always `MASCOT_HEIGHT` rows; art rows are exactly `MASCOT_WIDTH` cells.
pub(crate) fn mascot_lines(
    state: MascotState,
    activity: Option<MascotActivity>,
) -> Vec<Line<'static>> {
    let (antenna, eye_left, eye_right, mouth) = expression(state);
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

    fn rows(state: MascotState, activity: Option<MascotActivity>) -> Vec<String> {
        mascot_lines(state, activity)
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
                let rows = rows(state, activity);
                assert_eq!(rows.len(), MASCOT_HEIGHT as usize);
                for row in &rows[..6] {
                    assert_eq!(
                        cell_width(row),
                        MASCOT_WIDTH as usize,
                        "unstable footprint for {state:?}/{activity:?}: {row:?}"
                    );
                }
                assert!(cell_width(&rows[6]) <= MASCOT_WIDTH as usize);
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
