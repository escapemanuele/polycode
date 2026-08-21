//! Polycode terminal identity mark.
//!
//! No canonical mascot exists in product assets yet, so the design lives in
//! one central constant and can be swapped without touching layout logic.
//! ASCII-only, compact, and rendered only when the layout has room; the run
//! status label is a projection of the canonical `RunStatus`.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::domain::RunStatus;

/// Stacked polygon mark; ASCII-safe and fixed-width.
pub(crate) const MASCOT_ART: [&str; 4] =
    [r"    /\    ", r"   /  \   ", r"  / /\ \  ", r" /_/  \_\ "];

pub(crate) const MASCOT_WIDTH: u16 = 10;

/// Art rows plus wordmark and status label.
pub(crate) const MASCOT_HEIGHT: u16 = 6;

pub(crate) const fn status_label(status: Option<RunStatus>) -> &'static str {
    match status {
        Some(RunStatus::Running) => "RUNNING",
        Some(RunStatus::NeedsUser) => "NEEDS YOU",
        Some(RunStatus::Failed) => "FAILED",
        Some(RunStatus::Completed | RunStatus::Applied) => "DONE",
        Some(
            RunStatus::Created
            | RunStatus::Preparing
            | RunStatus::Ready
            | RunStatus::Paused
            | RunStatus::Interrupted
            | RunStatus::Discarded,
        )
        | None => "IDLE",
    }
}

const fn label_style(status: Option<RunStatus>) -> Style {
    match status {
        Some(RunStatus::Running) => Style::new().fg(Color::Cyan),
        Some(RunStatus::NeedsUser) => Style::new().fg(Color::Yellow),
        Some(RunStatus::Failed) => Style::new().fg(Color::Red),
        Some(RunStatus::Completed | RunStatus::Applied) => Style::new().fg(Color::Green),
        _ => Style::new().fg(Color::DarkGray),
    }
}

pub(crate) fn mascot_lines(status: Option<RunStatus>) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = MASCOT_ART
        .iter()
        .map(|row| {
            Line::from(Span::styled(*row, Style::default().fg(Color::Cyan)))
                .alignment(ratatui::layout::Alignment::Center)
        })
        .collect();
    lines.push(
        Line::from(Span::styled(
            "POLYCODE",
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .alignment(ratatui::layout::Alignment::Center),
    );
    lines.push(
        Line::from(Span::styled(
            status_label(status),
            label_style(status).add_modifier(Modifier::BOLD),
        ))
        .alignment(ratatui::layout::Alignment::Center),
    );
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mascot_is_ascii_only_and_fixed_width() {
        for row in MASCOT_ART {
            assert!(row.is_ascii());
            assert_eq!(row.len(), MASCOT_WIDTH as usize);
        }
        assert_eq!(MASCOT_HEIGHT as usize, MASCOT_ART.len() + 2);
        assert_eq!(mascot_lines(None).len(), MASCOT_HEIGHT as usize);
    }

    #[test]
    fn status_labels_project_run_status() {
        assert_eq!(status_label(Some(RunStatus::Running)), "RUNNING");
        assert_eq!(status_label(Some(RunStatus::NeedsUser)), "NEEDS YOU");
        assert_eq!(status_label(Some(RunStatus::Failed)), "FAILED");
        assert_eq!(status_label(Some(RunStatus::Applied)), "DONE");
        assert_eq!(status_label(None), "IDLE");
    }
}
