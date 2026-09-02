use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap};

use chrono::{DateTime, Utc};

use crate::app::{BlockedDependencyRef, RunDetails, StageDependencyRef, StageSummary};
use crate::domain::{AttentionKind, DependencyOutcome, RunStatus, StageKind, StageStatus};

use super::state::{Overlay, RetryRouteChoice, Screen, TuiState, UiMessageKind};
use super::{format, markdown, mascot, theme};

const MIN_WIDTH: u16 = 50;
const MIN_HEIGHT: u16 = 10;

/// Below this width the header drops the run's workflow and repository and
/// keeps only product identity and top-level state.
const HEADER_IDENTITY_WIDTH: u16 = 96;

pub(crate) fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new("Terminal too small — resize to continue\n\nq quit/detach")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title(" POLYCODE ")),
            area,
        );
        return;
    }
    // Footer grows one row for a notification; key hints are never replaced.
    let footer_height = if state.message.is_some() { 3 } else { 2 };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(footer_height),
        ])
        .split(area);
    render_header(frame, rows[0], state);
    match state.screen {
        Screen::Runs => render_runs(frame, rows[1], state),
        Screen::RunDetail => render_detail(frame, rows[1], state),
        Screen::Artifact => render_artifact(frame, rows[1], state),
        Screen::Logs => render_logs(frame, rows[1], state),
        Screen::Diff => render_diff(frame, rows[1], state),
        Screen::NewRun => render_new_run(frame, rows[1], state),
    }
    render_footer(frame, rows[2], state);
    if let Some(overlay) = state.overlay {
        render_overlay(frame, area, state, overlay);
    }
}

/// Product signature on the left, the run's identity and top-level state on
/// the right. Nothing here repeats the hero: the header speaks for the run,
/// the hero speaks for the stage.
fn render_header(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let screen = match state.screen {
        Screen::Runs => "RUNS",
        Screen::RunDetail => "MISSION DECK",
        Screen::Artifact => "ARTIFACT",
        Screen::Logs => "LOGS",
        Screen::Diff => "DIFF",
        Screen::NewRun => "NEW RUN",
    };
    let mut left = vec![
        Span::styled(
            "POLYCODE",
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ▌ ", theme::muted()),
        Span::styled(screen, Style::default().add_modifier(Modifier::BOLD)),
    ];
    if let Some(busy) = state.busy_label() {
        left.push(Span::styled(
            format!("  {busy}…"),
            Style::default().fg(theme::accent()),
        ));
    }
    let right = state
        .details
        .as_ref()
        .filter(|_| state.screen != Screen::NewRun)
        .map_or_else(Vec::new, |details| header_identity(details, area.width));
    frame.render_widget(
        Paragraph::new(theme::spread(left, right, area.width)).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(theme::muted()),
        ),
        area,
    );
}

/// Run identity for the header: the run id itself first — the handle every
/// CLI command takes — then workflow and repository, then the canonical run
/// state and its wall-clock elapsed. Narrow terminals keep the state and drop
/// the identity.
fn header_identity(details: &RunDetails, width: u16) -> Vec<Span<'static>> {
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    let mut spans = Vec::new();
    if width >= HEADER_IDENTITY_WIDTH {
        let mut identity = details.id.to_string();
        identity.push_str(" · ");
        identity.push_str(&enum_text(details.workflow));
        if let Some(path) = details.repository.as_deref() {
            identity.push_str(" · ");
            identity.push_str(&format::repository_name(path));
        }
        identity.push_str(" · ");
        spans.push(Span::styled(identity, theme::muted()));
    }
    spans.push(run_visual(details.status).badge_bold());
    if let Some(span) = format::elapsed(details.started_at, details.finished_at, now) {
        spans.push(Span::styled(
            format!(" · {}", format::format_duration(span)),
            theme::muted(),
        ));
    }
    spans
}

fn render_runs(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    if state.runs.is_empty() {
        let mut lines = Vec::new();
        // Decoration yields to content: the mascot appears only when the
        // empty state has room for it.
        if area.width >= 60 && area.height >= 14 {
            lines.extend(mascot::mascot_lines(
                mascot::MascotState::Idle,
                None,
                state.motion_frame(),
            ));
            lines.push(Line::from(""));
        }
        if state.hidden_count > 0 {
            // Not actually empty — everything is hidden; say so instead of
            // pretending the operator never ran anything.
            let plural = if state.hidden_count == 1 {
                "run"
            } else {
                "runs"
            };
            lines.push(Line::from(Span::styled(
                format!("{} hidden {plural}.", state.hidden_count),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(theme::action(
                "H",
                "Show hidden runs",
                theme::accent(),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "No runs yet.",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(theme::action(
                "n",
                "Start your first run",
                theme::accent(),
            )));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .alignment(Alignment::Center)
                .block(Block::default().padding(Padding::new(2, 2, 1, 0))),
            area,
        );
        return;
    }
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(area);
    let list_width = columns[0].width.saturating_sub(3);
    let items = state.runs.iter().enumerate().map(|(index, run)| {
        ListItem::new(run_row(
            run,
            index == state.selected_run_index,
            list_width,
            now,
        ))
    });
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(theme::muted())
                .padding(Padding::new(1, 1, 1, 0)),
        ),
        columns[0],
    );
    if let Some(details) = state.details.as_ref() {
        render_run_overview(frame, columns[1], details, now);
    } else {
        frame.render_widget(
            Paragraph::new("Loading selected run…")
                .style(theme::muted())
                .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
            columns[1],
        );
    }
}

/// One run row: cursor, state glyph, task, then quiet workflow and age. The
/// run's ULID is never the row's identity; technical detail keeps it.
fn run_row(
    run: &crate::app::RunListItem,
    selected: bool,
    width: u16,
    now: DateTime<Utc>,
) -> Line<'static> {
    let age = format::elapsed(Some(run.updated_at), None, now)
        .map(|span| format!("{} ago", format::format_duration(span)))
        .unwrap_or_default();
    // Only ever visible while the list is showing hidden runs.
    let hidden_mark = if run.hidden { "hidden  " } else { "" };
    let meta = format!("{hidden_mark}{}  {age}", enum_text(run.workflow));
    // Budget: cursor, glyph, the meta column, and a gap wide enough that the
    // ellipsis can never push the row past the rail.
    let task_width = (width as usize).saturating_sub(meta.chars().count() + 7);
    let task = format::truncate_title(&run.task_summary, task_width.max(8));
    let left = vec![
        Span::styled(
            if selected { "▸ " } else { "  " },
            Style::default().fg(theme::accent()),
        ),
        run_visual(run.status).glyph(),
        Span::styled(
            task,
            if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                theme::text()
            },
        ),
    ];
    theme::spread(left, vec![Span::styled(meta, theme::muted())], width)
}

/// The Runs screen's right column: enough of the selected run to decide
/// whether to open it, in the same visual language as the Mission Deck.
fn render_run_overview(
    frame: &mut Frame<'_>,
    area: Rect,
    details: &RunDetails,
    now: DateTime<Utc>,
) {
    let width = area.width.saturating_sub(3);
    let mut lines = vec![
        Line::from(Span::styled(
            format::truncate_title(
                details
                    .task
                    .as_deref()
                    .unwrap_or("<legacy input unavailable>"),
                width.saturating_sub(1) as usize,
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            details.repository.as_deref().map_or_else(
                || enum_text(details.workflow),
                |path| {
                    format!(
                        "{} · {}",
                        enum_text(details.workflow),
                        format::repository_name(path)
                    )
                },
            ),
            theme::muted(),
        )),
        Line::from(""),
        theme::section("STAGES"),
    ];
    for stage in &details.stages {
        lines.push(pipeline_line(stage, false, width, now));
    }
    // A failed run says why here, before the user has to open it: the
    // blocking stage's reason is the one fact that decides what to do next.
    if details.status == RunStatus::Failed
        && let Some(reason) = details.failure_reason.as_deref()
    {
        lines.extend(failure_reason_lines(reason));
    }
    if details.status == RunStatus::Completed
        && details.workflow != crate::domain::WorkflowKind::Review
    {
        lines.push(Line::from(""));
        lines.push(Line::from(theme::chip("READY TO REVIEW", theme::success())));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("[Enter]", Style::default().fg(theme::accent())),
            Span::styled(" Open the mission deck to review", theme::text()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("run {}", details.id),
        theme::muted(),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
        area,
    );
}

fn render_detail(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let Some(details) = state.details.as_ref() else {
        frame.render_widget(
            Paragraph::new("Run unavailable")
                .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
            area,
        );
        return;
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(area);
    // The status strip yields before the pipeline does: a rail too short for
    // both spends every row on the stages themselves.
    let rail = if columns[0].height > STATUS_HEIGHT + 10 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(STATUS_HEIGHT)])
            .split(columns[0]);
        render_status(frame, rows[1], details);
        rows[0]
    } else {
        columns[0]
    };
    render_pipeline(frame, rail, state, details);
    if state.technical {
        render_technical(frame, columns[1], state, details);
    } else {
        render_hero(frame, columns[1], state, details);
    }
}

/// Rows the status strip needs: a rule, its section label, and two sentences.
const STATUS_HEIGHT: u16 = 5;

/// Bottom of the rail: the run's actual state in one or two plain sentences.
/// Typed and state-driven like `activity_message` — never model prose.
fn render_status(frame: &mut Frame<'_>, area: Rect, details: &RunDetails) {
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    let width = area.width.saturating_sub(3);
    let mut lines = vec![theme::centered_rule(width), theme::section("STATUS")];
    lines.extend(
        status_sentences(details, now, width)
            .into_iter()
            .map(|sentence| Line::from(Span::styled(sentence, theme::text()))),
    );
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(theme::muted())
                .padding(Padding::new(1, 1, 0, 0)),
        ),
        area,
    );
}

/// A char budget for `format::truncate_title` that leaves room for the
/// ellipsis it appends on top of whatever budget it is given: passing a hard
/// column width straight through would let a truncated line land one column
/// past it, which Ratatui then wraps — pushing whatever follows down a row.
/// Char count, not true display width, matching every other width budget in
/// this file (`span_width`, the footer's navigation-hint fitting): this
/// codebase has no display-width-aware helper, and every line it truncates
/// this way is already assumed to render as one column per `char`.
fn ellipsis_budget(width: usize) -> usize {
    width.saturating_sub(1).max(1)
}

/// The strip's `RunStatus::Failed` sentence for the blocking failed stage.
///
/// Bounds the *composed* sentence to `width`, not just the reason: at the
/// narrowest supported terminal (the 50-column minimum leaves the rail only
/// about 16 columns) even the bare `"<Stage> failed: "` prefix can already
/// exceed the available width — "Implementation failed: " alone is 23
/// characters. Truncating only the reason to whatever budget was left after
/// subtracting the prefix's length still let an oversized prefix push the
/// whole line past `width` once the reason and its ellipsis were appended.
/// When there is not even room for the prefix plus one character of reason,
/// the reason is dropped for the reasonless generic sentence instead of a
/// truncation that would land mid-word or mid-prefix.
fn failed_stage_sentence(
    stage_kind: StageKind,
    failure_reason: Option<&str>,
    width: u16,
) -> String {
    let title = stage_title(stage_kind);
    let Some(reason) = failure_reason else {
        return format!("{title} failed — its logs say why.");
    };
    let prefix = format!("{title} failed: ");
    let composed = format!("{prefix}{reason}");
    let width = width as usize;
    if composed.chars().count() <= width {
        return composed;
    }
    let budget = ellipsis_budget(width);
    if prefix.chars().count() >= budget {
        return format!("{title} failed — its logs say why.");
    }
    format::truncate_title(&composed, budget)
}

/// What the run is doing right now, then how far along it is. The first
/// sentence follows the run's canonical status; the second counts stages, so
/// both stay facts the domain already asserted. `width` bounds the failure
/// reason: the strip is a fixed [`STATUS_HEIGHT`] rows, so a long reason must
/// be cut to fit its line rather than wrap and push the stage-count sentence
/// out of the box.
fn status_sentences(details: &RunDetails, now: DateTime<Utc>, width: u16) -> Vec<String> {
    let running = details
        .stages
        .iter()
        .find(|stage| stage.status == StageStatus::Running);
    let first = match details.status {
        RunStatus::Running => running.map_or_else(
            || "Run is moving between stages.".to_owned(),
            |stage| {
                format::elapsed(stage.started_at, None, now).map_or_else(
                    || format!("{} is running.", stage_title(stage.kind)),
                    |span| {
                        format!(
                            "{} has been running for {}.",
                            stage_title(stage.kind),
                            format::format_duration(span)
                        )
                    },
                )
            },
        ),
        RunStatus::NeedsUser => "Waiting on you to resolve a request.".to_owned(),
        RunStatus::Completed => completed_sentence(details),
        RunStatus::Applied => "Changes applied to the repository.".to_owned(),
        RunStatus::Discarded => "Run discarded.".to_owned(),
        // The blocking stage, never merely the first failed one in workflow
        // order: an optional dependency can fail without stopping the run,
        // so a stage that failed on the way to the real, completion-blocking
        // failure elsewhere must not be mistaken for the cause.
        RunStatus::Failed => details
            .stages
            .iter()
            .find(|stage| stage.status == StageStatus::Failed && stage.blocking)
            .map_or_else(
                || "Run failed.".to_owned(),
                |stage| failed_stage_sentence(stage.kind, stage.failure_reason.as_deref(), width),
            ),
        RunStatus::Paused | RunStatus::Interrupted => {
            "Run suspended — resume when ready.".to_owned()
        }
        RunStatus::Created | RunStatus::Preparing | RunStatus::Ready => {
            "Run has not started yet.".to_owned()
        }
    };
    let completed = details
        .stages
        .iter()
        .filter(|stage| stage.status == StageStatus::Completed)
        .count();
    let failed = failed_stage_titles(details).len();
    vec![
        first,
        if failed == 0 {
            format!("{completed} of {} stages complete.", details.stages.len())
        } else {
            format!(
                "{completed} of {} stages complete, {failed} failed.",
                details.stages.len()
            )
        },
    ]
}

/// The titles of every failed stage, in workflow order.
fn failed_stage_titles(details: &RunDetails) -> Vec<&'static str> {
    details
        .stages
        .iter()
        .filter(|stage| stage.status == StageStatus::Failed)
        .map(|stage| stage_title(stage.kind))
        .collect()
}

/// A run can complete with an optional stage failed on the way — the
/// workflow allows it, and the decision may well have ruled over the gap.
/// Saying only "run complete" would hide that, so the sentence names the
/// failure and the fact that the decision still concluded. What the decision
/// *ruled* stays out of it: the verdict is prose the result panel quotes
/// from the decision's own artifact, never something this strip restates.
fn completed_sentence(details: &RunDetails) -> String {
    let failed = failed_stage_titles(details);
    if failed.is_empty() {
        return "Run complete — the result is ready to review.".to_owned();
    }
    let failed = failed.join(" and ");
    let decided = details
        .stages
        .iter()
        .any(|stage| stage.kind == StageKind::Decision && stage.status == StageStatus::Completed);
    if decided {
        format!(
            "Run complete — {failed} failed, but the decision was still reached. Its verdict is ready to review."
        )
    } else {
        format!("Run complete — the result is ready to review, though {failed} failed on the way.")
    }
}

