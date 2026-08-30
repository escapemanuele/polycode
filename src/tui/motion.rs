//! When the interface is allowed to move.
//!
//! Motion is honest only when time is part of what it says: Polycode
//! considers this work active, or something just changed. Everything else
//! competes with the text the user is reading, and Polycode runs for hours.
//!
//! One contract governs the repeating half, and it is a claim about what
//! movement is *allowed to mean*:
//!
//! > Active-state motion is redundant, sparse and non-authoritative. It may
//! > reinforce an active semantic state; it must never be the evidence that
//! > work is actually alive.
//!
//! The distinction is not pedantry. `RUNNING`, the advancing elapsed, the
//! pipeline and POD's blink all report the same thing — the domain's view of
//! the run — and there is a window in which that view says Running while
//! every process behind it is already dead (see the abandoned-run grace in
//! `run_service`). A user who read the blink as proof of a live process
//! would be reading a guarantee nothing here can make. Because the movement
//! is redundant, it can be switched off entirely without losing one bit of
//! information, which is exactly the property that makes it safe to add.
//!
//! Two independent inputs decide it, and they meet as a minimum:
//!
//! * the surface states a **ceiling** — a screen the user reads never moves,
//!   whatever anyone asked for;
//! * the user states a **preference** — which can only lower that ceiling.
//!
//! Because a preference can never raise a ceiling, "reading surfaces never
//! move" is a property of this module rather than of the next contributor's
//! good taste.

use std::time::Duration;

use super::state::{Overlay, Screen};

/// How often POD blinks, and for how long.
///
/// Redundant by design: everything it reinforces is already written in words
/// and glyphs elsewhere on the screen. That is what lets it be this rare.
///
/// Deliberately sparse and lopsided. An even alternation — half a second
/// open, half a second shut — reads as a pulsing glitch rather than as a
/// creature, and over the hours a run takes it becomes something to look
/// away from. A short blink every few seconds says the same thing (this work
/// is active) and then gets out of the way. The blink is comfortably longer
/// than the 100ms event poll, so it never falls between two redraws.
const BLINK_PERIOD: Duration = Duration::from_secs(3);
const BLINK_LENGTH: Duration = Duration::from_millis(200);

/// How much movement is permitted, ordered from most to least restrictive so
/// the two inputs combine with `min`. An allowance rather than a policy: a
/// surface naming one is stating a ceiling, not a wish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MotionAllowance {
    /// Nothing moves. Every frame of this surface is identical to the last
    /// until the underlying state changes.
    Disabled,
    /// A finite reaction may play when state changes, but nothing repeats on
    /// its own.
    TransitionsOnly,
    /// Work the domain considers active may also carry the repeating,
    /// non-authoritative motion described in this module's contract.
    ActiveStateAndTransitions,
}

impl MotionAllowance {
    pub(crate) fn allows_active_state(self) -> bool {
        self == Self::ActiveStateAndTransitions
    }

    pub(crate) fn allows_transitions(self) -> bool {
        self >= Self::TransitionsOnly
    }
}

/// What the user asked for. Independent of the surface: it can only lower
/// what the surface already permits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MotionSetting {
    /// Nothing anywhere ever moves.
    Off,
    /// Something that just happened may still be shown, but nothing repeats.
    Reduced,
    /// The default: the surface decides.
    Full,
}

impl MotionSetting {
    const fn ceiling(self) -> MotionAllowance {
        match self {
            Self::Off => MotionAllowance::Disabled,
            Self::Reduced => MotionAllowance::TransitionsOnly,
            Self::Full => MotionAllowance::ActiveStateAndTransitions,
        }
    }

    /// Resolves the preference from environment *values* rather than reading
    /// them, so the policy stays a pure function and tests never mutate the
    /// process environment.
    ///
    /// A dumb terminal is treated as `Off`: it cannot be relied on to repaint
    /// a cell without leaving the previous frame behind.
    pub(crate) fn resolve(motion: Option<&str>, term: Option<&str>) -> Self {
        if term == Some("dumb") {
            return Self::Off;
        }
        match motion.map(str::trim) {
            Some("off") => Self::Off,
            Some("reduced") => Self::Reduced,
            // Anything else — unset, empty, misspelt — is the default. An
            // unreadable value must not silently change the interface.
            _ => Self::Full,
        }
    }

