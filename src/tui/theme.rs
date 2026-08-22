//! One Polycode palette and the small set of surfaces built from it.
//!
//! Every color the TUI paints comes from here, so semantic meaning stays
//! stable across screens: one accent per screen plus semantic exceptions.
//! State is never communicated by color alone — each surface pairs its color
//! with a glyph or a word, so a monochrome terminal reads the same.

use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Primary Polycode accent: active work, current stage, product identity.
pub(crate) const ACCENT: Color = Color::Cyan;
/// Completion, verified artifacts, apply.
pub(crate) const SUCCESS: Color = Color::Green;
/// The run needs the user. Never used for failure.
pub(crate) const ATTENTION: Color = Color::Yellow;
/// Failure and destructive actions.
pub(crate) const DANGER: Color = Color::Red;
/// Suspended work: paused and interrupted.
pub(crate) const SUSPENDED: Color = Color::Magenta;
/// Secondary labels, metadata, structure, inactive pipeline connectors.
pub(crate) const MUTED: Color = Color::DarkGray;

/// Primary content: the terminal's own foreground.
pub(crate) fn text() -> Style {
    Style::default()
}

/// Secondary content: labels, metadata, connectors, quiet hints.
pub(crate) fn muted() -> Style {
    Style::default().fg(MUTED)
}

/// Section label inside a panel — the only structural device that replaces a
/// border, so it must read as a heading without shouting.
pub(crate) fn section(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        label.to_owned(),
        Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
    ))
}

/// The one strong surface: a reversed block of color carrying a short word.
/// Used at most once per screen, for the state that must not be missed.
pub(crate) fn chip(label: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .bg(color)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    )
}

/// Keyboard affordance: the key reads as the interactive part, the verb as
/// plain content.
pub(crate) fn action(key: &str, label: &str, color: Color) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("[{key}]"), Style::default().fg(color)),
        Span::styled(format!(" {label}"), text()),
    ]
}

/// Horizontal rule used where a border would be too heavy.
pub(crate) fn rule(width: u16) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width as usize),
        Style::default().fg(MUTED),
    ))
}

/// Centered short rule, used to seat POD under the pipeline instead of
/// letting it float in whatever space is left over.
pub(crate) fn centered_rule(width: u16) -> Line<'static> {
    let span = width.saturating_sub(4).clamp(4, 14);
    Line::from(Span::styled(
        "─".repeat(span as usize),
        Style::default().fg(MUTED),
    ))
    .alignment(Alignment::Center)
}

/// One row with content pushed to both edges. Used for headers and footers
/// where the secondary half is quiet and droppable.
pub(crate) fn spread(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: u16,
) -> Line<'static> {
    let used: usize = left
        .iter()
        .chain(right.iter())
        .map(|span| span.content.chars().count())
        .sum();
    let gap = (width as usize).saturating_sub(used).max(1);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_pushes_the_secondary_half_to_the_right_edge() {
        let line = spread(vec![Span::raw("left")], vec![Span::raw("right")], 20);
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(rendered.chars().count(), 20);
        assert!(rendered.starts_with("left"));
        assert!(rendered.ends_with("right"));
    }

    #[test]
    fn spread_never_overlaps_when_the_halves_do_not_fit() {
        let line = spread(
            vec![Span::raw("a very long left half")],
            vec![Span::raw("and a right half")],
            10,
        );
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.contains("a very long left half and a right half"));
    }

    #[test]
    fn chips_and_actions_carry_a_word_not_only_a_color() {
        assert!(
            chip("READY TO REVIEW", SUCCESS)
                .content
                .contains("READY TO REVIEW")
        );
        let spans = action("a", "Apply changes", SUCCESS);
        let rendered: String = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(rendered, "[a] Apply changes");
    }

    #[test]
    fn centered_rule_stays_short_and_bounded() {
        assert!(centered_rule(200).spans[0].content.chars().count() <= 14);
        assert!(centered_rule(2).spans[0].content.chars().count() >= 4);
    }
}