/// Left rail: the run's task and its stages in workflow order with their
/// semantic durations. One vertical rule separates the rail from the hero —
/// no boxes.
fn render_pipeline(frame: &mut Frame<'_>, area: Rect, state: &TuiState, details: &RunDetails) {
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    let width = area.width.saturating_sub(3);
    let mut lines = vec![
        Line::from(Span::styled(
            format::truncate_title(
                details.task.as_deref().unwrap_or("<legacy input>"),
                width.saturating_sub(1) as usize,
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        theme::section("PIPELINE"),
    ];
    // Connector segments cost one row per gap; they are the first thing to
    // go when the rail is short.
    let stage_rows = details.stages.len() * 2 - details.stages.len().min(1);
    let connectors = area.height as usize > stage_rows + 6;
    for (index, stage) in details.stages.iter().enumerate() {
        if connectors && index > 0 {
            lines.push(Line::from(Span::styled("  │", theme::muted())));
        }
        lines.push(pipeline_line(
            stage,
            index == state.selected_stage_index,
            width,
            now,
        ));
    }
    // A booked fix is work the operator has already decided on, so the rail
    // shows it where that work will appear. Muted and unselectable: it is an
    // intention about this run, not yet a stage of it.
    if state.fix_is_booked() {
        if connectors {
            lines.push(Line::from(Span::styled("  ┆", theme::muted())));
        }
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("◇ ", theme::muted()),
            Span::styled("Fix (booked)", theme::muted()),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(theme::muted())
                .padding(Padding::new(1, 1, 1, 0)),
        ),
        area,
    );
}

/// One rail row. The cursor marks what is *selected*; the glyph and its color
/// carry what the stage is actually *doing* — the two are independent, since
/// the user can read a finished stage while another one runs.
fn pipeline_line(
    stage: &StageSummary,
    selected: bool,
    width: u16,
    now: DateTime<Utc>,
) -> Line<'static> {
    let name = stage_title(stage.kind);
    let mut duration = format::elapsed(stage.started_at, stage.finished_at, now)
        .map(format::format_duration)
        .unwrap_or_default();
    // Narrow rails give up the duration column rather than clipping either it
    // or the stage name.
    if 5 + name.chars().count() + duration.chars().count() > width as usize {
        duration.clear();
    }
    let left = vec![
        Span::styled(
            if selected { "▸ " } else { "  " },
            Style::default().fg(theme::accent()),
        ),
        stage_visual(stage.status).glyph(),
        Span::styled(name, stage_name_style(stage.status, selected)),
    ];
    theme::spread(left, vec![Span::styled(duration, theme::muted())], width)
}

/// Completed work stays calm and pending work recedes, so the live stage is
/// the only row with full contrast.
fn stage_name_style(status: StageStatus, selected: bool) -> Style {
    let base = match status {
        StageStatus::Running => Style::default().add_modifier(Modifier::BOLD),
        StageStatus::NeedsUser => Style::default()
            .fg(theme::attention())
            .add_modifier(Modifier::BOLD),
        StageStatus::Failed => Style::default().fg(theme::danger()),
        StageStatus::Pending | StageStatus::Ready | StageStatus::Skipped => theme::muted(),
        StageStatus::Completed | StageStatus::Paused | StageStatus::Interrupted => theme::text(),
    };
    if selected {
        base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        base
    }
}

/// Right panel, operational view: what is happening, for how long, on which
/// runtime, what needs the user, what came out, and what can be done next.
fn render_hero(frame: &mut Frame<'_>, area: Rect, state: &TuiState, details: &RunDetails) {
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    let width = area.width.saturating_sub(3);
    let Some(selected) = details.stages.get(state.selected_stage_index) else {
        frame.render_widget(
            Paragraph::new("No stage selected")
                .style(theme::muted())
                .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
            area,
        );
        return;
    };
    let applyable = state.run_is_applyable();
    let fix = FixOffer::of(state);
    let continuable = state.run_can_be_continued();
    let mut lines = if applyable {
        completed_hero(details, width, now)
    } else {
        stage_hero(selected, width, now)
    };
    // Attention outranks every remaining section, runtime included.
    if let Some(attention) = details.attention.first() {
        lines.push(Line::from(""));
        lines.push(Line::from(theme::chip(
            "⚠ ACTION REQUIRED",
            theme::attention(),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            attention.summary.clone(),
            theme::text(),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(theme::action(
            "u",
            "Review and resolve",
            theme::attention(),
        )));
    }
    if applyable {
        lines.push(Line::from(""));
        lines.push(Line::from(theme::chip("READY TO REVIEW", theme::success())));
        lines.push(Line::from(""));
        lines.extend(hero_actions(applyable, fix, continuable, selected.status));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        runtime_summary(selected),
        theme::muted(),
    )));
    if details.attention.is_empty() && !applyable {
        if let Some(reason) = failed_stage_reason(selected) {
            lines.extend(failure_reason_lines(reason));
        } else if let Some(activity) = activity_message(selected) {
            lines.push(Line::from(Span::styled(activity, theme::text())));
        }
    }
    lines.push(Line::from(""));
    // After the pivot the panel speaks for the run, so the result section
    // says which stage's artifact it is offering.
    lines.push(if applyable {
        theme::section(&format!(
            "RESULT · {}",
            stage_title(selected.kind).to_uppercase()
        ))
    } else {
        theme::section("RESULT")
    });
    lines.extend(result_lines(state, selected, width));
    lines.push(Line::from(""));
    lines.push(theme::section("RESOURCES"));
    lines.extend(
        resource_lines(details)
            .into_iter()
            .map(|line| Line::from(Span::styled(line, theme::text()))),
    );
    if !applyable {
        let actions = hero_actions(applyable, fix, continuable, selected.status);
        if actions
            .iter()
            .all(|line| span_width(&line.spans) <= width as usize)
        {
            lines.push(Line::from(""));
            lines.extend(actions);
        }
    }
    seat_mascot(&mut lines, area, width, state, details, selected);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
        area,
    );
}

/// POD's seat: one blank row, a short rule, another blank row, then POD —
/// under the panel that narrates the stage POD is acting out. Decoration
/// yields first: any doubt about the room, and the seat stays empty. Rows
/// are counted post-wrap, since the hero wraps and a quoted headline can
/// spend more rows than it has lines.
fn seat_mascot(
    lines: &mut Vec<Line<'static>>,
    area: Rect,
    width: u16,
    state: &TuiState,
    details: &RunDetails,
    selected: &StageSummary,
) {
    let seat = mascot::MASCOT_HEIGHT as usize + 3;
    let wrapped_rows: usize = lines
        .iter()
        .map(|line| {
            span_width(&line.spans)
                .div_ceil((width as usize).max(1))
                .max(1)
        })
        .sum();
    let inner_height = area.height.saturating_sub(1) as usize;
    if area.width >= mascot::MASCOT_WIDTH + 20 && inner_height > wrapped_rows + seat {
        lines.push(Line::from(""));
        lines.push(theme::centered_rule(width));
        lines.push(Line::from(""));
        lines.extend(mascot::mascot_lines(
            mascot::mascot_state(Some(details.status), Some(selected.status)),
            Some(mascot::mascot_activity(selected.kind)),
            state.motion_frame(),
        ));
    }
}

/// The hero's opening statement: which stage, in what state, for how long.
fn stage_hero(stage: &StageSummary, width: u16, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let clock = format::elapsed(stage.started_at, stage.finished_at, now)
        .map(hero_clock)
        .unwrap_or_default();
    vec![
        theme::spread(
            vec![Span::styled(
                stage_title(stage.kind).to_uppercase(),
                Style::default().add_modifier(Modifier::BOLD),
            )],
            vec![Span::styled(
                clock,
                Style::default().add_modifier(Modifier::BOLD),
            )],
            width,
        ),
        Line::from(stage_visual(stage.status).badge_bold()),
    ]
}

/// Once the run is applyable the panel stops monitoring and starts offering
/// review.
fn completed_hero(details: &RunDetails, width: u16, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let clock = format::elapsed(details.started_at, details.finished_at, now)
        .map(hero_clock)
        .unwrap_or_default();
    let completed = details
        .stages
        .iter()
        .filter(|stage| stage.status == StageStatus::Completed)
        .count();
    vec![
        theme::spread(
            vec![Span::styled(
                "RUN COMPLETE",
                Style::default().add_modifier(Modifier::BOLD),
            )],
            vec![Span::styled(
                clock,
                Style::default().add_modifier(Modifier::BOLD),
            )],
            width,
        ),
        Line::from({
            let mut spans = vec![Span::styled(
                format!("✓ {completed} of {} stages completed", details.stages.len()),
                Style::default()
                    .fg(theme::success())
                    .add_modifier(Modifier::BOLD),
            )];
            let failed = failed_stage_titles(details);
            if !failed.is_empty() {
                spans.push(Span::styled(
                    format!(" · {} failed", failed.join(" and ")),
                    Style::default()
                        .fg(theme::danger())
                        .add_modifier(Modifier::BOLD),
                ));
            }
            spans
        }),
    ]
}

/// The prominent figure. A span the clock cannot express honestly reads as a
/// span, never as a fabricated `00:00`.
fn hero_clock(span: chrono::TimeDelta) -> String {
    if span.num_seconds() < 1 {
        format::format_duration(span)
    } else {
        format::format_clock(span)
    }
}

/// Typed, state-driven activity text. Never inferred from logs or model prose,
/// and never repeating an action the actions row already offers. A completed
/// stage says nothing: its badge already reads COMPLETED, and the result
/// section below speaks for what it produced.
fn activity_message(stage: &StageSummary) -> Option<String> {
    match stage.status {
        StageStatus::Running => Some("Agent is working…".to_owned()),
        StageStatus::Pending | StageStatus::Ready => Some(waiting_message(stage)),
        StageStatus::Completed => None,
        StageStatus::Failed => Some("The provider ended this stage before it completed".to_owned()),
        StageStatus::Paused | StageStatus::Interrupted => Some("Stage suspended".to_owned()),
        StageStatus::NeedsUser => Some("Waiting on you".to_owned()),
        StageStatus::Skipped => Some("Stage skipped by the workflow".to_owned()),
    }
}

/// Specific reason a Pending/Ready stage isn't running, one line. A required
/// dependency having failed or been skipped outranks the others: this stage
/// is about to be skipped in turn, which matters more than what it was
/// otherwise waiting on. Degraded (optional deps failed/skipped) is
/// informational and only appended once there is something to wait on.
fn waiting_message(stage: &StageSummary) -> String {
    use std::fmt::Write as _;

    const FALLBACK: &str = "Waiting for the previous stage";
    let Some(waiting) = &stage.waiting else {
        return FALLBACK.to_owned();
    };
    if !waiting.blocked_by.is_empty() {
        return blocked_message(&waiting.blocked_by);
    }
    if waiting.waiting_on.is_empty() {
        return FALLBACK.to_owned();
    }
    let mut message = format!("Waiting on: {}", dependency_names(&waiting.waiting_on));
    if !waiting.degraded.is_empty() {
        let _ = write!(
            message,
            " (degraded: {})",
            dependency_names(&waiting.degraded)
        );
    }
    message
}

/// Human titles for a bucket of dependency stages, comma-joined.
fn dependency_names(dependencies: &[StageDependencyRef]) -> String {
    dependencies
        .iter()
        .map(|dependency| stage_title(dependency.kind))
        .collect::<Vec<_>>()
        .join(", ")
}

/// "Blocked: X failed, Y was skipped" — each blocked dependency states its
/// own outcome rather than assuming every one of them failed, since a
/// required dependency can also land here by being skipped in turn.
fn blocked_message(blocked: &[BlockedDependencyRef]) -> String {
    let parts = blocked
        .iter()
        .map(|dependency| {
            format!(
                "{} {}",
                stage_title(dependency.kind),
                outcome_phrase(dependency.outcome)
            )
        })
        .collect::<Vec<_>>();
    format!("Blocked: {}", parts.join(", "))
}

const fn outcome_phrase(outcome: DependencyOutcome) -> &'static str {
    match outcome {
        DependencyOutcome::Failed => "failed",
        DependencyOutcome::Skipped => "was skipped",
    }
}

/// A failed stage's own reason, when it has one. The reason outranks the
/// generic [`activity_message`] wherever it is shown: it is the one thing
/// that says *why*, not just *that*. Every other status, Pending/Ready
/// included, falls through to `activity_message`, which derives its own
/// waiting/blocked-on text from `stage.waiting`.
fn failed_stage_reason(stage: &StageSummary) -> Option<&str> {
    (stage.status == StageStatus::Failed)
        .then_some(stage.failure_reason.as_deref())
        .flatten()
}

/// The failure block shared by the hero and the Runs-screen overview: a
/// section header, then the whole reason. Deliberately *not* cut to one
/// line the way the status strip cuts it — both panels are wrapped
/// paragraphs with room to spare, and the reason was already capped at
/// [`crate::app::query`]'s 200-character limit and collapsed to one line
/// of sanitized text, so at worst it spends two or three rows. Reading
/// the message is the whole point of looking at a failed run; sending the
/// user to the raw logs for it was the complaint this fixes.
fn failure_reason_lines(reason: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        theme::section("WHY IT FAILED"),
        Line::from(Span::styled(reason.to_owned(), theme::text())),
    ]
}

/// Operational runtime line: which agent is doing the work, at what effort.
/// Configured and actual targets are only both shown when they disagree.
fn runtime_summary(stage: &StageSummary) -> String {
    let configured = format!(
        "{} · {}{}",
        stage.configured_provider,
        stage
            .configured_model
            .as_deref()
            .unwrap_or("native default"),
        if stage.route_overridden {
            " (override)"
        } else {
            ""
        }
    );
    let effort = stage.requested_effort.label();
    // Drift is only meaningful when the runtime departed from an explicit
    // configuration. Confirming a concrete model where the route asked for
    // the native default is normal operation, not a mismatch.
    let provider_drift = stage
        .actual_provider
        .as_deref()
        .is_some_and(|actual| actual != stage.configured_provider);
    let model_drift = matches!(
        (stage.configured_model.as_deref(), stage.actual_model.as_deref()),
        (Some(configured), Some(actual)) if configured != actual
    );
    if provider_drift || model_drift {
        return format!(
            "{configured} configured → {} · {} actual · {effort} effort",
            stage.actual_provider.as_deref().unwrap_or("unknown"),
            stage.actual_model.as_deref().unwrap_or("unconfirmed")
        );
    }
    // Once the runtime confirms a concrete model, that is what the operator
    // wants to read instead of "native default".
    match stage.actual_model.as_deref() {
        Some(model) if stage.configured_model.is_none() => {
            format!("{} · {model} · {effort} effort", stage.configured_provider)
        }
        _ => format!("{configured} · {effort} effort"),
    }
}

/// How much of the artifact's opening line the panel quotes: two wrapped
/// rows. Past that the line stops being a glance and starts being reading,
/// which is what opening the artifact is for.
const HEADLINE_ROWS: usize = 2;

fn result_lines(state: &TuiState, selected: &StageSummary, width: u16) -> Vec<Line<'static>> {
    if state.stages_with_artifacts.contains(&selected.id) {
        let mut lines = vec![Line::from(Span::styled(
            format!("✓ {}", result_statement(selected.kind)),
            Style::default()
                .fg(theme::success())
                .add_modifier(Modifier::BOLD),
        ))];
        lines.extend(headline_lines(state, selected, width));
        lines.push(Line::from(theme::action(
            "Enter/o",
            "Open result",
            theme::success(),
        )));
        return lines;
    }
    // Expected absence stays informational; only a completed stage without an
    // artifact is a real problem, and opening it reports that as an error.
    let text = match selected.status {
        StageStatus::Running => "Not available yet — stage is still running",
        StageStatus::Pending | StageStatus::Ready => "Not available yet — stage has not started",
        StageStatus::Failed => "No completed result",
        _ => "No verified artifact",
    };
    vec![Line::from(Span::styled(text, theme::muted()))]
}

/// The result line in the stage's own terms: what kind of outcome arrived,
/// in a few words. It names the shape of the result — a verdict, a plan, a
/// change — and leaves the judgment itself to the quoted bottom line below,
/// because the panel never states a verdict the artifact did not state.
const fn result_statement(kind: StageKind) -> &'static str {
    match kind {
        StageKind::Research => "Research findings ready",
        StageKind::Architecture => "Architecture plan ready",
        StageKind::Implementation => "Implementation ready",
        StageKind::Simplification => "Simplification ready",
        StageKind::CodeQualityReview => "Quality review verdict in",
        StageKind::SpecReview => "Spec review verdict in",
        StageKind::Review | StageKind::IndependentReview => "Review verdict in",
        StageKind::DeepAnalysis => "Analysis ready",
        StageKind::Synthesis => "Synthesis ready",
        StageKind::Decision => "Decision reached",
        StageKind::Fix => "Fix ready",
        StageKind::FollowUp => "Follow-up ready",
        StageKind::Verify => "Verification result in",
    }
}

/// The artifact's opening line, quoted. Never a summary this panel wrote:
/// the words are the agent's, either from the `## Bottom line` the stage
/// contract asks for, or — for an artifact that predates the contract or
/// ignored it — from its first paragraph, which reads as the excerpt it is.
fn headline_lines(state: &TuiState, selected: &StageSummary, width: u16) -> Vec<Line<'static>> {
    let Some(headline) = state
        .headline
        .as_ref()
        .filter(|headline| headline.stage_id == selected.id)
    else {
        return Vec::new();
    };
    let Some(quoted) = headline.text.as_ref() else {
        return Vec::new();
    };
    let budget = (width as usize).max(24) * HEADLINE_ROWS;
    let style = if headline.contracted {
        theme::text()
    } else {
        theme::muted().add_modifier(Modifier::ITALIC)
    };
    vec![Line::from(Span::styled(
        format::truncate_title(quoted, budget),
        style,
    ))]
}

/// Provider-native units in human words, one line per runtime that reported
/// any. Never summed across runtimes and never presented as cost.
///
/// A run routes different roles to different runtimes, and those runtimes do
/// not report the same quantity under the name "input": one line each is what
/// the numbers actually support.
fn resource_lines(details: &RunDetails) -> Vec<String> {
    if details.usage.is_empty() {
        return vec!["No usage reported yet".to_owned()];
    }
    details
        .usage
        .providers()
        .map(|entry| provider_resources(&entry))
        .collect()
}

/// The selected stage's own usage, attributed to the runtime that actually
/// reported it. Falls back to the configured runtime only while nothing has
/// started, when there is no reported usage to misattribute anyway.
fn stage_usage(evidence: &crate::app::StageExecutionEvidence) -> crate::app::ProviderUsage {
    let provider = evidence
        .actual_provider
        .clone()
        .unwrap_or_else(|| evidence.configured_provider.clone());
    crate::app::ProviderUsage {
        accounting: crate::providers::input_accounting(&provider),
        provider,
        usage: evidence.usage,
    }
}