    /// Reads the process environment. Kept separate from `resolve` so the
    /// policy itself stays pure.
    pub(crate) fn from_environment() -> Self {
        Self::resolve(
            std::env::var("POLYCODE_MOTION").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }
}

thread_local! {
    /// The preference this thread honours. Thread-local rather than global
    /// for the same reason as the palette: a test can render one surface
    /// under a different setting without disturbing the others. The TUI
    /// resolves it once at startup and never changes it.
    static SETTING: std::cell::Cell<MotionSetting> =
        const { std::cell::Cell::new(MotionSetting::Full) };
}

/// Installs the preference for this thread. Called once at TUI startup.
pub(crate) fn set_motion_setting(setting: MotionSetting) {
    SETTING.with(|current| current.set(setting));
}

pub(crate) fn motion_setting() -> MotionSetting {
    SETTING.with(std::cell::Cell::get)
}

/// The ceiling a surface imposes, before the user's preference lowers it.
///
/// An open overlay wins over the screen behind it: while Polycode is asking
/// for a decision — and one of those decisions discards work — the surface
/// under the question stops moving too. Otherwise "reading surfaces never
/// move" would hold only for as long as nobody looked past the overlay.
pub(crate) const fn surface_ceiling(screen: Screen, overlay: Option<Overlay>) -> MotionAllowance {
    if overlay.is_some() {
        return MotionAllowance::Disabled;
    }
    match screen {
        // Operating surfaces: what they show is work in progress, so time is
        // part of the information.
        Screen::Runs | Screen::RunDetail => MotionAllowance::ActiveStateAndTransitions,
        // Reading surfaces: prose, logs, a diff, a form being filled in.
        Screen::Artifact | Screen::Logs | Screen::Diff | Screen::NewRun => {
            MotionAllowance::Disabled
        }
    }
}

/// What is actually allowed: the more restrictive of what the surface permits
/// and what the user asked for.
pub(crate) fn allowance(
    screen: Screen,
    overlay: Option<Overlay>,
    setting: MotionSetting,
) -> MotionAllowance {
    surface_ceiling(screen, overlay).min(setting.ceiling())
}

/// Which frame the clock is on: 1 during a blink, 0 the rest of the
/// time. Phase 0 is the resting art, so a surface that never asks the clock
/// and a surface between blinks draw exactly the same thing.
pub(crate) fn active_phase(elapsed: Duration) -> u8 {
    let within = elapsed.as_millis() % BLINK_PERIOD.as_millis();
    u8::from(within >= BLINK_PERIOD.as_millis() - BLINK_LENGTH.as_millis())
}

/// One frame's permission to move, handed to whatever draws it.
///
/// The phase is private on purpose. A renderer cannot read the clock around
/// the allowance: it asks for `active_phase`, which is zero — the resting
/// frame — whenever motion is not permitted here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MotionFrame {
    allowance: MotionAllowance,
    phase: u8,
    reacting: bool,
}

impl MotionFrame {
    pub(crate) const fn new(allowance: MotionAllowance, phase: u8, reacting: bool) -> Self {
        Self {
            allowance,
            phase,
            reacting,
        }
    }

    /// A frame that never moves. Production always resolves a real policy
    /// from the surface, so this exists for the tests that are about
    /// something other than motion and must not depend on the clock.
    #[cfg(test)]
    pub(crate) const fn still() -> Self {
        Self {
            allowance: MotionAllowance::Disabled,
            phase: 0,
            reacting: false,
        }
    }

    pub(crate) fn active_phase(self) -> u8 {
        if self.allowance.allows_active_state() {
            self.phase
        } else {
            0
        }
    }

