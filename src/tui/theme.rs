//! One Polycode palette and the small set of surfaces built from it.
//!
//! Every color the TUI paints comes from here, so semantic meaning stays
//! stable across screens: one accent per screen plus semantic exceptions.
//! State is never communicated by color alone — each surface pairs its color
//! with a glyph or a word, so a monochrome terminal reads the same.

use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// What the terminal can actually paint. Detected once at startup; the
/// palette is chosen from it, so nothing downstream has to ask again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorCapability {
    /// No color at all: `NO_COLOR`, a dumb terminal, or output the user has
    /// asked to keep plain. Every semantic token collapses to the terminal's
    /// own foreground, so meaning has to survive on glyphs and words alone.
    Mono,
    /// The sixteen ANSI colors, named rather than specified — they resolve to
    /// whatever the user's terminal theme defines, so Polycode sits inside
    /// their palette instead of imposing one.
    Ansi16,
}

impl ColorCapability {
    /// Resolves capability from the environment, taking the values rather
    /// than reading them, so the decision is testable without mutating the
    /// process environment.
    ///
    /// `NO_COLOR` follows the convention at no-color.org: present and
    /// non-empty disables color regardless of its value.
    pub(crate) fn resolve(no_color: Option<&str>, term: Option<&str>) -> Self {
        if no_color.is_some_and(|value| !value.is_empty()) || term == Some("dumb") {
            return Self::Mono;
        }
        Self::Ansi16
    }

    /// Reads the process environment. Kept separate from `resolve` so the
    /// policy stays a pure function.
    pub(crate) fn from_environment() -> Self {
        Self::resolve(
            std::env::var("NO_COLOR").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }
}

/// The eight semantic tokens, materialised for one capability. Code names the
/// meaning; only this struct knows the hue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    accent: Color,
    success: Color,
    attention: Color,
    danger: Color,
    suspended: Color,
    muted: Color,
    structure: Color,
    chip_fg: Color,
}

impl Palette {
    pub(crate) const fn for_capability(capability: ColorCapability) -> Self {
        match capability {
            ColorCapability::Ansi16 => Self {
                // Primary Polycode accent: active work, current stage,
                // product identity.
                accent: Color::Cyan,
                // Completion, verified artifacts, apply.
                success: Color::Green,
                // The run needs the user. Never used for failure.
                attention: Color::Yellow,
                // Failure and destructive actions.
                danger: Color::Red,
                // Suspended work: paused and interrupted.
                suspended: Color::Magenta,
                // Secondary labels, metadata, structure, inactive connectors.
                muted: Color::DarkGray,
                // Structural framing and the architecture role accent. Kept
                // distinct from the accent so framework reads as structure,
                // not focus, and never competes with the one active-work
                // accent per screen.
                structure: Color::Blue,
                // Foreground of a reversed chip: black on the chip's own
                // color keeps its word legible on any accent hue.
                chip_fg: Color::Black,
            },
            // Every token is the terminal's own foreground. Surfaces that
            // relied on hue alone become indistinguishable here — which is
            // the point: the monochrome test reads this palette to prove the
            // interface still tells the truth without color.
            ColorCapability::Mono => Self {
                accent: Color::Reset,
                success: Color::Reset,
                attention: Color::Reset,
                danger: Color::Reset,
                suspended: Color::Reset,
                muted: Color::Reset,
                structure: Color::Reset,
                // A reversed chip still inverts, so its text takes the
                // background the terminal paints behind it.
                chip_fg: Color::Reset,
            },
        }
    }
}

thread_local! {
    /// The palette this thread paints with. Thread-local rather than global
    /// so a test can render one variant monochrome without disturbing the
    /// others; the TUI resolves it once at startup and never changes it.
    static PALETTE: std::cell::Cell<Palette> =
        const { std::cell::Cell::new(Palette::for_capability(ColorCapability::Ansi16)) };
}

/// Installs the palette for this thread. Called once at TUI startup.
pub(crate) fn set_palette(palette: Palette) {
    PALETTE.with(|current| current.set(palette));
}

/// Runs `body` with `palette` installed, restoring the previous one even if
/// `body` panics. Tests render monochrome without leaking that choice into
/// whatever else the harness runs on this thread.
#[cfg(test)]
pub(crate) fn with_palette<T>(palette: Palette, body: impl FnOnce() -> T) -> T {
    struct Restore(Palette);
    impl Drop for Restore {
        fn drop(&mut self) {
            set_palette(self.0);
        }
    }
    let _restore = Restore(self::palette());
    set_palette(palette);
    body()
}

fn palette() -> Palette {
    PALETTE.with(std::cell::Cell::get)
}

/// Primary Polycode accent: active work, current stage, product identity.
pub(crate) fn accent() -> Color {
    palette().accent
}
/// Completion, verified artifacts, apply.
pub(crate) fn success() -> Color {
    palette().success
}
/// The run needs the user. Never used for failure.
pub(crate) fn attention() -> Color {
    palette().attention
}
/// Failure and destructive actions.
pub(crate) fn danger() -> Color {
    palette().danger
}
/// Suspended work: paused and interrupted.
pub(crate) fn suspended() -> Color {
    palette().suspended
}
/// Secondary labels, metadata, structure, inactive pipeline connectors.
pub(crate) fn muted_color() -> Color {
    palette().muted
}
/// Structural framing and the architecture role accent.
pub(crate) fn structure() -> Color {
    palette().structure
}
/// Foreground of a reversed chip.
pub(crate) fn chip_fg() -> Color {
    palette().chip_fg
}

/// Primary content: the terminal's own foreground.
pub(crate) fn text() -> Style {
    Style::default()
}