/// One runtime's reported units, written so no token is counted twice.
///
/// A runtime whose input total already contains its cache reads says so in
/// place, and its cache read is not repeated as if it were a further
/// quantity. A runtime that keeps them disjoint lists both.
fn provider_resources(entry: &crate::app::ProviderUsage) -> String {
    use std::fmt::Write as _;
    let mut line = format!(
        "{} · {} input",
        entry.provider,
        format::format_units(entry.usage.input_units)
    );
    let folded_cache_read = entry
        .input_contains_cache_reads()
        .then_some(entry.usage.cache_read_units)
        .flatten();
    if let Some(cached) = folded_cache_read {
        let _ = write!(line, " ({} of it cached)", format::format_units(cached));
    }
    let _ = write!(
        line,
        " · {} output",
        format::format_units(entry.usage.output_units)
    );
    for (label, value) in [
        (
            "cache read",
            if folded_cache_read.is_some() {
                None
            } else {
                entry.usage.cache_read_units
            },
        ),
        ("cache write", entry.usage.cache_write_units),
        ("reasoning output", entry.usage.reasoning_output_units),
    ] {
        if let Some(value) = value {
            let _ = write!(line, " · {} {label}", format::format_units(value));
        }
    }
    line
}

/// Actions offered by the panel, gated on canonical state: apply and discard
/// appear only for a run the workspace layer would accept.
/// What the interface may offer about a fix, resolved once so the panel and
/// the footer cannot disagree about it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FixOffer {
    /// No verdict to answer, or no way to answer one yet.
    Unavailable,
    /// The run has reached its verdict; the key starts the fix now.
    Now,
    /// The run is still working; the key books the fix against its verdict.
    Book,
    /// Already booked. The key cancels it.
    Booked,
}

impl FixOffer {
    fn of(state: &TuiState) -> Self {
        if state.run_can_be_fixed() {
            Self::Now
        } else if !state.run_can_book_a_fix() {
            Self::Unavailable
        } else if state.fix_is_booked() {
            Self::Booked
        } else {
            Self::Book
        }
    }

    const fn label(self) -> Option<&'static str> {
        match self {
            Self::Unavailable => None,
            Self::Now => Some("Fix it"),
            Self::Book => Some("Fix when done"),
            Self::Booked => Some("Fix booked — cancel"),
        }
    }
}

fn hero_actions(
    applyable: bool,
    fix: FixOffer,
    continuable: bool,
    status: StageStatus,
) -> Vec<Line<'static>> {
    let mut spans = Vec::new();
    if applyable {
        spans.extend(theme::action("d", "Review diff", theme::accent()));
        spans.push(Span::raw("   "));
        spans.extend(theme::action("a", "Apply changes", theme::success()));
        spans.push(Span::raw("   "));
        spans.extend(theme::action("P", "Pull request", theme::success()));
        if let Some(label) = fix.label() {
            spans.push(Span::raw("   "));
            spans.extend(theme::action("f", label, theme::attention()));
        }
        spans.extend(continue_action_spans(continuable));
        spans.push(Span::raw("   "));
        spans.extend(theme::action("X", "Discard", theme::danger()));
    } else if status == StageStatus::Failed {
        spans.extend(theme::action("l", "Logs", theme::accent()));
        spans.push(Span::raw("   "));
        spans.extend(theme::action("t", "Retry", theme::attention()));
        spans.push(Span::raw("   "));
        spans.extend(theme::action("d", "Diff", theme::accent()));
    } else {
        spans.extend(theme::action("o", "Result", theme::accent()));
        spans.push(Span::raw("   "));
        spans.extend(theme::action("l", "Logs", theme::accent()));
        spans.push(Span::raw("   "));
        spans.extend(theme::action("d", "Diff", theme::accent()));
        // A review reaches a verdict without ever becoming applyable, so this
        // is the only place its fix and continue cycle are ever offered.
        if let Some(label) = fix.label() {
            spans.push(Span::raw("   "));
            spans.extend(theme::action("f", label, theme::attention()));
        }
        spans.extend(continue_action_spans(continuable));
    }
    spans.push(Span::raw("   "));
    spans.extend(theme::action("i", "Details", theme::muted_color()));
    vec![Line::from(spans)]
}

/// The `[c]`/`[w]` pair, offered together: both answer a decision the same
/// way `[f]` Fix does, so whichever cycle the operator picks starts from the
/// same completed-and-decided run.
fn continue_action_spans(continuable: bool) -> Vec<Span<'static>> {
    if !continuable {
        return Vec::new();
    }
    let mut spans = vec![Span::raw("   ")];
    spans.extend(theme::action("c", "Continue", theme::attention()));
    spans.push(Span::raw("   "));
    spans.extend(theme::action("w", "Follow-ups", theme::attention()));
    spans
}

/// Human stage name for operational rows; technical mode keeps the raw
/// serialized kind.
const fn stage_title(kind: StageKind) -> &'static str {
    match kind {
        StageKind::Research => "Research",
        StageKind::Architecture => "Architecture",
        StageKind::Implementation => "Implementation",
        StageKind::Simplification => "Simplification",
        StageKind::CodeQualityReview => "Quality review",
        StageKind::SpecReview => "Spec review",
        StageKind::Review => "Review",
        StageKind::IndependentReview => "Independent review",
        StageKind::DeepAnalysis => "Deep analysis",
        StageKind::Synthesis => "Synthesis",
        StageKind::Decision => "Decision",
        StageKind::Fix => "Fix",
        StageKind::FollowUp => "Follow-up",
        StageKind::Verify => "Verify",
    }
}

/// One aligned `label  value` row inside a technical group.
fn technical_row(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<13}"), theme::muted()),
        Span::styled(value, theme::text()),
    ])
}

/// Right panel, technical view: every diagnostic the operational view hides,
/// grouped rather than listed.
#[allow(
    clippy::too_many_lines,
    reason = "one diagnostic panel keeps execution, runtime, evidence, workspace, and routing aligned"
)]
fn render_technical(frame: &mut Frame<'_>, area: Rect, state: &TuiState, details: &RunDetails) {
    let width = area.width.saturating_sub(3);
    let Some(selected) = details.stages.get(state.selected_stage_index) else {
        frame.render_widget(
            Paragraph::new("No stage selected")
                .style(theme::muted())
                .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
            area,
        );
        return;
    };
    let mut lines = vec![
        theme::spread(
            vec![Span::styled(
                "TECHNICAL",
                Style::default().add_modifier(Modifier::BOLD),
            )],
            vec![Span::styled(selected.id.to_string(), theme::muted())],
            width,
        ),
        Line::from(""),
        theme::section("EXECUTION"),
        technical_row("Stage", selected.id.to_string()),
        technical_row("Kind", enum_text(selected.kind)),
        technical_row("Role", enum_text(selected.role)),
        Line::from(vec![
            Span::styled(format!("  {:<13}", "Status"), theme::muted()),
            stage_visual(selected.status).badge(),
        ]),
        technical_row(
            "Process",
            selected
                .process_status
                .as_deref()
                .unwrap_or("unavailable")
                .to_owned(),
        ),
        technical_row("Effort", selected.requested_effort.label().to_owned()),
        Line::from(""),
        theme::section("RUNTIME"),
        technical_row(
            "Configured",
            format!(
                "{} / {}",
                selected.configured_provider,
                selected
                    .configured_model
                    .as_deref()
                    .unwrap_or("native default")
            ),
        ),
        technical_row(
            "Actual",
            format!(
                "{} / {}",
                selected.actual_provider.as_deref().unwrap_or("not started"),
                selected.actual_model.as_deref().unwrap_or("unconfirmed")
            ),
        ),
        technical_row(
            "Session",
            selected
                .provider_session_status
                .as_deref()
                .unwrap_or("unavailable")
                .to_owned(),
        ),
        technical_row(
            "Native",
            selected
                .native_session
                .as_deref()
                .map_or("unavailable", short_id)
                .to_owned(),
        ),
        Line::from(""),
        theme::section("RESOURCE EVIDENCE"),
    ];
    // Per-stage execution evidence: every row below is scoped to the selected
    // stage. Usage in particular is the stage's own, not the run's — one
    // stage runs on one runtime, so the units here need no disclaimer. The
    // run-wide, per-runtime breakdown belongs to the RESOURCES section.
    // Provider latency stays here and is never presented as the stage's
    // wall-clock elapsed time.
    if let Some(evidence) = state.evidence.as_ref() {
        lines.push(technical_row(
            "Usage",
            provider_resources(&stage_usage(evidence)),
        ));
        lines.push(technical_row(
            "Invocations",
            evidence.invocation_count.to_string(),
        ));
        lines.push(technical_row(
            "Latency",
            evidence.latency_ms.map_or_else(
                || "unavailable".to_owned(),
                |ms| format!("{ms} ms provider"),
            ),
        ));
        lines.push(technical_row(
            "Prompt",
            evidence.injected_prompt_bytes.map_or_else(
                || "unavailable".to_owned(),
                |bytes| format!("{bytes} injected bytes"),
            ),
        ));
        // Requested effort and observed effort are different facts. A
        // native-default request asks for nothing, so the level the runtime
        // then chose is visible only here, and only when it recorded one.
        lines.push(technical_row(
            "Effort",
            format!(
                "{} requested → {} observed",
                selected.requested_effort.label(),
                evidence.native_effort.as_deref().unwrap_or("unobserved")
            ),
        ));
        if let Some(version) = evidence.provider_cli_version.as_deref() {
            lines.push(technical_row("CLI", version.to_owned()));
        }
    }
    lines.extend([
        Line::from(""),
        theme::section("WORKSPACE"),
        technical_row(
            "State",
            details
                .workspace_status
                .map_or("unavailable".to_owned(), |status| {
                    format!("{status:?}").to_lowercase()
                }),
        ),
        technical_row(
            "Base commit",
            details
                .base_commit
                .as_deref()
                .unwrap_or("unavailable")
                .to_owned(),
        ),
        technical_row(
            "Repository",
            details
                .repository
                .as_deref()
                .map_or("unavailable".to_owned(), |path| path.display().to_string()),
        ),
        Line::from(""),
        theme::section("ROUTING"),
        technical_row("Run", details.id.to_string()),
        technical_row(
            "Profile",
            format!("{} ({})", details.profile, details.profile_version),
        ),
    ]);
    for route in &details.routes {
        // Role names run long enough to collide with an aligned value column,
        // so routes read as one sentence per line instead.
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", enum_text(route.role)), theme::text()),
            Span::styled(
                format!(
                    "→ {} / {} ({})",
                    route.configured_provider,
                    route
                        .configured_model
                        .as_deref()
                        .unwrap_or("native default"),
                    route.reason
                ),
                theme::muted(),
            ),
        ]));
    }
    lines.extend([
        Line::from(""),
        Line::from(theme::action("i", "operational view", theme::muted_color())),
    ]);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
        area,
    );
}

/// A viewer's own heading: what is being read, and in which mode. The screen
/// is already framed by the header and footer rules, so no box is added.
fn viewer_heading(title: String, meta: String, width: u16) -> Vec<Line<'static>> {
    vec![
        theme::spread(
            vec![Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            )],
            vec![Span::styled(meta, theme::muted())],
            width,
        ),
        theme::rule(width),
    ]
}

fn render_artifact(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let width = area.width.saturating_sub(3);
    let mut lines = state.artifact.as_ref().map_or_else(
        || viewer_heading("ARTIFACT".to_owned(), "unavailable".to_owned(), width),
        |artifact| {
            viewer_heading(
                stage_title_for(&artifact.summary.stage_id.to_string()),
                format!(
                    "attempt {} · {}",
                    artifact.summary.attempt,
                    if state.artifact_raw {
                        "raw · [m] rendered"
                    } else {
                        "rendered · [m] raw"
                    }
                ),
                width,
            )
        },
    );
    if let Some(artifact) = state.artifact.as_ref() {
        if state.artifact_raw {
            lines.extend(
                artifact
                    .text
                    .lines()
                    .map(|line| Line::from(format::viewer_line(line))),
            );
        } else {
            lines.extend(markdown::render_markdown(&artifact.text));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((state.scroll, 0))
            .wrap(Wrap { trim: false })
            .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
        area,
    );
}

/// Artifact headings read as the stage that produced them.
fn stage_title_for(stage_id: &str) -> String {
    stage_id.replace('_', " ").to_uppercase()
}

fn render_logs(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let width = area.width.saturating_sub(3);
    let mut lines = viewer_heading(
        "RAW OUTPUT".to_owned(),
        state.logs.as_ref().map_or_else(
            || "unavailable".to_owned(),
            |logs| {
                format!(
                    "{} · {} · read-only",
                    short_id(&logs.process_id.to_string()),
                    logs.process_status
                )
            },
        ),
        width,
    );
    if let Some(logs) = state.logs.as_ref() {
        for (label, stream) in [("STDOUT", &logs.stdout), ("STDERR", &logs.stderr)] {
            lines.push(Line::from(""));
            lines.push(theme::section(label));
            if stream.truncated {
                lines.push(Line::from(Span::styled("[tail truncated]", theme::muted())));
            }
            lines.extend(
                stream
                    .text
                    .lines()
                    .map(|line| Line::from(format::viewer_line(line))),
            );
        }
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((state.scroll, 0))
            .wrap(Wrap { trim: false })
            .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
        area,
    );
}

fn render_diff(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let width = area.width.saturating_sub(3);
    let mut lines = viewer_heading(
        "WORKSPACE DIFF".to_owned(),
        state.diff.as_ref().map_or_else(
            || "unavailable".to_owned(),
            |diff| format!("{} files · read-only", diff.changed_files.len()),
        ),
        width,
    );
    if let Some(diff) = state.diff.as_ref() {
        if diff.truncated {
            lines.push(Line::from(Span::styled(
                format!(
                    "[preview truncated at 2 MiB; total {} bytes]",
                    diff.total_bytes
                ),
                Style::default().fg(theme::attention()),
            )));
        }
        for line in diff.text.lines() {
            let style = if line.starts_with("+++") || line.starts_with("---") {
                Style::default().fg(theme::accent())
            } else if line.starts_with('+') {
                Style::default().fg(theme::success())
            } else if line.starts_with('-') {
                Style::default().fg(theme::danger())
            } else if line.starts_with("diff --git") || line.starts_with("@@") {
                theme::diff_hunk()
            } else {
                theme::text()
            };
            lines.push(Line::styled(format::viewer_line(line), style));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((state.scroll, 0))
            .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
        area,
    );
}

/// The entry point to the Mission Deck: the same field semantics, presented
/// as a briefing rather than a raw form.
fn render_new_run(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let form = &state.new_run;
    let width = area.width.saturating_sub(3);
    let workflow = enum_text(form.workflow);
    let task = field_display(&form.task, form.focus == 0);
    let repository = field_display(&form.repository, form.focus == 2);
    let mut lines = vec![
        Line::from(Span::styled(
            "START A RUN",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "One task, routed through specialist agents.",
            theme::muted(),
        )),
        theme::rule(width),
    ];
    for (label, value, focused) in [
        ("TASK", task.as_str(), form.focus == 0),
        ("WORKFLOW", workflow.as_str(), form.focus == 1),
        ("REPOSITORY", repository.as_str(), form.focus == 2),
        ("EXECUTION", form.execution.label(), form.focus == 3),
        ("EFFORT", form.effort.label(), form.focus == 4),
    ] {
        lines.push(Line::from(""));
        lines.push(theme::section(label));
        lines.push(Line::from(vec![
            Span::styled(
                if focused { "▸ " } else { "  " },
                Style::default().fg(theme::accent()),
            ),
            Span::styled(
                value.to_owned(),
                if focused {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    theme::text()
                },
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(theme::rule(width));
    lines.push(Line::from(""));
    let mut actions = theme::action("Enter", "Start run", theme::accent());
    actions.push(Span::raw("   "));
    actions.extend(theme::action("Esc", "Cancel", theme::muted_color()));
    lines.push(Line::from(actions));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().padding(Padding::new(2, 1, 1, 0))),
        area,
    );
}

/// The screen's contextual actions, gated on canonical state so the footer
/// never advertises something the domain would refuse. Apply and discard
/// appear only for an applyable run.
fn primary_actions(screen: Screen, state: &TuiState) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut push = |key: &str, label: &str, color: Color| {
        if !spans.is_empty() {
            spans.push(Span::raw("   "));
        }
        spans.extend(theme::action(key, label, color));
    };
    match screen {
        Screen::Runs => {
            push("Enter", "Open", theme::accent());
            push("n", "New run", theme::accent());
            if let Some(run) = state.runs.get(state.selected_run_index) {
                push(
                    "h",
                    if run.hidden { "Unhide" } else { "Hide" },
                    theme::muted_color(),
                );
            }
            if state.show_hidden {
                push("H", "Hide hidden", theme::muted_color());
            } else if state.hidden_count > 0 {
                push("H", "Show hidden", theme::muted_color());
            }
        }
        Screen::RunDetail => {
            let needs_user = state
                .details
                .as_ref()
                .is_some_and(|details| !details.attention.is_empty());
            let stage_status = state
                .details
                .as_ref()
                .and_then(|details| details.stages.get(state.selected_stage_index))
                .map(|stage| stage.status);
            let run_status = state.details.as_ref().map(|details| details.status);
            if needs_user {
                push("u", "Resolve attention", theme::attention());
                push("l", "Logs", theme::accent());
                push("s", "Stop", theme::attention());
            } else if state.run_is_applyable() {
                push("d", "Review diff", theme::accent());
                push("a", "Apply", theme::success());
                push("P", "Pull request", theme::success());
                if let Some(label) = FixOffer::of(state).label() {
                    push("f", label, theme::attention());
                }
                if state.run_can_be_continued() {
                    push("c", "Continue", theme::attention());
                    push("w", "Follow-ups", theme::attention());
                }
                push("X", "Discard", theme::danger());
            } else {
                push("o", "Result", theme::accent());
                push("l", "Logs", theme::accent());
                push("d", "Diff", theme::accent());
                if stage_status == Some(StageStatus::Failed) {
                    push("t", "Retry", theme::attention());
                }
                if let Some(label) = FixOffer::of(state).label() {
                    push("f", label, theme::attention());
                }
                if state.run_can_be_continued() {
                    push("c", "Continue", theme::attention());
                    push("w", "Follow-ups", theme::attention());
                }
                // A running run normally has its driver in this process and
                // needs no key; one nobody holds was left behind by a dead
                // instance, and resume is how it gets a driver again.
                let orphaned = run_status == Some(RunStatus::Running)
                    && state
                        .selected_run
                        .is_some_and(|run_id| !state.run_is_held(run_id));
                if matches!(
                    run_status,
                    Some(RunStatus::Paused | RunStatus::Interrupted | RunStatus::Ready)
                ) || orphaned
                {
                    push("r", "Resume", theme::attention());
                }
                if state.run_is_stoppable() {
                    push("s", "Stop", theme::attention());
                }
            }
            push(
                "i",
                if state.technical {
                    "Operational"
                } else {
                    "Details"
                },
                theme::muted_color(),
            );
        }
        Screen::Artifact => push("m", "Raw/rendered", theme::accent()),
        Screen::Logs | Screen::Diff => push("Esc", "Back", theme::accent()),
        Screen::NewRun => {
            push("Enter", "Start run", theme::accent());
            push("Esc", "Cancel", theme::muted_color());
        }
    }
    spans
}

/// Quiet navigation, in full and compact shapes. Navigation is what gets
/// dropped when the row is tight — never the contextual actions.
const fn navigation_hints(screen: Screen) -> (&'static str, &'static str) {
    match screen {
        Screen::Runs => ("↑↓ runs · n new · ? help · q quit/detach", "↑↓ · ? help"),
        Screen::RunDetail => (
            "↑↓ stages · Esc runs · ? help · q quit/detach",
            "↑↓ · Esc runs",
        ),
        Screen::Artifact | Screen::Logs | Screen::Diff => (
            "↑↓/PgUp/PgDn scroll · Esc run detail · ? help",
            "↑↓ scroll · Esc run detail",
        ),
        Screen::NewRun => (
            "Tab/Shift-Tab fields · ←→ choices/edit · ? help",
            "Tab fields",
        ),
    }
}

fn span_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| span.content.chars().count()).sum()
}

/// Footer row: contextual actions on the left, quiet navigation right-aligned
/// and dropped progressively as the terminal narrows.
fn footer_line(screen: Screen, state: &TuiState, width: u16) -> Line<'static> {
    let actions = primary_actions(screen, state);
    let (full, compact) = navigation_hints(screen);
    let used = span_width(&actions);
    let navigation = if used + full.chars().count() + 3 <= width as usize {
        full
    } else if used + compact.chars().count() + 3 <= width as usize {
        compact
    } else {
        ""
    };
    theme::spread(
        actions,
        vec![Span::styled(navigation, theme::muted())],
        width,
    )
}

fn message_presentation(kind: UiMessageKind) -> (&'static str, Style) {
    match kind {
        UiMessageKind::Info => ("ℹ", Style::default().fg(theme::accent())),
        UiMessageKind::Success => ("✓", Style::default().fg(theme::success())),
        UiMessageKind::Warning => ("⚠", Style::default().fg(theme::attention())),
        UiMessageKind::Error => (
            "✗",
            Style::default()
                .fg(theme::danger())
                .add_modifier(Modifier::BOLD),
        ),
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let mut lines = Vec::new();
    if let Some(message) = state.message.as_ref() {
        let (glyph, style) = message_presentation(message.kind);
        lines.push(theme::spread(
            vec![
                Span::styled(format!("{glyph} "), style),
                Span::styled(message.text.clone(), style),
            ],
            vec![Span::styled("x dismiss", theme::muted())],
            area.width,
        ));
    }
    lines.push(footer_line(state.screen, state, area.width));
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(theme::muted()),
        ),
        area,
    );
}