    /// Whether POD is in the moment just after something changed. Finite by
    /// construction — the caller only ever reports a window that ends — and
    /// permitted one level lower than the repeating kind, because a reaction
    /// says something happened rather than repeating itself forever.
    pub(crate) fn is_reacting(self) -> bool {
        self.reacting && self.allowance.allows_transitions()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_SCREENS: [Screen; 6] = [
        Screen::Runs,
        Screen::RunDetail,
        Screen::Artifact,
        Screen::Logs,
        Screen::Diff,
        Screen::NewRun,
    ];
    const READING_SCREENS: [Screen; 4] =
        [Screen::Artifact, Screen::Logs, Screen::Diff, Screen::NewRun];
    const ALL_OVERLAYS: [Overlay; 5] = [
        Overlay::Help,
        Overlay::Attention,
        Overlay::ApplyConfirm,
        Overlay::DiscardConfirm,
        Overlay::Update,
    ];
    const ALL_SETTINGS: [MotionSetting; 3] = [
        MotionSetting::Off,
        MotionSetting::Reduced,
        MotionSetting::Full,
    ];

    #[test]
    fn a_surface_the_user_reads_never_moves_however_much_motion_was_asked_for() {
        for screen in READING_SCREENS {
            for setting in ALL_SETTINGS {
                assert_eq!(
                    allowance(screen, None, setting),
                    MotionAllowance::Disabled,
                    "{screen:?} is read, not operated, so it may not move under {setting:?}"
                );
            }
        }
    }

    #[test]
    fn a_preference_can_lower_a_ceiling_and_never_raise_one() {
        for screen in ALL_SCREENS {
            for overlay in ALL_OVERLAYS.map(Some).into_iter().chain([None]) {
                let ceiling = surface_ceiling(screen, overlay);
                for setting in ALL_SETTINGS {
                    let resolved = allowance(screen, overlay, setting);
                    assert!(
                        resolved <= ceiling,
                        "{setting:?} raised {screen:?}/{overlay:?} above its ceiling {ceiling:?}"
                    );
                    assert!(
                        resolved <= setting.ceiling(),
                        "{screen:?}/{overlay:?} moved more than {setting:?} allows"
                    );
                }
            }
        }
    }

    #[test]
    fn an_open_overlay_stills_the_surface_behind_it() {
        for screen in ALL_SCREENS {
            for overlay in ALL_OVERLAYS {
                assert_eq!(
                    allowance(screen, Some(overlay), MotionSetting::Full),
                    MotionAllowance::Disabled,
                    "{overlay:?} asks a question, so {screen:?} behind it holds still"
                );
            }
        }
    }

    #[test]
    fn an_operating_surface_is_the_only_one_that_may_repeat() {
        for screen in ALL_SCREENS {
            let allows = allowance(screen, None, MotionSetting::Full).allows_active_state();
            assert_eq!(
                allows,
                matches!(screen, Screen::Runs | Screen::RunDetail),
                "{screen:?} disagrees with the operating/reading split"
            );
        }
    }

    #[test]
    fn the_preference_understands_off_and_reduced_and_ignores_the_rest() {
        assert_eq!(
            MotionSetting::resolve(Some("off"), None),
            MotionSetting::Off
        );
        assert_eq!(
            MotionSetting::resolve(Some("reduced"), None),
            MotionSetting::Reduced
        );
        assert_eq!(
            MotionSetting::resolve(Some(" reduced "), None),
            MotionSetting::Reduced
        );
        for value in [None, Some(""), Some("full"), Some("yes"), Some("Off")] {
            assert_eq!(
                MotionSetting::resolve(value, None),
                MotionSetting::Full,
                "{value:?} is not a recognised preference, so it must not change anything"
            );
        }
    }

    #[test]
    fn a_dumb_terminal_holds_still_whatever_the_preference_says() {
        assert_eq!(
            MotionSetting::resolve(Some("full"), Some("dumb")),
            MotionSetting::Off
        );
    }

    /// A blink is brief and rare: most of the time POD's face is the resting
    /// art, which is what keeps a running screen calm to sit in front of.
    /// Repeating motion needs the top allowance. A reaction happens once and
    /// stops, which is exactly what `reduced` keeps.
    #[test]
    fn a_reaction_survives_one_level_below_the_repeating_kind() {
        let reacting = |allowance| MotionFrame::new(allowance, 0, true).is_reacting();
        assert!(reacting(MotionAllowance::ActiveStateAndTransitions));
        assert!(reacting(MotionAllowance::TransitionsOnly));
        assert!(!reacting(MotionAllowance::Disabled));
        assert!(!MotionFrame::still().is_reacting());
        assert!(
            !MotionFrame::new(MotionAllowance::ActiveStateAndTransitions, 0, false).is_reacting(),
            "a frame nothing happened in must not react"
        );
    }

    #[test]
    fn a_blink_is_short_rare_and_repeats() {
        assert_eq!(active_phase(Duration::ZERO), 0);
        assert_eq!(active_phase(Duration::from_millis(2799)), 0);
        assert_eq!(active_phase(Duration::from_millis(2800)), 1);
        assert_eq!(active_phase(Duration::from_millis(2999)), 1);
        assert_eq!(active_phase(Duration::from_secs(3)), 0);
        // And again on the next cycle, hours in, without overflowing.
        assert_eq!(active_phase(Duration::from_millis(5800)), 1);
        assert_eq!(active_phase(Duration::from_secs(36_000)), 0);

        // Overwhelmingly still: at 100ms redraws, at most a couple of frames
        // in every thirty show a blink at all.
        let moving = (0..30_000)
            .step_by(100)
            .filter(|ms| active_phase(Duration::from_millis(*ms)) == 1)
            .count();
        assert!(
            moving * 10 < 30_000 / 100,
            "POD blinked on {moving} of 300 frames, which is a twitch, not a blink"
        );
    }

    #[test]
    fn a_frame_that_may_not_move_reports_the_resting_phase_whatever_the_clock_says() {
        for allowance in [MotionAllowance::Disabled, MotionAllowance::TransitionsOnly] {
            assert_eq!(
                MotionFrame::new(allowance, 1, false).active_phase(),
                0,
                "{allowance:?} handed out a moving phase"
            );
        }
        assert_eq!(MotionFrame::still().active_phase(), 0);
        assert_eq!(
            MotionFrame::new(MotionAllowance::ActiveStateAndTransitions, 1, false).active_phase(),
            1
        );
    }
}