/// Secondary content: labels, metadata, connectors, quiet hints.
pub(crate) fn muted() -> Style {
    Style::default().fg(muted_color())
}

/// Section label inside a panel — the only structural device that replaces a
/// border, so it must read as a heading without shouting.
pub(crate) fn section(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        label.to_owned(),
        Style::default()
            .fg(muted_color())
            .add_modifier(Modifier::BOLD),
    ))
}

/// The one strong surface: a reversed block of color carrying a short word.
/// Used at most once per screen, for the state that must not be missed.
pub(crate) fn chip(label: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .bg(color)
            .fg(chip_fg())
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
        Style::default().fg(muted_color()),
    ))
}

/// Diff hunk header — the `diff --git` line and the `@@` range: structural
/// framing in bold so it reads as a boundary, not as changed content.
pub(crate) fn diff_hunk() -> Style {
    Style::default()
        .fg(structure())
        .add_modifier(Modifier::BOLD)
}

/// Centered short rule, used to seat POD under the pipeline instead of
/// letting it float in whatever space is left over.
pub(crate) fn centered_rule(width: u16) -> Line<'static> {
    let span = width.saturating_sub(4).clamp(4, 14);
    Line::from(Span::styled(
        "─".repeat(span as usize),
        Style::default().fg(muted_color()),
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
    fn no_color_and_dumb_terminals_resolve_to_monochrome() {
        // The no-color.org convention: present and non-empty, whatever the
        // value. An empty NO_COLOR is explicitly not a request for mono.
        assert_eq!(
            ColorCapability::resolve(Some("1"), None),
            ColorCapability::Mono
        );
        assert_eq!(
            ColorCapability::resolve(Some("0"), None),
            ColorCapability::Mono
        );
        assert_eq!(
            ColorCapability::resolve(Some(""), None),
            ColorCapability::Ansi16
        );
        assert_eq!(
            ColorCapability::resolve(None, None),
            ColorCapability::Ansi16
        );
        assert_eq!(
            ColorCapability::resolve(None, Some("xterm-256color")),
            ColorCapability::Ansi16
        );
        assert_eq!(
            ColorCapability::resolve(None, Some("dumb")),
            ColorCapability::Mono
        );
    }

    /// Under Mono every token is the same colour, so anything that survives
    /// there is carried by a glyph or a word. This is what lets the palette
    /// grow later without the aesthetic layer quietly acquiring meaning.
    #[test]
    fn the_monochrome_palette_leaves_no_token_distinguishable_by_colour() {
        let mono = Palette::for_capability(ColorCapability::Mono);
        let tokens = [
            mono.accent,
            mono.success,
            mono.attention,
            mono.danger,
            mono.suspended,
            mono.muted,
            mono.structure,
            mono.chip_fg,
        ];
        for token in tokens {
            assert_eq!(
                token, tokens[0],
                "a token that keeps its own hue under Mono can still smuggle meaning in colour"
            );
        }

        // And the coloured palette must genuinely distinguish them, or the
        // monochrome check above would be vacuous.
        let ansi = Palette::for_capability(ColorCapability::Ansi16);
        let coloured = [
            ansi.accent,
            ansi.success,
            ansi.attention,
            ansi.danger,
            ansi.suspended,
            ansi.muted,
            ansi.structure,
        ];
        for (index, token) in coloured.iter().enumerate() {
            for other in coloured.iter().skip(index + 1) {
                assert_ne!(token, other, "semantic tokens must not share a hue");
            }
        }
    }

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
            chip("READY TO REVIEW", success())
                .content
                .contains("READY TO REVIEW")
        );
        let spans = action("a", "Apply changes", success());
        let rendered: String = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(rendered, "[a] Apply changes");
    }

    #[test]
    fn centered_rule_stays_short_and_bounded() {
        assert!(centered_rule(200).spans[0].content.chars().count() <= 14);
        assert!(centered_rule(2).spans[0].content.chars().count() >= 4);
    }

    // Regression guard for the "one palette" invariant: a raw Color:: may live only
    // in theme.rs, the canonical source of truth. Every other tui source file must
    // reference colors solely through theme constants/helpers. The one permitted
    // exception is the ratatui Color::Reset assertion in render.rs, which proves a
    // rail was *not* painted rather than choosing a hue. Scanning recursively means
    // a future submodule under src/tui can't hide a leak either.
    #[test]
    fn raw_color_lives_only_in_theme() {
        // Nested helpers must be declared before any statement (items_after_statements).
        fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_rs(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }

        // Permitted raw Color:: usages OUTSIDE theme.rs: the single entry is the reset
        // assertion in render.rs. Any occurrence not covered here fails with its location,
        // so an edit can't silently reintroduce a raw hue.
        let allow: &[(&str, &str)] = &[("render.rs", "Color::Reset")];

        let tui_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tui");
        let mut files: Vec<_> = Vec::new();
        collect_rs(&tui_dir, &mut files);
        files.sort();

        for path in &files {
            let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
            if file_name == "theme.rs" {
                continue; // canonical palette source of truth — raw Color:: belongs here
            }
            let contents = std::fs::read_to_string(path).unwrap_or_default();
            for (i, line) in contents.lines().enumerate() {
                if !line.contains("Color::") {
                    continue;
                }
                let allowed = allow
                    .iter()
                    .any(|(name, sub)| file_name == *name && line.contains(sub));
                assert!(
                    allowed,
                    "raw Color:: leaked outside theme.rs at {file_name}:{line_no}\n      {snippet}",
                    line_no = i + 1,
                    snippet = line.trim(),
                );
            }
        }
    }
}