fn render_overlay(frame: &mut Frame<'_>, area: Rect, state: &TuiState, overlay: Overlay) {
    let popup = centered_rect(78, 70, area);
    frame.render_widget(Clear, popup);
    match overlay {
        Overlay::Help => frame.render_widget(
            Paragraph::new(
                "Global\n  ↑/↓ or j/k  navigate\n  Enter        open/confirm\n  Esc          back/close\n  n            new run\n  R            runs screen\n  x            dismiss notification\n  ?            help\n  q / Ctrl-C   quit/detach\n\nRun\n  Enter/o open selected stage result\n  r resume/recover\n  s stop (keeps the run and its work)\n  t retry selected failed stage (choose provider)\n  u resolve selected attention\n  l raw logs (read-only)\n  d workspace diff (read-only)\n  a apply (confirmation)\n  P pull request (push branch, confirmation)\n  X discard (confirmation)\n  f fix a completed run's decision\n  c continue a completed run with a new instruction\n  w work on a decision's Follow-ups\n\nRuns list\n  h hide/unhide selected run\n  H show/hide hidden runs\n\nArtifact viewer\n  m toggle raw/rendered Markdown",
            )
            .block(overlay_block(" Help · Esc closes ", theme::muted_color())),
            popup,
        ),
        Overlay::Attention => render_attention(frame, popup, state),
        Overlay::Update => render_update(frame, area, state),
        Overlay::ApplyConfirm => render_confirmation(frame, popup, state, Confirmation::Apply),
        Overlay::PublishConfirm => render_confirmation(frame, popup, state, Confirmation::Publish),
        Overlay::DiscardConfirm => render_confirmation(frame, popup, state, Confirmation::Discard),
        Overlay::Continue => render_continue(frame, popup, state),
        Overlay::FollowUps => render_follow_ups(frame, popup, state),
        Overlay::RetryRoute => render_retry_route(frame, popup, state),
    }
}

fn render_retry_route(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let selected = state
        .details
        .as_ref()
        .and_then(|details| details.stages.get(state.selected_stage_index));
    let mut lines = vec![
        Line::from(theme::chip("RETRY", theme::attention())),
        Line::from(""),
    ];
    if let Some(stage) = selected {
        lines.push(Line::from(vec![
            Span::styled(stage_title_for(stage.id.as_str()), theme::text()),
            Span::styled(
                format!(" · configured {}", stage.configured_provider),
                theme::muted(),
            ),
        ]));
        lines.push(Line::from(""));
    }
    for choice in RetryRouteChoice::ALL {
        let highlighted = state.retry_route_choice == choice;
        let label = match choice {
            RetryRouteChoice::Configured => selected.map_or_else(
                || "Configured provider".to_owned(),
                |stage| format!("Configured provider ({})", stage.configured_provider),
            ),
            RetryRouteChoice::Claude => "Claude (native default model)".to_owned(),
            RetryRouteChoice::Codex => "Codex (native default model)".to_owned(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                if highlighted { "  → " } else { "    " },
                Style::default().fg(theme::attention()),
            ),
            Span::styled(
                label,
                if highlighted {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    theme::muted()
                },
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Only this stage moves; the run's routing stays as configured.",
        theme::muted(),
    )));
    lines.push(Line::from(Span::styled(
        "↑/↓ choose · Enter retry · Esc cancel",
        theme::muted(),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(overlay_block(" Retry stage ", theme::attention())),
        area,
    );
}

/// Overlays are the one place a full border earns its keep: they float over
/// the deck and need their own edge.
fn overlay_block(title: &'static str, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(title)
        .title_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .padding(Padding::new(1, 1, 0, 0))
}

/// The update prompt: compact, Mission Deck-native, and never larger than it
/// needs to be. It states both versions, says what installing would mean, and
/// — when Polycode cannot install safely — says so instead of offering a
/// button that would lie.
fn render_update(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let Some(info) = state.update.as_ref() else {
        return;
    };
    let installable = state.update_is_installable();
    let mut lines = vec![
        Line::from(Span::styled(
            "UPDATE AVAILABLE",
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Polycode ", theme::text()),
            Span::styled(info.current_version.to_string(), theme::muted()),
            Span::styled(" → ", theme::muted()),
            Span::styled(
                info.available_version.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
    ];
    if installable {
        lines.push(Line::from(Span::styled("Install now?", theme::text())));
        lines.push(Line::from(Span::styled(
            "It applies when Polycode restarts.",
            theme::muted(),
        )));
        lines.push(Line::from(""));
        for (selected, label) in [
            (state.update_install_selected, "Yes"),
            (!state.update_install_selected, "No"),
        ] {
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "  → " } else { "    " },
                    Style::default().fg(theme::accent()),
                ),
                Span::styled(
                    label,
                    if selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        theme::muted()
                    },
                ),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled(
            state
                .update_install
                .map_or_else(String::new, |source| source.strategy().guidance()),
            theme::text(),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(theme::action(
            "Enter",
            "Continue",
            theme::accent(),
        )));
    }
    // The prompt is an aside, not a takeover: it occupies a compact band
    // rather than the whole screen.
    let popup = update_rect(area, &lines);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(overlay_block(" UPDATE ", theme::accent())),
        popup,
    );
}

/// A band wide enough for one version line and tall enough for its content
/// *after wrapping*, centered, and always inside the terminal.
fn update_rect(area: Rect, lines: &[Line<'_>]) -> Rect {
    let width = area.width.saturating_sub(4).clamp(20, 62);
    // Borders take two columns and the block pads one on each side, so the
    // band must be measured against the width the text actually wraps to.
    let inner = usize::from(width).saturating_sub(4).max(1);
    let rows: usize = lines
        .iter()
        .map(|line| span_width(&line.spans).div_ceil(inner).max(1))
        .sum();
    let height = u16::try_from(rows + 2).unwrap_or(u16::MAX).min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn render_attention(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let Some(details) = state.details.as_ref() else {
        return;
    };
    let mut lines = vec![
        Line::from(theme::chip("⚠ NEEDS YOU", theme::attention())),
        Line::from(""),
    ];
    for (index, attention) in details.attention.iter().enumerate() {
        let selected = index == state.attention_index;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▸ " } else { "  " },
                Style::default().fg(theme::attention()),
            ),
            Span::styled(
                format!("{} · {}", enum_text(attention.kind), attention.summary),
                if selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    theme::text()
                },
            ),
        ]));
    }
    lines.push(Line::from(""));
    let selected_kind = details
        .attention
        .get(state.attention_index)
        .map(|attention| attention.kind);
    if selected_kind == Some(AttentionKind::Permission) {
        lines.push(Line::from(Span::styled(
            "Permission request — Enter approves; a typed response continues without granting",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(format!(
            "Response (optional): {}",
            field_display(&state.attention_response, true)
        )));
        lines.push(Line::from(Span::styled(
            "↑/↓ select request · Enter approve/resolve · type to answer instead · Esc cancel",
            theme::muted(),
        )));
    } else {
        lines.push(Line::from(format!(
            "Response: {}",
            field_display(&state.attention_response, true)
        )));
        lines.push(Line::from(Span::styled(
            "↑/↓ select request · type response · Enter submit · Esc cancel",
            theme::muted(),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(overlay_block(" Attention ", theme::attention())),
        area,
    );
}

/// Single-field prompt for `[c]` Continue: an operator instruction that,
/// once submitted, becomes the follow-up stage's own task.
fn render_continue(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let lines = vec![
        Line::from(theme::chip("CONTINUE", theme::attention())),
        Line::from(""),
        Line::from("How should the agent continue?"),
        Line::from(""),
        Line::from(field_display(&state.continue_instruction, true)),
        Line::from(""),
        Line::from(Span::styled(
            "type instruction · Enter submit · Esc cancel",
            theme::muted(),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(overlay_block(" Continue ", theme::attention())),
        area,
    );
}

/// Two-option chooser for `[w]` Work on follow-ups: continue the operator's
/// own extracted text in this run, or hand it to a new run's composer.
/// Mirrors the update prompt's own up/down toggle.
fn render_follow_ups(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let mut lines = vec![
        Line::from(theme::chip("FOLLOW-UPS", theme::attention())),
        Line::from(""),
    ];
    if let Some(text) = state.follow_ups_text.as_ref() {
        for line in text.lines() {
            lines.push(Line::from(Span::styled(
                format::viewer_line(line),
                theme::text(),
            )));
        }
        lines.push(Line::from(""));
    }
    for (selected, label) in [
        (!state.follow_ups_as_new_run, "In this run"),
        (state.follow_ups_as_new_run, "As new run"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "  → " } else { "    " },
                Style::default().fg(theme::attention()),
            ),
            Span::styled(
                label,
                if selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    theme::muted()
                },
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑/↓ choose · Enter confirm · Esc cancel",
        theme::muted(),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(overlay_block(" Work on follow-ups ", theme::attention())),
        area,
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirmation {
    Apply,
    Publish,
    Discard,
}

fn render_confirmation(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    confirmation: Confirmation,
) {
    let Some(details) = state.details.as_ref() else {
        return;
    };
    let (action, color) = match confirmation {
        Confirmation::Apply => ("APPLY", theme::success()),
        Confirmation::Publish => ("PULL REQUEST", theme::success()),
        Confirmation::Discard => ("DISCARD", theme::danger()),
    };
    let mut lines = vec![
        Line::from(theme::chip(action, color)),
        Line::from(""),
        Line::from(Span::styled(
            details
                .task
                .as_deref()
                .unwrap_or("<legacy input unavailable>")
                .to_owned(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            details
                .repository
                .as_deref()
                .map_or("unavailable".to_owned(), |path| path.display().to_string()),
            theme::muted(),
        )),
        Line::from(Span::styled(format!("run {}", details.id), theme::muted())),
        Line::from(""),
    ];
    if confirmation == Confirmation::Discard {
        lines.push(Line::from(
            "Discard is logical disposition; owned cleanup follows application semantics.",
        ));
        lines.push(Line::from("Enter confirms discard."));
    } else {
        if let Some(diff) = state.diff.as_ref() {
            lines.push(theme::section(&format!(
                "{} FILES",
                diff.changed_files.len()
            )));
            for file in diff.changed_files.iter().take(8) {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {}{}",
                        file.path,
                        if file.binary { " [binary]" } else { "" }
                    ),
                    theme::text(),
                )));
            }
            lines.push(Line::from(""));
        }
        lines.push(Line::from(if confirmation == Confirmation::Apply {
            "Review [d] diff first when needed. Enter confirms apply."
        } else {
            "Commits on the run's branch, pushes to origin, opens a pull request. \
             Your checkout is untouched. Enter confirms."
        }));
    }
    lines.push(Line::from(Span::styled("Esc cancels", theme::muted())));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(overlay_block(
                match confirmation {
                    Confirmation::Apply => " APPLY ",
                    Confirmation::Publish => " PULL REQUEST ",
                    Confirmation::Discard => " DISCARD ",
                },
                color,
            )),
        area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// One state made visible: the glyph, the word, and the colour, bound
/// together.
///
/// Nothing here hands out a bare colour. A caller that wants to show a state
/// gets a span that already carries the state in characters, so stripping
/// every colour — `NO_COLOR`, a monochrome terminal, a colour-blind reader —
/// leaves the meaning intact. That rule used to hold by discipline, with
/// `run_glyph` and `status_style` sitting side by side and nothing stopping a
/// caller from reaching for the second alone; it now holds by construction.
///
/// It is also the single source for the glyph. The run status line used to
/// spell out its own `"✗ FAILED"`, duplicating the glyph table a few hundred
/// lines away and free to drift from it.
#[derive(Clone, Copy)]
struct StatusVisual {
    glyph: &'static str,
    label: &'static str,
    color: Color,
}

impl StatusVisual {
    /// The glyph alone, for rails where a neighbouring column carries the word.
    fn glyph(self) -> Span<'static> {
        Span::styled(format!("{} ", self.glyph), Style::new().fg(self.color))
    }

    /// Glyph and word together, for a single span that must stand on its own.
    fn badge(self) -> Span<'static> {
        Span::styled(
            format!("{} {}", self.glyph, self.label),
            Style::new().fg(self.color),
        )
    }

    /// As `badge`, emphasised where the state is the headline of its panel.
    fn badge_bold(self) -> Span<'static> {
        Span::styled(
            format!("{} {}", self.glyph, self.label),
            Style::new().fg(self.color).add_modifier(Modifier::BOLD),
        )
    }
}

fn run_visual(status: RunStatus) -> StatusVisual {
    let (glyph, label, color) = match status {
        RunStatus::Completed => ("✓", "COMPLETED", theme::success()),
        RunStatus::Applied => ("✓", "APPLIED", theme::success()),
        RunStatus::Running => ("●", "RUNNING", theme::accent()),
        RunStatus::NeedsUser => ("⚠", "NEEDS YOU", theme::attention()),
        RunStatus::Failed => ("✗", "FAILED", theme::danger()),
        RunStatus::Paused => ("‖", "PAUSED", theme::suspended()),
        RunStatus::Interrupted => ("↻", "INTERRUPTED", theme::suspended()),
        RunStatus::Ready => ("○", "WAITING", theme::attention()),
        RunStatus::Created | RunStatus::Preparing => ("○", "WAITING", theme::muted_color()),
        RunStatus::Discarded => ("×", "DISCARDED", theme::muted_color()),
    };
    StatusVisual {
        glyph,
        label,
        color,
    }
}

fn stage_visual(status: StageStatus) -> StatusVisual {
    let (glyph, label, color) = match status {
        StageStatus::Completed => ("✓", "COMPLETED", theme::success()),
        StageStatus::Running => ("●", "RUNNING", theme::accent()),
        StageStatus::NeedsUser => ("⚠", "NEEDS YOU", theme::attention()),
        StageStatus::Failed => ("✗", "FAILED", theme::danger()),
        StageStatus::Paused => ("‖", "PAUSED", theme::suspended()),
        StageStatus::Interrupted => ("↻", "INTERRUPTED", theme::suspended()),
        StageStatus::Ready => ("○", "READY", theme::attention()),
        StageStatus::Pending => ("○", "PENDING", theme::muted_color()),
        StageStatus::Skipped => ("○", "SKIPPED", theme::muted_color()),
    };
    StatusVisual {
        glyph,
        label,
        color,
    }
}

fn enum_text(value: impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn short_id(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

fn field_display(field: &super::state::TextField, selected: bool) -> String {
    if !selected {
        return field.text().to_owned();
    }
    let byte = field
        .text()
        .char_indices()
        .nth(field.cursor())
        .map_or(field.text().len(), |(index, _)| index);
    let mut value = field.text().to_owned();
    value.insert(byte, '│');
    value
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::{RouteSummary, RunListItem, StageSummary, StageWaitingSummary, UsageSummary};
    use crate::domain::{EffortSetting, Role, RunId, StageId, StageKind, WorkflowKind};
    use crate::tui::state::StageHeadline;

    // POD's legs, folded to half-blocks: the one art fragment every scene
    // keeps, so it marks "POD is on screen" regardless of costume or prop.
    const POD_SHELL: &str = "██    ██";

    /// The contract guarded where it can actually be reopened.
    ///
    /// `every_state_stays_distinguishable_with_all_colour_removed` proves the
    /// states are distinguishable today. It cannot prove they will stay that
    /// way: a caller only has to reach past the glyph for the colour, and that
    /// test keeps passing while the interface quietly loses its meaning for
    /// anyone without colour. So the two ways of reaching past it are closed
    /// here — `StatusVisual` may only produce spans, and production code may
    /// not read its colour field directly.
    #[test]
    fn a_status_visual_never_hands_out_a_bare_colour() {
        let source = include_str!("render.rs");
        let implementation = source
            .split("impl StatusVisual {")
            .nth(1)
            .expect("StatusVisual has an impl block")
            .split("\n}\n")
            .next()
            .expect("impl block body");
        for signature in implementation
            .lines()
            .filter(|line| line.trim_start().starts_with("fn "))
        {
            assert!(
                signature.contains("-> Span<'static>"),
                "a status must reach the screen carrying its glyph: {signature}"
            );
        }

        // Outside its own impl block, where reading the field is how the
        // spans get built in the first place.
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production half")
            .replace(implementation, "");
        assert!(
            !production.contains(".color"),
            "production code must take a span from StatusVisual, never its colour"
        );
    }

    /// The contract that lets the palette get richer later: strip every
    /// colour and the interface must still say which state each thing is in.
    /// Under Mono all tokens collapse to one value, so anything still legible
    /// is carried by a glyph or a word — never by hue. Without this, adding a
    /// vivid theme or motion would quietly let the aesthetic layer own
    /// meaning, and a colour-blind or piped reader would lose it.
    #[test]
    fn every_state_stays_distinguishable_with_all_colour_removed() {
        theme::with_palette(
            theme::Palette::resolve(theme::ColorCapability::Mono, theme::ThemeChoice::Native),
            || {
                // The states a reader must never confuse, whatever the terminal.
                let runs = [
                    RunStatus::Running,
                    RunStatus::Completed,
                    RunStatus::Failed,
                    RunStatus::NeedsUser,
                    RunStatus::Paused,
                    RunStatus::Interrupted,
                ];
                for (index, status) in runs.iter().enumerate() {
                    for other in runs.iter().skip(index + 1) {
                        assert_ne!(
                            run_visual(*status).glyph,
                            run_visual(*other).glyph,
                            "{status:?} and {other:?} are told apart only by colour"
                        );
                    }
                    // And colour genuinely carries nothing here, so the glyph above
                    // is doing the whole job rather than merely helping.
                    assert_eq!(
                        run_visual(*status).color,
                        run_visual(runs[0]).color,
                        "a status keeping its own colour under Mono is still using hue"
                    );
                }

                let stages = [
                    StageStatus::Running,
                    StageStatus::Completed,
                    StageStatus::Failed,
                    StageStatus::NeedsUser,
                    StageStatus::Paused,
                    StageStatus::Interrupted,
                ];
                for (index, status) in stages.iter().enumerate() {
                    for other in stages.iter().skip(index + 1) {
                        assert_ne!(
                            stage_visual(*status).glyph,
                            stage_visual(*other).glyph,
                            "{status:?} and {other:?} are told apart only by colour"
                        );
                    }
                    assert_eq!(stage_visual(*status).color, stage_visual(stages[0]).color);
                }
            },
        );
    }

    fn render_text(state: &TuiState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// Every foreground colour a frame actually paints.
    fn painted_colours(state: &TuiState, width: u16, height: u16) -> Vec<Color> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.fg)
            .collect()
    }

    /// One symbol per rendered cell, so a test can compare two frames cell by
    /// cell rather than as a run-together string.
    fn render_symbols(state: &TuiState, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol().to_owned())
            .collect()
    }

    /// The footer's contextual half as plain text.
    fn actions_text(state: &TuiState) -> String {
        primary_actions(state.screen, state)
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn at(hour: u32, minute: u32, second: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 21, hour, minute, second)
            .single()
            .unwrap()
    }

    fn stage(id: &str, kind: StageKind, role: Role, status: StageStatus) -> StageSummary {
        StageSummary {
            id: StageId::new(id).unwrap(),
            kind,
            role,
            status,
            configured_provider: "codex".to_owned(),
            requested_effort: EffortSetting::NativeDefault,
            observed_effort: None,
            configured_model: None,
            route_overridden: false,
            actual_provider: Some("codex".to_owned()),
            actual_model: None,
            provider_session_record: Some("session-record".to_owned()),
            native_session: Some("native-session-id".to_owned()),
            provider_session_status: Some("completed".to_owned()),
            process_status: Some("exited".to_owned()),
            started_at: None,
            finished_at: None,
            waiting: None,
            failure_reason: None,
            blocking: false,
        }
    }

    /// A skipped required dependency must never read as "failed": each
    /// blocked dependency states its own outcome.
    #[test]
    fn blocked_message_states_each_dependencys_own_outcome() {
        let mut pending = stage(
            "decision",
            StageKind::Decision,
            Role::EngineeringLead,
            StageStatus::Pending,
        );
        pending.waiting = Some(StageWaitingSummary {
            waiting_on: Vec::new(),
            blocked_by: vec![
                BlockedDependencyRef {
                    id: StageId::new("quality_review").unwrap(),
                    kind: StageKind::CodeQualityReview,
                    outcome: DependencyOutcome::Failed,
                },
                BlockedDependencyRef {
                    id: StageId::new("spec_review").unwrap(),
                    kind: StageKind::SpecReview,
                    outcome: DependencyOutcome::Skipped,
                },
            ],
            degraded: Vec::new(),
        });

        assert_eq!(
            waiting_message(&pending),
            "Blocked: Quality review failed, Spec review was skipped"
        );
    }

    fn details(status: RunStatus, stages: Vec<StageSummary>) -> RunDetails {
        RunDetails {
            id: RunId::from_u128(3),
            task: Some("Add OAuth provider support".to_owned()),
            workflow: WorkflowKind::Standard,
            status,
            repository: Some(std::path::PathBuf::from("/Users/e/Code/wp-calypso-2")),
            workspace_mode: Some(crate::workspace::WorkspaceMode::Branch),
            workspace_status: Some(crate::workspace::WorkspaceStatus::Ready),
            base_commit: Some("abc1234".to_owned()),
            profile: "recommended".to_owned(),
            profile_version: "recommended_v2".to_owned(),
            routes: vec![RouteSummary {
                role: Role::Implementer,
                configured_provider: "codex".to_owned(),
                configured_model: None,
                reason: "recommended_role_assignment".to_owned(),
                requested_effort: Some(crate::domain::EffortSetting::NativeDefault),
            }],
            revision: crate::store::RunRevision::initial(),
            created_at: at(12, 0, 0),
            updated_at: at(12, 5, 0),
            stages,
            attention: Vec::new(),
            usage: crate::app::RunUsage::from_totals([
                (
                    "claude".to_owned(),
                    UsageSummary {
                        input_units: 128,
                        output_units: 50_435,
                        cache_read_units: Some(4_373_955),
                        cache_write_units: Some(308_947),
                        reasoning_output_units: None,
                    },
                ),
                (
                    "codex".to_owned(),
                    UsageSummary {
                        input_units: 9_246_322,
                        output_units: 63_345,
                        cache_read_units: Some(8_885_760),
                        cache_write_units: Some(0),
                        reasoning_output_units: Some(31_932),
                    },
                ),
            ]),
            started_at: None,
            finished_at: None,
            failure_reason: None,
        }
    }

    /// A run whose implementation stage is in flight.
    fn running_state() -> TuiState {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.screen = Screen::RunDetail;
        state.selected_run = Some(RunId::from_u128(3));
        let mut stages = vec![
            stage(
                "architecture",
                StageKind::Architecture,
                Role::Architect,
                StageStatus::Completed,
            ),
            stage(
                "implementation",
                StageKind::Implementation,
                Role::Implementer,
                StageStatus::Running,
            ),
            stage(
                "quality_review",
                StageKind::CodeQualityReview,
                Role::CodeQualityReviewer,
                StageStatus::Pending,
            ),
        ];
        stages[0].started_at = Some(at(12, 0, 0));
        stages[0].finished_at = Some(at(12, 2, 14));
        stages[1].started_at = Some(at(12, 2, 14));
        let mut run = details(RunStatus::Running, stages);
        run.started_at = Some(at(12, 0, 0));
        state.replace_details(run);
        state.selected_stage_index = 1;
        state.selected_stage = Some(StageId::new("implementation").unwrap());
        state
    }

    /// A completed, applyable run.
    fn completed_state() -> TuiState {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.screen = Screen::RunDetail;
        state.selected_run = Some(RunId::from_u128(3));
        let mut stages = vec![stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            StageStatus::Completed,
        )];
        stages[0].started_at = Some(at(12, 0, 0));
        stages[0].finished_at = Some(at(12, 4, 32));
        let mut run = details(RunStatus::Completed, stages);
        run.started_at = Some(at(12, 0, 0));
        run.finished_at = Some(at(12, 12, 48));
        state.replace_details(run);
        state
    }

    /// The review workflow's real shape when an optional review fails and
    /// the run still completes: the decision ruled over the gap.
    fn completed_with_failure_details() -> RunDetails {
        let stages = vec![
            stage(
                "research",
                StageKind::Research,
                Role::Researcher,
                StageStatus::Completed,
            ),
            stage(
                "quality_review",
                StageKind::CodeQualityReview,
                Role::CodeQualityReviewer,
                StageStatus::Completed,
            ),
            stage(
                "spec_review",
                StageKind::SpecReview,
                Role::SpecReviewer,
                StageStatus::Failed,
            ),
            stage(
                "synthesis",
                StageKind::Synthesis,
                Role::EngineeringLead,
                StageStatus::Completed,
            ),
            stage(
                "decision",
                StageKind::Decision,
                Role::EngineeringLead,
                StageStatus::Completed,
            ),
        ];
        let mut run = details(RunStatus::Completed, stages);
        run.started_at = Some(at(12, 0, 0));
        run.finished_at = Some(at(12, 12, 48));
        run
    }

    fn completed_with_failure_state() -> TuiState {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.screen = Screen::RunDetail;
        state.selected_run = Some(RunId::from_u128(3));
        state.replace_details(completed_with_failure_details());
        state
    }

    #[test]
    fn empty_runs_and_small_terminal_render_without_panicking() {
        let state = TuiState::new(std::path::Path::new("/repo"));
        assert!(render_text(&state, 90, 24).contains("No runs yet"));
        assert!(render_text(&state, 49, 9).contains("Terminal too small"));
    }

    #[test]
    fn header_carries_product_identity_and_run_state() {
        let text = render_text(&running_state(), 160, 40);
        assert!(text.contains("POLYCODE"));
        assert!(text.contains("MISSION DECK"));
        assert!(text.contains("wp-calypso-2"), "concise repository identity");
        assert!(
            !text.contains("/Users/e/Code/wp-calypso-2"),
            "the full path stays in technical mode"
        );
        // The run id is the handle every CLI command takes, so entering a run
        // shows it without hunting for technical mode.
        assert!(
            text.contains(&RunId::from_u128(3).to_string()),
            "header names the run id"
        );
        // Narrow terminals keep the state and drop the identity.
        let narrow = render_text(&running_state(), 70, 24);
        assert!(narrow.contains("POLYCODE"));
        assert!(!narrow.contains("wp-calypso-2"));
    }

    #[test]
    fn runs_screen_renders_summary_and_needs_user_prominently() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        let id = RunId::from_u128(1);
        state.replace_runs(vec![RunListItem {
            id,
            workflow: WorkflowKind::Standard,
            status: RunStatus::NeedsUser,
            task_summary: "OAuth provider".to_owned(),
            repository: Some(std::path::PathBuf::from("/repo")),
            updated_at: at(12, 0, 0),
            hidden: false,
        }]);
        state.details = Some(details(RunStatus::NeedsUser, Vec::new()));
        let text = render_text(&state, 120, 30);
        assert!(text.contains("OAuth provider"));
        assert!(text.contains("NEEDS YOU"));
        assert!(text.contains("▸ "), "the selected run carries a cursor");
        state.overlay = Some(Overlay::Help);
        assert!(render_text(&state, 120, 30).contains("Help · Esc closes"));
    }

    /// Hiding has three visible surfaces: the footer offers the keys, a run
    /// shown by the all-runs view carries a "hidden" mark, and a list where
    /// everything is hidden says so instead of claiming there are no runs.
    #[test]
    fn hidden_runs_are_advertised_marked_and_counted() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.replace_runs(vec![RunListItem {
            id: RunId::from_u128(7),
            workflow: WorkflowKind::Standard,
            status: RunStatus::Completed,
            task_summary: "Ship the widget".to_owned(),
            repository: None,
            updated_at: at(12, 0, 0),
            hidden: false,
        }]);
        state.details = Some(details(RunStatus::Completed, Vec::new()));
        let text = render_text(&state, 120, 30);
        assert!(text.contains("[h] Hide"), "footer offers hiding");
        assert!(!text.contains("Show hidden"), "nothing is hidden yet");

        // Something hidden exists: the footer says how to see it.
        state.hidden_count = 1;
        assert!(render_text(&state, 120, 30).contains("[H] Show hidden"));

        // The all-runs view marks the hidden run and offers to re-hide.
        state.show_hidden = true;
        state.runs[0].hidden = true;
        let showing = render_text(&state, 120, 30);
        assert!(showing.contains("hidden  "), "hidden run carries its mark");
        assert!(showing.contains("[h] Unhide"));
        assert!(showing.contains("[H] Hide hidden"));

        // Everything hidden: the empty state tells the truth.
        state.show_hidden = false;
        state.replace_runs(Vec::new());
        state.hidden_count = 2;
        let empty = render_text(&state, 120, 30);
        assert!(empty.contains("2 hidden runs."), "{empty}");
        assert!(empty.contains("Show hidden runs"));
        assert!(!empty.contains("No runs yet"));
    }

    #[test]
    fn pipeline_shows_human_stages_with_durations_and_no_fake_zero() {
        let text = render_text(&running_state(), 160, 40);
        assert!(text.contains("PIPELINE"));
        assert!(text.contains("Architecture"), "human stage name");
        assert!(text.contains("Quality review"), "human stage name");
        assert!(
            text.contains("2m 14s"),
            "completed stage shows its duration"
        );
        assert!(
            !text.contains("0s"),
            "a pending stage never shows a fabricated duration"
        );
    }

    #[test]
    fn selection_and_execution_are_distinct_in_the_rail() {
        let mut state = running_state();
        // Reading a finished stage while another one runs.
        state.selected_stage_index = 0;
        state.selected_stage = Some(StageId::new("architecture").unwrap());
        let text = render_text(&state, 160, 40);
        let cursor = text.find("▸ ").expect("the cursor marks the selection");
        let architecture = text.find("Architecture").unwrap();
        let implementation = text.find("Implementation").unwrap();
        assert!(
            cursor < architecture && cursor < implementation,
            "the cursor sits on the selected row, not the running one"
        );
        assert_eq!(
            text.matches("▸ ").count(),
            1,
            "exactly one row is selected at a time"
        );
        // The running stage keeps its own glyph regardless of the cursor.
        assert!(text.contains("● "), "execution state stays on the live row");
        assert!(
            text.contains("COMPLETED"),
            "the hero follows the selection, not the running stage"
        );
    }

    #[test]
    fn running_hero_leads_with_state_clock_runtime_and_activity() {
        let text = render_text(&running_state(), 160, 40);
        assert!(text.contains("IMPLEMENTATION"), "stage names the panel");
        assert!(text.contains("RUNNING"));
        assert!(text.contains("Agent is working…"));
        assert!(text.contains("codex"), "runtime summary is present");
        assert!(text.contains("native default"));
        assert!(
            !text.contains("Kind         "),
            "operational view drops technical field rows"
        );
    }

    #[test]
    fn operational_view_hides_diagnostics_and_technical_view_groups_them() {
        let mut state = running_state();
        let operational = render_text(&state, 160, 40);
        assert!(!operational.contains("native-session-id"), "no native ids");
        assert!(!operational.contains("session-record"));
        assert!(
            !operational.contains("/Users/e/Code/wp-calypso-2"),
            "operational view keeps the full path out"
        );
        assert!(operational.contains("[i] Details"));

        state.technical = true;
        let technical = render_text(&state, 160, 40);
        assert!(technical.contains("TECHNICAL"), "mode is labelled");
        for group in [
            "EXECUTION",
            "RUNTIME",
            "RESOURCE EVIDENCE",
            "WORKSPACE",
            "ROUTING",
        ] {
            assert!(technical.contains(group), "{group} group is present");
        }
        assert!(technical.contains("native-session-id".get(..8).unwrap()));
        assert!(technical.contains("/Users/e/Code/wp-calypso-2"));
        assert!(technical.contains("recommended_v2"));
        assert!(technical.contains("recommended_role_assignment"));
        assert!(technical.contains("abc1234"), "base commit stays available");
        assert!(technical.contains("[i] operational view"));
        assert!(
            !technical.contains(POD_SHELL),
            "technical mode spends POD's rows on diagnostics"
        );
    }

    #[test]
    fn runtime_summary_reports_mismatch_only_when_targets_disagree() {
        let mut aligned = stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            StageStatus::Running,
        );
        aligned.actual_model = None;
        assert!(!runtime_summary(&aligned).contains("configured →"));

        // A confirmed model where the route asked for the native default is
        // confirmation, not drift.
        let mut confirmed = aligned.clone();
        confirmed.actual_model = Some("gpt-5.4-codex".to_owned());
        let summary = runtime_summary(&confirmed);
        assert!(
            !summary.contains("configured →"),
            "confirmation is not drift"
        );
        assert!(summary.contains("gpt-5.4-codex"), "the real model is named");

        let mut drifted = aligned.clone();
        drifted.configured_model = Some("sonnet".to_owned());
        drifted.actual_provider = Some("claude".to_owned());
        drifted.actual_model = Some("opus".to_owned());
        let summary = runtime_summary(&drifted);
        assert!(summary.contains("configured →"), "drift is surfaced");
        assert!(summary.contains("opus"));
    }

    /// The two runtimes do not report the same quantity under the name
    /// "input": Claude's excludes what its cache served, Codex's includes it.
    /// So there is one line per runtime, no total across them, and a cached
    /// token is never printed twice on the runtime that already folded it in.
    #[test]
    fn resources_never_sum_two_runtimes_or_count_a_cached_token_twice() {
        let lines = resource_lines(&details(RunStatus::Running, Vec::new()));
        assert_eq!(lines.len(), 2, "one line per reporting runtime");
        let claude = lines
            .iter()
            .find(|line| line.starts_with("claude"))
            .expect("claude line");
        let codex = lines
            .iter()
            .find(|line| line.starts_with("codex"))
            .expect("codex line");

        // Claude keeps input and cache read disjoint, so both are quantities
        // and both are listed.
        assert!(claude.contains("128 input"), "{claude}");
        assert!(claude.contains("4.3M cache read"), "{claude}");

        // Codex folds cache reads into its input total. Naming it again as a
        // separate dimension would show the same 8.9M tokens twice.
        assert!(codex.contains("9.2M input"), "{codex}");
        assert!(codex.contains("8.8M of it cached"), "{codex}");
        assert!(
            !codex.contains("cache read"),
            "cached input is already inside the input total: {codex}"
        );
        // 8.9M cached tokens must not also appear as their own dimension.
        assert_eq!(
            codex.matches("8.8M").count(),
            1,
            "a cached token is named once: {codex}"
        );

        // 128 + 9_246_322 is not a quantity of anything, so no line may
        // speak for both runtimes at once.
        for line in &lines {
            assert!(
                !(line.contains("claude") && line.contains("codex")),
                "runtimes are never merged into one figure: {line}"
            );
        }
        assert!(
            !lines.iter().any(|line| line.contains('$')),
            "usage never implies cost"
        );
    }

    /// The one input figure that means the same thing on both runtimes.
    #[test]
    fn uncached_input_is_derived_only_where_the_runtime_declared_how_it_counts() {
        let usage = details(RunStatus::Running, Vec::new()).usage;
        let by_provider = usage.providers().collect::<Vec<_>>();
        let claude = &by_provider[0];
        let codex = &by_provider[1];
        assert_eq!(claude.uncached_input_units(), Some(128));
        assert_eq!(codex.uncached_input_units(), Some(9_246_322 - 8_885_760));

        // An unrecognised runtime declared no convention, so nothing is
        // derived on its behalf.
        let unknown = crate::app::RunUsage::from_totals([(
            "gemini".to_owned(),
            UsageSummary {
                input_units: 4_000,
                output_units: 10,
                cache_read_units: Some(3_000),
                cache_write_units: None,
                reasoning_output_units: None,
            },
        )]);
        let entry = unknown.providers().next().expect("one entry");
        assert_eq!(entry.uncached_input_units(), None);
        assert!(!entry.input_contains_cache_reads());
    }

    #[test]
    fn needs_user_dominates_the_hero_and_precedes_everything_secondary() {
        let mut state = running_state();
        let details = state.details.as_mut().unwrap();
        details.status = RunStatus::NeedsUser;
        details.stages[1].status = StageStatus::NeedsUser;
        details.attention = vec![crate::app::AttentionSummary {
            id: crate::domain::AttentionRequestId::from_u128(1),
            stage_id: StageId::new("implementation").unwrap(),
            kind: AttentionKind::Permission,
            summary: "Claude requests permission to use Bash".to_owned(),
        }];
        let text = render_text(&state, 160, 40);
        assert!(text.contains("ACTION REQUIRED"));
        assert!(text.contains("Claude requests permission to use Bash"));
        assert!(text.contains("[u] Review and resolve"));
        let action = text.find("ACTION REQUIRED").unwrap();
        for secondary in ["RESOURCES", "RESULT", "codex · native default"] {
            assert!(
                action < text.find(secondary).unwrap(),
                "attention outranks {secondary}"
            );
        }
        assert!(
            actions_text(&state).starts_with("[u] Resolve attention"),
            "the footer leads with the attention shortcut"
        );
    }

    #[test]
    fn completed_run_pivots_to_review_and_drops_monitoring_language() {
        let text = render_text(&completed_state(), 160, 40);
        assert!(text.contains("RUN COMPLETE"));
        assert!(text.contains("12:48"), "run elapsed is prominent");
        assert!(text.contains("READY TO REVIEW"));
        assert!(text.contains("[a] Apply changes"));
        assert!(text.contains("[X] Discard"));
        assert!(
            !text.contains("Agent is working"),
            "monitoring language is gone after completion"
        );
        assert!(
            !text.contains("Stage finished"),
            "the run, not the stage, speaks after the pivot"
        );
    }

    #[test]
    fn failed_state_is_legible_and_offers_recovery_once() {
        let mut state = running_state();
        let details = state.details.as_mut().unwrap();
        details.status = RunStatus::Failed;
        details.stages[1].status = StageStatus::Failed;
        details.stages[1].finished_at = Some(at(12, 3, 0));
        let text = render_text(&state, 160, 40);
        assert!(text.contains("✗ FAILED"));
        assert!(text.contains("No completed result"));
        assert!(text.contains("[t] Retry"));
        assert!(text.contains("[l] Logs"));
        assert_eq!(
            text.matches("[t] Retry").count(),
            2,
            "hero and footer each offer retry exactly once"
        );
        assert!(
            !text.contains("provider exited"),
            "raw provider metadata stays out of the operational view"
        );
    }

    /// The failure reason folded onto a stage is the one place the operator
    /// learns *why* without ever leaving the panel: the strip's sentence and
    /// the hero's activity line both show it in place of the generic text.
    #[test]
    fn a_failed_stage_shows_its_reason_instead_of_generic_text() {
        let mut state = running_state();
        let details = state.details.as_mut().unwrap();
        details.status = RunStatus::Failed;
        details.stages[1].status = StageStatus::Failed;
        details.stages[1].finished_at = Some(at(12, 3, 0));
        details.stages[1].failure_reason = Some("compile failed: missing semicolon".to_owned());
        details.stages[1].blocking = true;

        let sentences = status_sentences(state.details.as_ref().unwrap(), at(12, 13, 0), 160);
        assert_eq!(
            sentences[0],
            "Implementation failed: compile failed: missing semicolon"
        );

        let text = render_text(&state, 160, 40);
        assert!(text.contains("compile failed: missing semicolon"));
        assert!(
            !text.contains("The provider ended this stage before it completed"),
            "the reason replaces the generic activity line, not adds to it"
        );
    }

    /// A stage that failed without the runtime reporting why keeps the
    /// generic activity text — the reason is a bonus, never a requirement.
    #[test]
    fn a_failed_stage_without_a_reason_keeps_the_generic_text() {
        let mut state = running_state();
        let details = state.details.as_mut().unwrap();
        details.status = RunStatus::Failed;
        details.stages[1].status = StageStatus::Failed;
        details.stages[1].finished_at = Some(at(12, 3, 0));
        let text = render_text(&state, 160, 40);
        assert!(text.contains("The provider ended this stage before it completed"));
    }

    /// The strip is a fixed [`STATUS_HEIGHT`] rows. A reason near the 200
    /// character cap must be cut to the strip's own width rather than wrap
    /// and push the stage-count sentence out of the box.
    #[test]
    fn status_strip_truncates_a_long_reason_to_fit_the_available_width() {
        let mut failing = stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            StageStatus::Failed,
        );
        failing.failure_reason = Some("x".repeat(200));
        failing.blocking = true;
        let run = details(RunStatus::Failed, vec![failing]);

        let width = 40u16;
        let sentences = status_sentences(&run, at(12, 13, 0), width);
        assert!(
            sentences[0].chars().count() <= width as usize,
            "the reason line must fit the strip's fixed width: {:?}",
            sentences[0]
        );
        assert!(sentences[0].ends_with('…'), "the cut is visible");
        assert_eq!(
            sentences[1], "0 of 1 stages complete, 1 failed.",
            "the stage-count sentence survives a long reason instead of being pushed out"
        );
    }

    /// At the 50-column supported minimum the rail's inner width is only
    /// about 16 columns — narrower than truncating just the reason ever
    /// accounted for, since the untruncated `"<Stage> failed: "` prefix could
    /// still push the composed line past `width`. With a short stage title
    /// the prefix itself still fits, so the fix (bounding the whole composed
    /// sentence) must truncate successfully rather than fall back, and the
    /// result must never exceed the strip's width.
    #[test]
    fn status_strip_bounds_the_whole_composed_sentence_at_a_16_column_width() {
        let mut failing = stage(
            "fix",
            StageKind::Fix,
            Role::Implementer,
            StageStatus::Failed,
        );
        failing.failure_reason = Some("x".repeat(200));
        failing.blocking = true;
        let run = details(RunStatus::Failed, vec![failing]);

        let width = 16u16;
        let sentences = status_sentences(&run, at(12, 13, 0), width);
        assert!(
            sentences[0].chars().count() <= width as usize,
            "the composed sentence must fit the strip's width: {:?}",
            sentences[0]
        );
        assert!(
            sentences[0].starts_with("Fix failed: "),
            "the prefix must survive intact, not be cut mid-word: {:?}",
            sentences[0]
        );
        assert!(sentences[0].ends_with('…'), "the cut is visible");
        assert_eq!(
            sentences[1], "0 of 1 stages complete, 1 failed.",
            "the stage-count sentence survives a bounded composed sentence"
        );
    }

    /// The real bug this guards: `"Implementation failed: "` alone is 23
    /// characters, already wider than the ~16-column rail at the smallest
    /// supported terminal. No truncation of that prefix can produce anything
    /// legible, so rather than land mid-word the strip drops the reason
    /// entirely and falls back to the reasonless generic sentence — the same
    /// text shown when a failed stage carries no reason at all. This
    /// necessarily still exceeds 16 columns (an unavoidable consequence of
    /// naming a 14-character stage at that width, present before failure
    /// reasons existed), but the stage-count sentence must still be exactly
    /// right — proving the fallback never corrupts anything past it.
    #[test]
    fn status_strip_falls_back_to_the_generic_sentence_when_even_the_prefix_cannot_fit() {
        let mut failing = stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            StageStatus::Failed,
        );
        failing.failure_reason = Some("x".repeat(200));
        failing.blocking = true;
        let run = details(RunStatus::Failed, vec![failing]);

        let sentences = status_sentences(&run, at(12, 13, 0), 16);
        assert_eq!(
            sentences[0], "Implementation failed — its logs say why.",
            "an unfittable prefix falls back cleanly instead of a garbled truncation"
        );
        assert_eq!(
            sentences[1], "0 of 1 stages complete, 1 failed.",
            "the stage-count sentence survives even the fallback path"
        );
    }

    /// The hero shows the whole reason, not a one-line cut of it: reading
    /// the message is why anyone looks at a failed stage. A reason at the
    /// 200-character cap is longer than the panel, so it must wrap onto
    /// further rows and still be fully present in the rendered text.
    #[test]
    fn hero_shows_the_whole_failure_reason_wrapped_under_its_own_heading() {
        let mut state = running_state();
        let details = state.details.as_mut().unwrap();
        details.status = RunStatus::Failed;
        let reason = "You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at 4:05 PM.";
        details.stages[1].status = StageStatus::Failed;
        details.stages[1].failure_reason = Some(reason.to_owned());
        details.stages[1].blocking = true;
        state.selected_stage_index = 1;
        let text = render_text(&state, 120, 40);
        assert!(text.contains("WHY IT FAILED"), "{text}");
        let joined: String = text
            .lines()
            .map(|line| line.trim_start_matches(['│', ' ']).trim_end())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.contains("try again at 4:05 PM."),
            "the tail of the reason survives instead of an ellipsis:\n{text}"
        );
        assert!(!text.contains("usage limit…"), "no one-line cut:\n{text}");
    }

    /// A failed stage with no reason, or any non-failed status, falls back
    /// to the generic typed activity text rather than an empty heading.
    #[test]
    fn hero_falls_back_to_the_generic_message_without_a_reason() {
        let failing = stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            StageStatus::Failed,
        );
        assert_eq!(failed_stage_reason(&failing), None);
        assert_eq!(
            activity_message(&failing),
            Some("The provider ended this stage before it completed".to_owned())
        );
        let running = stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            StageStatus::Running,
        );
        assert_eq!(failed_stage_reason(&running), None);
        assert_eq!(
            activity_message(&running),
            Some("Agent is working…".to_owned())
        );
    }

    /// A failed stage's reason outranks stale waiting info on the same
    /// stage; a Pending stage with no failure keeps its waiting text.
    #[test]
    fn failure_reason_outranks_waiting_info_by_status() {
        let mut failing = stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            StageStatus::Failed,
        );
        failing.failure_reason = Some("compile failed".to_owned());
        failing.waiting = Some(StageWaitingSummary {
            waiting_on: vec![StageDependencyRef {
                id: StageId::new("architecture").unwrap(),
                kind: StageKind::Architecture,
            }],
            blocked_by: Vec::new(),
            degraded: Vec::new(),
        });
        assert_eq!(failed_stage_reason(&failing), Some("compile failed"));

        let mut pending = stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            StageStatus::Pending,
        );
        pending.failure_reason = Some("stale".to_owned());
        pending.waiting = Some(StageWaitingSummary {
            waiting_on: vec![StageDependencyRef {
                id: StageId::new("architecture").unwrap(),
                kind: StageKind::Architecture,
            }],
            blocked_by: Vec::new(),
            degraded: Vec::new(),
        });
        assert_eq!(failed_stage_reason(&pending), None);
        assert_eq!(
            activity_message(&pending),
            Some("Waiting on: Architecture".to_owned())
        );
    }

    /// The Runs screen's overview names the reason before the run is even
    /// opened, and only for a failed run with a blocking reason.
    #[test]
    fn runs_overview_shows_the_blocking_failure_reason() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.replace_runs(vec![RunListItem {
            id: RunId::from_u128(1),
            workflow: WorkflowKind::Standard,
            status: RunStatus::Failed,
            task_summary: "OAuth provider".to_owned(),
            repository: Some(std::path::PathBuf::from("/repo")),
            updated_at: at(12, 0, 0),
            hidden: false,
        }]);
        let mut failed = details(RunStatus::Failed, Vec::new());
        failed.failure_reason = Some("You've hit your usage limit.".to_owned());
        state.details = Some(failed);
        let text = render_text(&state, 160, 40);
        assert!(text.contains("WHY IT FAILED"), "{text}");
        assert!(text.contains("You've hit your usage limit."), "{text}");

        state.details = Some(details(RunStatus::Failed, Vec::new()));
        let text = render_text(&state, 160, 40);
        assert!(
            !text.contains("WHY IT FAILED"),
            "no heading without a reason:\n{text}"
        );
    }

    /// Opening a failed run lands on the stage that failed it, so the hero
    /// opens on the reason; a run that is not failed keeps its selection,
    /// and a failed stage that did not block the run is not chosen.
    #[test]
    fn focus_blocking_failure_selects_the_blocking_failed_stage_only() {
        let mut state = running_state();
        state.selected_stage_index = 0;
        state.selected_stage = state
            .details
            .as_ref()
            .map(|details| details.stages[0].id.clone());
        state.focus_blocking_failure();
        assert_eq!(state.selected_stage_index, 0, "a running run is untouched");

        let details = state.details.as_mut().unwrap();
        details.status = RunStatus::Failed;
        details.stages[1].status = StageStatus::Failed;
        details.stages[1].blocking = false;
        state.focus_blocking_failure();
        assert_eq!(
            state.selected_stage_index, 0,
            "a non-blocking failure is not the reason the run failed"
        );

        state.details.as_mut().unwrap().stages[1].blocking = true;
        state.focus_blocking_failure();
        assert_eq!(state.selected_stage_index, 1);
        assert_eq!(
            state.selected_stage.as_ref().map(ToString::to_string),
            Some(state.details.as_ref().unwrap().stages[1].id.to_string())
        );
    }

    #[test]
    fn footer_advertises_apply_only_when_the_run_is_applyable() {
        let running = running_state();
        let actions = actions_text(&running);
        assert!(!actions.contains("[a] Apply"), "no apply while running");
        assert!(actions.contains("[o] Result"));

        let completed = completed_state();
        let actions = actions_text(&completed);
        assert!(actions.contains("[a] Apply"));
        assert!(actions.contains("[X] Discard"));

        // Narrow terminals drop navigation, never the contextual actions.
        let wide: String = footer_line(Screen::RunDetail, &running, 200)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(wide.contains("Esc runs"));
        let narrow: String = footer_line(Screen::RunDetail, &running, 46)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(narrow.contains("[o] Result"), "actions survive");
        assert!(!narrow.contains("? help"), "navigation yields first");
    }

    /// A run whose sealed configuration routes both cycle roles offers `[c]`
    /// Continue and `[w]` Follow-ups beside `[f]` Fix, in both the footer
    /// and the hero panel; one that cannot route them offers neither.
    #[test]
    fn continue_and_follow_ups_hints_follow_the_same_gate_as_fix() {
        let mut continuable = completed_with_failure_details();
        continuable.routes.push(RouteSummary {
            role: Role::EngineeringLead,
            configured_provider: "codex".to_owned(),
            configured_model: None,
            reason: "test".to_owned(),
            requested_effort: Some(EffortSetting::NativeDefault),
        });
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.screen = Screen::RunDetail;
        state.selected_run = Some(RunId::from_u128(3));
        state.replace_details(continuable);

        let actions = actions_text(&state);
        assert!(actions.contains("[c] Continue"));
        assert!(actions.contains("[w] Follow-ups"));
        let text = render_text(&state, 160, 40);
        assert!(text.contains("[c] Continue"));
        assert!(text.contains("[w] Follow-ups"));

        // Without the EngineeringLead route, neither cycle can be offered.
        let unroutable = actions_text(&completed_with_failure_state());
        assert!(!unroutable.contains("Continue"));
        assert!(!unroutable.contains("Follow-ups"));
    }

    #[test]
    fn footer_offers_resume_for_suspended_and_orphaned_runs() {
        // Running with nobody driving it: left behind by a dead instance.
        let orphaned = running_state();
        assert!(actions_text(&orphaned).contains("[r] Resume"));

        // Running with its driver in flight needs no second one.
        let mut driven = running_state();
        driven.begin_action(crate::tui::worker::ActionKind::Resume, driven.selected_run);
        assert!(!actions_text(&driven).contains("[r] Resume"));

        let mut paused = running_state();
        paused.details.as_mut().unwrap().status = RunStatus::Paused;
        assert!(actions_text(&paused).contains("[r] Resume"));
    }

    #[test]
    fn result_section_is_state_aware() {
        let running = running_state();
        let text = render_text(&running, 160, 40);
        assert!(
            text.contains("Not available yet"),
            "informational, not error"
        );
        assert!(!text.contains("✓ Verified artifact available"));

        let mut completed = completed_state();
        completed
            .stages_with_artifacts
            .insert(StageId::new("implementation").unwrap());
        let text = render_text(&completed, 160, 40);
        assert!(text.contains("✓ Implementation ready"));
        assert!(text.contains("[Enter/o] Open result"));
    }

    /// The panel quotes the artifact of the stage the operator is looking at,
    /// and of no other. A headline outlives a keypress; the selection it was
    /// read for does not.
    #[test]
    fn the_result_section_quotes_the_selected_stage_and_only_that_one() {
        let mut state = completed_state();
        let implementation = StageId::new("implementation").unwrap();
        state.stages_with_artifacts.insert(implementation.clone());
        state.headline = Some(StageHeadline {
            stage_id: implementation,
            attempt: 1,
            content_size: 512,
            text: Some("It does what the task asked, but the retry path is untested.".to_owned()),
            contracted: true,
        });

        let text = render_text(&state, 160, 40);
        assert!(text.contains("It does what the task asked, but the retry path is untested."));
        assert!(
            text.contains("[Enter/o] Open result"),
            "the quote is a lead-in, not a replacement"
        );

        state.headline = state.headline.take().map(|headline| StageHeadline {
            stage_id: StageId::new("research").unwrap(),
            ..headline
        });
        let text = render_text(&state, 160, 40);
        assert!(!text.contains("It does what the task asked"));
        assert!(
            text.contains("✓ Implementation ready"),
            "a stale quote hides itself without hiding the artifact"
        );
    }

    /// A glance is two rows. An agent that ignored the word limit is cut off
    /// with an ellipsis rather than pushing Resources off the panel.
    #[test]
    fn a_long_opening_line_is_cut_to_two_rows() {
        let mut state = completed_state();
        let implementation = StageId::new("implementation").unwrap();
        state.stages_with_artifacts.insert(implementation.clone());
        state.headline = Some(StageHeadline {
            stage_id: implementation.clone(),
            attempt: 1,
            content_size: 4096,
            text: Some("word ".repeat(400)),
            contracted: false,
        });
        let selected = state.details.as_ref().unwrap().stages[0].clone();

        let lines = headline_lines(&state, &selected, 40);
        let quoted: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert!(quoted.ends_with('…'), "the cut is visible");
        assert!(
            quoted.chars().count() <= 40 * HEADLINE_ROWS + 1,
            "two rows at most: {}",
            quoted.chars().count()
        );
    }

    #[test]
    fn notification_never_hides_run_detail_navigation_hints() {
        let mut state = running_state();
        state.set_error("resuming run failed for run-1: boom");
        let text = render_text(&state, 160, 40);
        assert!(text.contains("boom"), "message is visible");
        assert!(text.contains("Esc runs"), "leave-screen hint stays visible");
        assert!(text.contains("x dismiss"), "dismiss affordance advertised");
    }

    #[test]
    fn message_kinds_render_distinct_styles() {
        let styles: Vec<_> = [
            UiMessageKind::Info,
            UiMessageKind::Success,
            UiMessageKind::Warning,
            UiMessageKind::Error,
        ]
        .into_iter()
        .map(message_presentation)
        .collect();
        for (index, (glyph, style)) in styles.iter().enumerate() {
            for (other_glyph, other_style) in styles.iter().skip(index + 1) {
                assert!(glyph != other_glyph || style != other_style);
            }
        }
    }

    /// Viewer content is external: a diff carries repository source, logs
    /// carry provider stdout. A control character written into a cell measures
    /// zero columns for Ratatui but still moves the terminal cursor, so the
    /// two disagree and the diff-based repaint can no longer erase what it
    /// drew — content survives on screen after the viewer is closed. The
    /// invariant is therefore checked at the buffer, where it is deterministic:
    /// nothing a viewer renders may be a control character.
    #[test]
    fn viewers_never_write_control_characters_into_the_buffer() {
        const HOSTILE: &str =
            "fn main() {\n\tlet x = 1;\r\n\u{1b}[31mred\u{1b}[0m\n\u{0}nul\u{7}bell\n";

        fn control_cells(text: &str) -> Vec<char> {
            text.chars()
                .filter(|character| {
                    character.is_control()
                        || matches!(character, '\u{7f}'..='\u{9f}' | '\u{200b}'..='\u{200f}')
                })
                .collect()
        }

        let mut state = completed_state();

        state.screen = Screen::Diff;
        state.diff = Some(crate::app::RunDiffPreview {
            text: format!("diff --git a/x b/x\n@@ -1 +1 @@\n+{HOSTILE}"),
            changed_files: vec![crate::app::ChangedFileSummary {
                path: "x".to_owned(),
                binary: false,
            }],
            total_bytes: 64,
            truncated: false,
        });
        let diff = render_text(&state, 120, 30);
        assert!(
            control_cells(&diff).is_empty(),
            "diff viewer leaked control characters: {:?}",
            control_cells(&diff)
        );
        assert!(diff.contains("let x = 1;"), "content still renders: {diff}");

        state.screen = Screen::Logs;
        state.logs = Some(crate::app::ProcessLogView {
            process_id: crate::process::ManagedProcessId::new(),
            process_status: "running".to_owned(),
            stdout: crate::app::ProcessLogStream {
                text: HOSTILE.to_owned(),
                total_bytes: 32,
                truncated: false,
            },
            stderr: crate::app::ProcessLogStream {
                text: String::new(),
                total_bytes: 0,
                truncated: false,
            },
        });
        let logs = render_text(&state, 120, 30);
        assert!(
            control_cells(&logs).is_empty(),
            "log viewer leaked control characters: {:?}",
            control_cells(&logs)
        );

        state.screen = Screen::Artifact;
        state.artifact = Some(crate::app::ArtifactView {
            summary: crate::app::ArtifactSummary {
                stage_id: StageId::new("implementation").unwrap(),
                kind: crate::domain::ArtifactKind::Implementation,
                status: crate::domain::ArtifactStatus::Complete,
                attempt: 1,
                provider: None,
                model: None,
                content_size: 10,
                created_at: at(12, 0, 0),
            },
            text: HOSTILE.to_owned(),
        });
        let rendered = render_text(&state, 120, 30);
        assert!(
            control_cells(&rendered).is_empty(),
            "artifact viewer leaked control characters: {:?}",
            control_cells(&rendered)
        );
        state.artifact_raw = true;
        let raw = render_text(&state, 120, 30);
        assert!(
            control_cells(&raw).is_empty(),
            "raw artifact viewer leaked control characters: {:?}",
            control_cells(&raw)
        );

        // The `[w]` overlay copies lines extracted from an agent-authored
        // decision artifact, not a viewer reading a file — the same
        // untrusted-content risk under a different entry point.
        state.artifact_raw = false;
        state.screen = Screen::RunDetail;
        state.overlay = Some(Overlay::FollowUps);
        state.follow_ups_text = Some(HOSTILE.to_owned());
        let follow_ups = render_text(&state, 120, 30);
        assert!(
            control_cells(&follow_ups).is_empty(),
            "follow-ups overlay leaked control characters: {:?}",
            control_cells(&follow_ups)
        );
        assert!(
            follow_ups.contains("let x = 1;"),
            "content still renders: {follow_ups}"
        );
    }

    #[test]
    fn artifact_viewer_renders_markdown_and_advertises_back() {
        let mut state = completed_state();
        state.screen = Screen::Artifact;
        state.artifact = Some(crate::app::ArtifactView {
            summary: crate::app::ArtifactSummary {
                stage_id: StageId::new("implementation").unwrap(),
                kind: crate::domain::ArtifactKind::Implementation,
                status: crate::domain::ArtifactStatus::Complete,
                attempt: 1,
                provider: None,
                model: None,
                content_size: 10,
                created_at: at(12, 0, 0),
            },
            text: "## Result\n\n**done** with `code`".to_owned(),
        });
        let text = render_text(&state, 120, 30);
        assert!(text.contains("Esc run detail"), "viewer advertises back");
        assert!(text.contains("IMPLEMENTATION"), "viewer names its stage");
        assert!(text.contains("Result"), "heading text renders");
        assert!(!text.contains("## Result"), "no literal markdown heading");
        assert!(!text.contains("**done**"), "no literal bold markers");

        state.artifact_raw = true;
        let raw = render_text(&state, 120, 30);
        assert!(
            raw.contains("## Result"),
            "raw mode shows markdown verbatim"
        );
        assert!(raw.contains("**done**"));
    }

    #[test]
    fn new_run_reads_as_a_briefing_with_a_focused_field() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.screen = Screen::NewRun;
        let text = render_text(&state, 120, 30);
        assert!(text.contains("START A RUN"));
        for label in ["TASK", "WORKFLOW", "REPOSITORY", "EXECUTION", "EFFORT"] {
            assert!(text.contains(label), "{label} field is labelled");
        }
        assert!(text.contains("[Enter] Start run"));
        assert!(text.contains("▸ "), "the focused field carries a cursor");
    }

    #[test]
    fn mascot_appears_with_space_and_disappears_when_constrained() {
        let empty = TuiState::new(std::path::Path::new("/repo"));
        assert!(render_text(&empty, 90, 24).contains(POD_SHELL));
        assert!(render_text(&empty, 90, 24).contains("READY"));
        assert!(!render_text(&empty, 55, 12).contains(POD_SHELL));

        let running = running_state();
        let text = render_text(&running, 160, 40);
        assert!(text.contains(POD_SHELL));
        assert!(
            text.contains("BUILDING"),
            "implementer running reads BUILDING"
        );
        assert!(
            text.contains("▄███████▄"),
            "the builder's laptop stands beside POD"
        );
        assert!(!render_text(&running, 70, 24).contains(POD_SHELL));
    }

    /// The palette has to arrive through the accessors, not merely resolve
    /// correctly in isolation. Rendering with Vivid installed paints
    /// specified colours; the default paints none, so the assertion is about
    /// the theme rather than about colour existing at all.
    #[test]
    fn the_vivid_palette_reaches_the_screen_and_the_native_one_leaves_it_to_the_terminal() {
        let state = running_state();
        let specified = |palette| {
            theme::with_palette(palette, || {
                painted_colours(&state, 160, 40)
                    .iter()
                    .copied()
                    .any(theme::is_specified)
            })
        };
        assert!(
            specified(theme::Palette::resolve(
                theme::ColorCapability::TrueColor,
                theme::ThemeChoice::Vivid
            )),
            "vivid resolved but never reached a cell"
        );
        assert!(
            !specified(theme::Palette::resolve(
                theme::ColorCapability::TrueColor,
                theme::ThemeChoice::Native
            )),
            "native painted a colour the terminal theme cannot override"
        );
    }

    /// A reaction says something happened. So it fires when the world moves
    /// under POD — and not when the user simply looks somewhere else, which
    /// also changes the face POD is wearing.
    #[test]
    fn pod_reacts_to_the_world_moving_and_not_to_being_looked_away_from() {
        let stages = |implementation| {
            vec![
                // Deliberately not the same face as the stage below it, so
                // moving the selection genuinely changes what POD shows.
                stage(
                    "architecture",
                    StageKind::Architecture,
                    Role::Architect,
                    StageStatus::Failed,
                ),
                stage(
                    "implementation",
                    StageKind::Implementation,
                    Role::Implementer,
                    implementation,
                ),
            ]
        };

        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.screen = Screen::RunDetail;
        state.selected_stage_index = 1;
        state.replace_details(details(RunStatus::Running, stages(StageStatus::Running)));
        state.settle_reaction(std::time::Instant::now());
        assert!(
            !state.reacting,
            "the first sighting of a run is not something that happened"
        );

        state.replace_details(details(RunStatus::Running, stages(StageStatus::Completed)));
        state.settle_reaction(std::time::Instant::now());
        assert!(state.reacting, "the stage finished and POD did not notice");

        // It ends on its own, without anything else happening.
        state.settle_reaction(std::time::Instant::now() + std::time::Duration::from_secs(1));
        assert!(!state.reacting, "the reaction outlived its window");

        // Looking at a different stage shows a different face, which is not
        // an event. Moved through the real selection path: replace_details
        // restores the index from the selected stage id, so setting the index
        // alone would silently move nothing and prove nothing.
        state.move_stage(false);
        assert_eq!(state.selected_stage_index, 0, "the selection has to move");
        state.replace_details(details(RunStatus::Running, stages(StageStatus::Completed)));
        state.settle_reaction(std::time::Instant::now());
        assert!(
            !state.reacting,
            "POD reacted to the user pressing an arrow key"
        );

        // ... and the face it now wears really is a different one, so the
        // assertion above is about identity rather than about nothing having
        // changed.
        assert_ne!(
            mascot::mascot_state(Some(RunStatus::Running), Some(StageStatus::Failed)),
            mascot::mascot_state(Some(RunStatus::Running), Some(StageStatus::Completed)),
            "the stage moved to has to look different for this to mean anything"
        );

        // Nor to a different run: that is a different thing to watch, not
        // this one changing. Same selected stage name, different run, and a
        // face that genuinely differs from the one POD was wearing.
        let mut elsewhere = details(
            RunStatus::Running,
            vec![stage(
                "architecture",
                StageKind::Architecture,
                Role::Architect,
                StageStatus::Running,
            )],
        );
        elsewhere.id = RunId::from_u128(4);
        state.replace_details(elsewhere);
        state.settle_reaction(std::time::Instant::now());
        assert!(!state.reacting, "POD reacted to a different run entirely");
    }

    /// And the reaction has to reach the screen, inside POD's footprint.
    #[test]
    fn a_reaction_reaches_the_screen_without_moving_anything() {
        const WIDTH: u16 = 160;
        let mut state = running_state();
        let resting = render_symbols(&state, WIDTH, 40);
        state.reacting = true;
        let reacting = render_symbols(&state, WIDTH, 40);

        assert_ne!(resting, reacting, "the reaction never reached a cell");
        let changed: Vec<usize> = resting
            .iter()
            .zip(&reacting)
            .enumerate()
            .filter(|(_, (old, new))| old != new)
            .map(|(index, _)| index)
            .collect();
        assert!(
            changed.len() <= 4,
            "a reaction is four eye cells, not {} cells of the screen",
            changed.len()
        );
        let rows: std::collections::HashSet<usize> =
            changed.iter().map(|index| index / WIDTH as usize).collect();
        assert_eq!(rows.len(), 1, "the whole change lives on POD's eye row");
    }

    /// The property that makes the repeating motion safe to have at all: it
    /// is redundant. A frame with every kind of movement switched off still
    /// names the state in words, so `POLYCODE_MOTION=off` costs the user
    /// nothing, and nobody has to read a blink as evidence of anything.
    #[test]
    fn a_frame_that_never_moves_still_names_the_state() {
        let mut state = running_state();
        state.motion_phase = 0;
        state.reacting = false;
        let text = render_text(&state, 160, 40);
        assert!(
            text.contains("RUNNING"),
            "the run state is not written down"
        );
        assert!(
            text.contains("BUILDING"),
            "the stage's work is not written down"
        );
    }

    /// Motion may repaint a cell; it may never move one. A frame drawn mid
    /// blink and a frame drawn between blinks differ only where POD's own
    /// eyes are, so nothing reflows, no width changes, and no line the user
    /// is reading shifts underneath them.
    #[test]
    fn a_blink_repaints_cells_and_never_moves_them() {
        const WIDTH: u16 = 160;
        let mut resting = running_state();
        resting.motion_phase = 0;
        let mut blinking = running_state();
        // The blink tick of the loop in `motion`, which the prop cycle rests
        // on — so the only cells this frame may repaint are POD's eyes.
        blinking.motion_phase = 5;

        let before = render_symbols(&resting, WIDTH, 40);
        let after = render_symbols(&blinking, WIDTH, 40);
        assert_eq!(before.len(), after.len(), "the grid itself must not move");

        let changed: Vec<usize> = before
            .iter()
            .zip(&after)
            .enumerate()
            .filter(|(_, (old, new))| old != new)
            .map(|(index, _)| index)
            .collect();
        assert!(
            !changed.is_empty(),
            "running work has to look alive on an operating surface"
        );
        assert!(
            changed.len() <= 4,
            "a blink is four eye cells, not {} cells of the screen",
            changed.len()
        );
        let rows: std::collections::HashSet<usize> =
            changed.iter().map(|index| index / WIDTH as usize).collect();
        assert_eq!(rows.len(), 1, "the whole change lives on POD's eye row");
    }

    #[test]
    fn pod_is_seated_under_the_hero_rather_than_floating() {
        let text = render_text(&running_state(), 160, 40);
        let resources = text.find("RESOURCES").unwrap();
        let tail = &text[resources..];
        let rule = tail.find("──────────").expect("POD sits under a rule");
        let pod = tail.find(POD_SHELL).unwrap();
        assert!(rule < pod, "the rule separates the hero from its operator");
    }

    /// The rail's bottom strip narrates the run in plain sentences, and the
    /// sentence follows the run's canonical status rather than the selection.
    #[test]
    fn status_strip_speaks_the_runs_actual_state_in_sentences() {
        let running = render_text(&running_state(), 160, 40);
        assert!(running.contains("STATUS"));
        assert!(
            running.contains("Implementation has been running for"),
            "the live stage is named with its elapsed"
        );
        assert!(running.contains("1 of 3 stages complete."));

        let completed = render_text(&completed_state(), 160, 40);
        assert!(completed.contains("Run complete — the result is ready to review."));
        assert!(completed.contains("1 of 1 stages complete."));

        // A rail too short for the strip spends its rows on the stages.
        assert!(!render_text(&running_state(), 70, 15).contains("STATUS"));
    }

    /// A workflow may complete over an optional stage's failure — the
    /// decision ruled with one review missing. "Run complete" alone would
    /// hide that, so every completed surface names the failure: the strip's
    /// sentence, its stage count, and the hero's tally. What the decision
    /// ruled stays a quote from its own artifact, never this strip's claim.
    #[test]
    fn a_run_completed_over_a_failure_says_so_everywhere_it_counts() {
        // The sentences are asserted at the seam — the rendered rail wraps
        // long lines, so `contains` on the screen text cannot see them whole.
        let sentences = status_sentences(&completed_with_failure_details(), at(12, 13, 0), 160);
        assert_eq!(
            sentences[0],
            "Run complete — Spec review failed, but the decision was still reached. \
             Its verdict is ready to review."
        );
        assert_eq!(sentences[1], "4 of 5 stages complete, 1 failed.");

        let text = render_text(&completed_with_failure_state(), 160, 40);
        assert!(text.contains("✓ 4 of 5 stages completed · Spec review failed"));
        assert!(
            !text.contains("the result is ready to review."),
            "the plain sentence would hide the failure"
        );
    }

    /// A completed stage's badge already reads COMPLETED; the hero does not
    /// repeat it as prose while the run works on.
    #[test]
    fn a_completed_stage_is_not_narrated_as_finished() {
        let mut state = running_state();
        state.selected_stage_index = 0;
        state.selected_stage = Some(StageId::new("architecture").unwrap());
        let text = render_text(&state, 160, 40);
        assert!(text.contains("COMPLETED"), "the badge carries the state");
        assert!(
            !text.contains("Stage finished"),
            "the badge is not repeated as prose"
        );
    }

    #[test]
    fn selection_uses_no_background_so_pod_keeps_its_contrast() {
        // M13d.1 noted that POD's solid eyes lose contrast on a
        // background-highlighted row. The rail marks selection with a cursor
        // and modifiers only, so nothing in the left column paints a surface.
        for status in [
            StageStatus::Running,
            StageStatus::Completed,
            StageStatus::Pending,
            StageStatus::NeedsUser,
            StageStatus::Failed,
        ] {
            for selected in [true, false] {
                assert!(
                    stage_name_style(status, selected).bg.is_none(),
                    "{status:?} selected={selected} paints a background"
                );
            }
        }
        let state = running_state();
        let backend = TestBackend::new(160, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rail = 160_u16 * 38 / 100;
        for y in 2..38 {
            for x in 0..rail {
                // POD's sprite is the one legitimate surface: a `▀` cell
                // carries its bottom pixel in the background. Everything
                // else in the rail stays unpainted.
                if buffer[(x, y)].symbol() == "▀" {
                    continue;
                }
                assert_eq!(
                    buffer[(x, y)].bg,
                    ratatui::style::Color::Reset,
                    "the pipeline rail stays unpainted at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn state_is_never_carried_by_color_alone() {
        // Each state pairs its color with a glyph and a word, so a monochrome
        // terminal reads the same interface.
        for (status, word) in [
            (StageStatus::Running, "RUNNING"),
            (StageStatus::NeedsUser, "NEEDS YOU"),
            (StageStatus::Failed, "FAILED"),
            (StageStatus::Completed, "COMPLETED"),
        ] {
            let visual = stage_visual(status);
            assert_eq!(visual.label, word);
            assert!(!visual.glyph.is_empty());
        }
        let mut state = running_state();
        state.details.as_mut().unwrap().status = RunStatus::Failed;
        state.details.as_mut().unwrap().stages[1].status = StageStatus::Failed;
        let text = render_text(&state, 160, 40);
        assert!(text.contains("✗ FAILED"), "glyph and word travel together");
    }

    #[test]
    fn footer_shapes_follow_canonical_state() {
        let running = actions_text(&running_state());
        assert!(running.starts_with("[o] Result"));
        assert!(!running.contains("Apply") && !running.contains("Resolve"));

        let completed = actions_text(&completed_state());
        assert!(completed.starts_with("[d] Review diff"));
        assert!(!completed.contains("[o] Result"), "review, not monitoring");

        let mut technical = running_state();
        technical.technical = true;
        assert!(
            actions_text(&technical).contains("[i] Operational"),
            "the toggle names the mode it leads to"
        );
    }

    #[test]
    fn operational_information_survives_every_supported_size() {
        for (width, height) in [(160, 40), (120, 35), (100, 30), (80, 26), (70, 24)] {
            let text = render_text(&running_state(), width, height);
            assert!(
                text.contains("RUNNING"),
                "status survives at {width}x{height}"
            );
            assert!(
                text.contains("Implementation") || text.contains("IMPLEMENTATION"),
                "current stage survives at {width}x{height}"
            );
            assert!(
                text.contains("[o] Result"),
                "the primary action survives at {width}x{height}"
            );
            assert!(
                text.contains("2m 14s") || text.contains("14s"),
                "elapsed survives at {width}x{height}"
            );
        }
        // POD is the first thing to go, and only after operational content fits.
        assert!(render_text(&running_state(), 160, 40).contains(POD_SHELL));
        assert!(!render_text(&running_state(), 70, 24).contains(POD_SHELL));
    }

    #[test]
    fn narrow_attention_keeps_the_state_action() {
        let mut state = running_state();
        let details = state.details.as_mut().unwrap();
        details.status = RunStatus::NeedsUser;
        details.stages[1].status = StageStatus::NeedsUser;
        details.attention = vec![crate::app::AttentionSummary {
            id: crate::domain::AttentionRequestId::from_u128(1),
            stage_id: StageId::new("implementation").unwrap(),
            kind: AttentionKind::Permission,
            summary: "Claude requests permission to use Bash".to_owned(),
        }];
        let text = render_text(&state, 70, 24);
        assert!(
            text.contains("ACTION REQUIRED"),
            "attention cannot be missed"
        );
        assert!(text.contains("[u] Resolve attention"));
    }

    #[test]
    fn attention_overlay_distinguishes_permission_from_question() {
        let mut state = running_state();
        let details = state.details.as_mut().unwrap();
        details.attention = vec![crate::app::AttentionSummary {
            id: crate::domain::AttentionRequestId::from_u128(1),
            stage_id: StageId::new("implementation").unwrap(),
            kind: AttentionKind::Permission,
            summary: "allow network access".to_owned(),
        }];
        state.overlay = Some(Overlay::Attention);
        let text = render_text(&state, 120, 30);
        assert!(text.contains("Permission request"));
        assert!(text.contains("Enter approve/resolve"));
        assert!(
            text.contains("Response (optional):"),
            "permission offers an answer that continues without granting"
        );

        state.details.as_mut().unwrap().attention[0].kind = AttentionKind::Question;
        let text = render_text(&state, 120, 30);
        assert!(
            text.contains("Response:"),
            "question keeps editable response"
        );
        assert!(text.contains("Enter submit"));
    }

    fn update_info(current: &str, available: &str) -> crate::update::UpdateInfo {
        crate::update::UpdateInfo {
            current_version: semver::Version::parse(current).unwrap(),
            available_version: semver::Version::parse(available).unwrap(),
            tag: format!("v{available}"),
            release_url: "https://example.invalid/r".to_owned(),
            published_at: None,
        }
    }

    #[test]
    fn no_update_overlay_exists_without_an_available_release() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        assert!(
            !state.update_prompt_is_due(),
            "nothing is offered before a check concludes"
        );
        // A concluded check that found nothing leaves the field empty.
        state.update = None;
        state.overlay = Some(Overlay::Update);
        let text = render_text(&state, 120, 30);
        assert!(
            !text.contains("UPDATE AVAILABLE"),
            "the overlay renders nothing without an update"
        );
    }

    #[test]
    fn an_available_update_prompts_with_both_versions() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.update = Some(update_info("0.1.0", "0.2.0"));
        state.update_install = Some(crate::update::InstallSource::OfficialBinary);
        assert!(state.update_prompt_is_due());
        state.overlay = Some(Overlay::Update);
        let text = render_text(&state, 120, 30);
        assert!(text.contains("UPDATE AVAILABLE"));
        assert!(text.contains("0.1.0"));
        assert!(text.contains("0.2.0"));
        assert!(text.contains("Install now?"));
        assert!(text.contains("It applies when Polycode restarts."));
        assert!(text.contains("→ Yes"), "the default answer is highlighted");
        assert!(text.contains("No"));
    }

    #[test]
    fn an_unsupported_installation_is_told_the_truth() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.update = Some(update_info("0.1.0", "0.2.0"));
        state.update_install = Some(crate::update::InstallSource::Source);
        state.overlay = Some(Overlay::Update);
        let text = render_text(&state, 120, 30);
        assert!(text.contains("UPDATE AVAILABLE"));
        assert!(text.contains("managed from source"));
        assert!(
            !text.contains("Install now?"),
            "an install that cannot happen is never offered"
        );
    }

    #[test]
    fn the_update_prompt_yields_to_run_attention_and_stays_on_the_runs_screen() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.update = Some(update_info("0.1.0", "0.2.0"));
        assert!(state.update_prompt_is_due(), "quiet Runs screen");

        state.screen = Screen::RunDetail;
        assert!(!state.update_prompt_is_due(), "never on the mission deck");
        state.screen = Screen::Runs;

        state.overlay = Some(Overlay::Attention);
        assert!(!state.update_prompt_is_due(), "never over another overlay");
        state.overlay = None;

        state.begin_action(
            crate::tui::worker::ActionKind::Apply,
            Some(RunId::from_u128(1)),
        );
        assert!(!state.update_prompt_is_due(), "never during an action");
        state.settle_action(
            crate::tui::worker::ActionKind::Apply,
            Some(RunId::from_u128(1)),
        );

        state.replace_runs(vec![RunListItem {
            id: RunId::from_u128(1),
            workflow: WorkflowKind::Standard,
            status: RunStatus::NeedsUser,
            task_summary: "OAuth".to_owned(),
            repository: None,
            updated_at: at(12, 0, 0),
            hidden: false,
        }]);
        assert!(
            !state.update_prompt_is_due(),
            "a run that needs the user outranks a software update"
        );
    }

    #[test]
    fn a_dismissed_update_never_reopens_in_this_process() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.update = Some(update_info("0.1.0", "0.2.0"));
        state.update_dismissed = true;
        assert!(!state.update_prompt_is_due());
    }

    #[test]
    fn the_update_prompt_uses_mission_deck_theme_and_fits_narrow_terminals() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        state.update = Some(update_info("0.1.0", "0.2.0"));
        state.update_install = Some(crate::update::InstallSource::OfficialBinary);
        state.overlay = Some(Overlay::Update);
        for (width, height) in [(160, 40), (100, 30), (70, 24), (50, 12)] {
            let text = render_text(&state, width, height);
            assert!(
                text.contains("UPDATE AVAILABLE"),
                "prompt survives {width}x{height}"
            );
            assert!(
                text.contains("0.2.0"),
                "the new version survives {width}x{height}"
            );
        }
        // The band is an aside, never a takeover.
        let band = update_rect(
            Rect::new(0, 0, 160, 40),
            &[Line::from("one"), Line::from("two")],
        );
        assert!(band.width <= 62 && band.height <= 12);
        assert!(band.x > 0 && band.y > 0);

        // A long guidance line wraps, and the band grows so the action row
        // stays visible instead of being clipped.
        let tall = update_rect(
            Rect::new(0, 0, 160, 40),
            &[Line::from("x".repeat(200)), Line::from("[Enter] Continue")],
        );
        assert!(
            tall.height > band.height,
            "wrapped content is accounted for"
        );
    }

    #[test]
    fn confirmation_overlays_keep_existing_semantics() {
        let mut state = completed_state();
        state.overlay = Some(Overlay::ApplyConfirm);
        assert!(render_text(&state, 120, 30).contains("Enter confirms apply"));
        state.overlay = Some(Overlay::DiscardConfirm);
        assert!(render_text(&state, 120, 30).contains("Enter confirms discard"));
    }

    #[test]
    fn retry_route_chooser_lists_the_configured_provider_first() {
        let mut state = completed_state();
        state.retry_route_choice = RetryRouteChoice::Claude;
        state.overlay = Some(Overlay::RetryRoute);
        let text = render_text(&state, 120, 30);
        assert!(text.contains("Retry stage"), "{text}");
        assert!(text.contains("Configured provider ("), "{text}");
        assert!(text.contains("→ Claude (native default model)"), "{text}");
        assert!(text.contains("Codex (native default model)"), "{text}");
        assert!(text.contains("Only this stage moves"), "{text}");
    }

    #[test]
    fn runtime_summary_marks_an_operator_override() {
        let mut overridden = stage(
            "implementation",
            StageKind::Implementation,
            Role::Implementer,
            StageStatus::Pending,
        );
        overridden.configured_provider = "claude".to_owned();
        overridden.route_overridden = true;
        assert!(runtime_summary(&overridden).starts_with("claude · native default (override)"));
    }
}
