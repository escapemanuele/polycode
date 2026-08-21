use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::app::{QuiescentState, RunDetails, StageSummary};
use crate::domain::{RunStatus, StageStatus};

use super::state::{Overlay, Screen, TuiState};

const MIN_WIDTH: u16 = 50;
const MIN_HEIGHT: u16 = 10;

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
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(2),
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

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let title = match state.screen {
        Screen::Runs => "POLYCODE · RUNS CONTROL ROOM",
        Screen::RunDetail => "POLYCODE · RUN DETAIL",
        Screen::Artifact => "POLYCODE · VERIFIED ARTIFACT",
        Screen::Logs => "POLYCODE · RAW LOGS",
        Screen::Diff => "POLYCODE · WORKSPACE DIFF",
        Screen::NewRun => "POLYCODE · NEW RUN",
    };
    let busy = state
        .worker_busy
        .as_deref()
        .map_or(String::new(), |busy| format!("  ·  {busy}…"));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(busy, Style::default().fg(Color::Cyan)),
        ]))
        .block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn render_runs(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    if state.runs.is_empty() {
        frame.render_widget(
            Paragraph::new("No runs yet.\n\n[n] Start your first run")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title(" Runs ")),
            area,
        );
        return;
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);
    let items = state.runs.iter().enumerate().map(|(index, run)| {
        let selected = index == state.selected_run_index;
        let line = Line::from(vec![
            Span::styled(
                format!("{} ", run_glyph(run.status)),
                status_style(run.status),
            ),
            Span::styled(
                format!("{:<8} ", enum_text(run.workflow)),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(&run.task_summary),
        ]);
        ListItem::new(line).style(if selected {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        })
    });
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(" Runs ")),
        columns[0],
    );
    if let Some(details) = state.details.as_ref() {
        render_run_overview(frame, columns[1], details);
    } else {
        frame.render_widget(
            Paragraph::new("Loading selected run…").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Selected run "),
            ),
            columns[1],
        );
    }
}

fn render_run_overview(frame: &mut Frame<'_>, area: Rect, details: &RunDetails) {
    let mut lines = vec![
        status_heading(details.status),
        Line::from(Span::styled(
            details
                .task
                .as_deref()
                .unwrap_or("<legacy input unavailable>"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "{} · {} ({})",
            enum_text(details.workflow),
            details.profile,
            details.profile_version
        )),
        Line::from(""),
    ];
    for stage in &details.stages {
        lines.push(stage_line(stage));
    }
    if details.status == RunStatus::Completed
        && details.workflow != crate::domain::WorkflowKind::Review
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "READY TO REVIEW  [d] diff  [a] apply  [X] discard",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Selected run "),
        ),
        area,
    );
}

fn render_detail(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let Some(details) = state.details.as_ref() else {
        frame.render_widget(Paragraph::new("Run unavailable"), area);
        return;
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(area);
    let mut left = vec![status_heading(details.status)];
    if let Some(QuiescentState::WaitingForProvider { stage_id }) = state.quiescent.as_ref() {
        let provider = details
            .stages
            .iter()
            .find(|stage| &stage.id == stage_id)
            .map_or("provider", |stage| stage.configured_provider.as_str());
        left.push(Line::from(Span::styled(
            format!("Waiting for {provider}…"),
            Style::default().fg(Color::Cyan),
        )));
    }
    if !details.attention.is_empty() {
        left.push(Line::from(Span::styled(
            "⚠ NEEDS YOU  [u] open attention",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    }
    if details.status == RunStatus::Completed
        && details.workflow != crate::domain::WorkflowKind::Review
    {
        left.push(Line::from(Span::styled(
            "READY TO REVIEW",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
    }
    left.push(Line::from(""));
    for (index, stage) in details.stages.iter().enumerate() {
        let mut line = stage_line(stage);
        if index == state.selected_stage_index {
            line = line.style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );
        }
        left.push(line);
    }
    frame.render_widget(
        Paragraph::new(left).block(Block::default().borders(Borders::ALL).title(format!(
            " {} · {} ",
            enum_text(details.workflow),
            short_id(&details.id.to_string())
        ))),
        columns[0],
    );
    render_stage_context(frame, columns[1], state, details);
}

#[allow(
    clippy::too_many_lines,
    reason = "single stage panel keeps configured, actual, runtime, and route data aligned"
)]
fn render_stage_context(frame: &mut Frame<'_>, area: Rect, state: &TuiState, details: &RunDetails) {
    let Some(selected) = details.stages.get(state.selected_stage_index) else {
        frame.render_widget(
            Paragraph::new("No stage selected")
                .block(Block::default().borders(Borders::ALL).title(" Stage ")),
            area,
        );
        return;
    };
    let lines = vec![
        Line::from(vec![
            Span::raw("Stage       "),
            Span::styled(selected.id.to_string(), Modifier::BOLD),
        ]),
        Line::from(format!("Kind        {}", enum_text(selected.kind))),
        Line::from(format!("Role        {}", enum_text(selected.role))),
        Line::from(vec![
            Span::raw("Status      "),
            Span::styled(enum_text(selected.status), stage_style(selected.status)),
        ]),
        Line::from(""),
        Line::from(format!(
            "Configured  {} / {}",
            selected.configured_provider,
            selected
                .configured_model
                .as_deref()
                .unwrap_or("native default")
        )),
        Line::from(format!("Effort      {}", selected.requested_effort.label())),
        Line::from(format!(
            "Actual      {} / {}",
            selected.actual_provider.as_deref().unwrap_or("not started"),
            selected.actual_model.as_deref().unwrap_or("unconfirmed")
        )),
        Line::from(format!(
            "Session     {}",
            selected
                .provider_session_status
                .as_deref()
                .unwrap_or("unavailable")
        )),
        Line::from(format!(
            "Native      {}",
            selected
                .native_session
                .as_deref()
                .map_or("unavailable", short_id)
        )),
        Line::from(format!(
            "Process     {}",
            selected.process_status.as_deref().unwrap_or("unavailable")
        )),
        Line::from(""),
        Line::from(format!(
            "Profile     {} ({})",
            details.profile, details.profile_version
        )),
        Line::from(format!("Run         {}", details.id)),
        Line::from({
            use std::fmt::Write as _;
            // Provider-native units; optional dimensions appear only when the
            // runtime reported them.
            let mut usage = format!(
                "Usage       {} in / {} out",
                details.usage.input_units, details.usage.output_units
            );
            for (label, value) in [
                ("cache read", details.usage.cache_read_units),
                ("cache write", details.usage.cache_write_units),
                ("reasoning out", details.usage.reasoning_output_units),
            ] {
                if let Some(value) = value {
                    let _ = write!(usage, " / {value} {label}");
                }
            }
            usage
        }),
        Line::from(format!(
            "Workspace   {}",
            details
                .workspace_status
                .map_or("unavailable".to_owned(), |status| format!("{status:?}")
                    .to_lowercase())
        )),
        Line::from(format!(
            "Repository  {}",
            details
                .repository
                .as_deref()
                .map_or("unavailable".to_owned(), |path| path.display().to_string())
        )),
        Line::from(""),
        Line::from(Span::styled("Persisted routes", Modifier::BOLD)),
    ];
    let mut lines = lines;
    for route in &details.routes {
        lines.push(Line::from(format!(
            "{} → {}/{} ({})",
            enum_text(route.role),
            route.configured_provider,
            route
                .configured_model
                .as_deref()
                .unwrap_or("native default"),
            route.reason
        )));
    }
    lines.extend([
        Line::from(""),
        Line::from("[o] artifact  [l] logs  [d] diff"),
    ]);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", enum_text(selected.kind))),
        ),
        area,
    );
}

fn render_artifact(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let (title, text) = state.artifact.as_ref().map_or_else(
        || (" Artifact ".to_owned(), "Artifact unavailable".to_owned()),
        |artifact| {
            (
                format!(
                    " {} · attempt {} ",
                    artifact.summary.stage_id, artifact.summary.attempt
                ),
                artifact.text.clone(),
            )
        },
    );
    frame.render_widget(
        Paragraph::new(text)
            .scroll((state.scroll, 0))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn render_logs(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let text = state.logs.as_ref().map_or_else(
        || Text::from("Logs unavailable"),
        |logs| {
            Text::from(vec![
                Line::from(Span::styled("STDOUT", Modifier::BOLD)),
                Line::from(if logs.stdout.truncated {
                    "[tail truncated]"
                } else {
                    ""
                }),
                Line::from(logs.stdout.text.clone()),
                Line::from(""),
                Line::from(Span::styled("STDERR", Modifier::BOLD)),
                Line::from(if logs.stderr.truncated {
                    "[tail truncated]"
                } else {
                    ""
                }),
                Line::from(logs.stderr.text.clone()),
            ])
        },
    );
    let title = state.logs.as_ref().map_or_else(
        || " Raw retained output · read-only ".to_owned(),
        |logs| {
            format!(
                " Raw retained output · {} · {} · read-only ",
                short_id(&logs.process_id.to_string()),
                logs.process_status
            )
        },
    );
    frame.render_widget(
        Paragraph::new(text)
            .scroll((state.scroll, 0))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn render_diff(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let lines = state.diff.as_ref().map_or_else(
        || vec![Line::from("Diff unavailable")],
        |diff| {
            let mut lines = Vec::new();
            if diff.truncated {
                lines.push(Line::from(Span::styled(
                    format!(
                        "[preview truncated at 2 MiB; total {} bytes]",
                        diff.total_bytes
                    ),
                    Style::default().fg(Color::Yellow),
                )));
            }
            for line in diff.text.lines() {
                let style = if line.starts_with("+++") || line.starts_with("---") {
                    Style::default().fg(Color::Cyan)
                } else if line.starts_with('+') {
                    Style::default().fg(Color::Green)
                } else if line.starts_with('-') {
                    Style::default().fg(Color::Red)
                } else if line.starts_with("diff --git") || line.starts_with("@@") {
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::styled(line.to_owned(), style));
            }
            lines
        },
    );
    frame.render_widget(
        Paragraph::new(lines).scroll((state.scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Unified diff · read-only "),
        ),
        area,
    );
}

fn render_new_run(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let form = &state.new_run;
    let workflow = enum_text(form.workflow);
    let task = field_display(&form.task, form.focus == 0);
    let repository = field_display(&form.repository, form.focus == 2);
    let lines = vec![
        form_line("Task", &task, form.focus == 0),
        Line::from(""),
        form_line("Workflow", &workflow, form.focus == 1),
        Line::from(""),
        form_line("Repository", &repository, form.focus == 2),
        Line::from(""),
        form_line("Execution", form.execution.label(), form.focus == 3),
        Line::from(""),
        form_line("Effort", form.effort.label(), form.focus == 4),
        Line::from(""),
        Line::from("Tab/Shift-Tab fields · ←/→ choices · Enter start · Esc cancel"),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" New run ")),
        area,
    );
}

fn form_line<'a>(label: &'a str, value: &'a str, selected: bool) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{label:<12}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            value,
            if selected {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            },
        ),
    ])
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let default = match state.screen {
        Screen::Runs => "↑↓/jk navigate  Enter open  n new  ? help  q quit/detach",
        Screen::RunDetail => {
            "↑↓/jk stage  r resume  t retry  u attention  o artifact  l logs  d diff  a apply  X discard"
        }
        Screen::Artifact | Screen::Logs | Screen::Diff => {
            "↑↓ PgUp/PgDn scroll  Esc back  q quit/detach"
        }
        Screen::NewRun => "Tab fields  ←→ choices/edit  Enter start  Esc cancel",
    };
    let (text, style) = state.message.as_ref().map_or(
        (default.to_owned(), Style::default().fg(Color::DarkGray)),
        |message| {
            (
                message.text.clone(),
                if message.persistent {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Green)
                },
            )
        },
    );
    frame.render_widget(
        Paragraph::new(text)
            .style(style)
            .block(Block::default().borders(Borders::TOP)),
        area,
    );
}

fn render_overlay(frame: &mut Frame<'_>, area: Rect, state: &TuiState, overlay: Overlay) {
    let popup = centered_rect(78, 70, area);
    frame.render_widget(Clear, popup);
    match overlay {
        Overlay::Help => frame.render_widget(
            Paragraph::new(
                "Global\n  ↑/↓ or j/k  navigate\n  Enter        open/confirm\n  Esc          back/close\n  n            new run\n  R            runs screen\n  ?            help\n  q / Ctrl-C   quit/detach\n\nRun\n  r resume/recover\n  t retry selected failed stage\n  u resolve selected attention\n  o verified artifact\n  l raw logs (read-only)\n  d workspace diff (read-only)\n  a apply (confirmation)\n  X discard (confirmation)",
            )
            .block(Block::default().borders(Borders::ALL).title(" Help · Esc closes ")),
            popup,
        ),
        Overlay::Attention => render_attention(frame, popup, state),
        Overlay::ApplyConfirm => render_confirmation(frame, popup, state, true),
        Overlay::DiscardConfirm => render_confirmation(frame, popup, state, false),
    }
}

fn render_attention(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let Some(details) = state.details.as_ref() else {
        return;
    };
    let mut lines = vec![Line::from(Span::styled(
        "⚠ NEEDS YOU",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))];
    for (index, attention) in details.attention.iter().enumerate() {
        lines.push(Line::styled(
            format!(
                "{} {} · {} · {}",
                if index == state.attention_index {
                    ">"
                } else {
                    " "
                },
                attention.stage_id,
                enum_text(attention.kind),
                attention.summary
            ),
            if index == state.attention_index {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            },
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "Response: {}",
        field_display(&state.attention_response, true)
    )));
    lines.push(Line::from(
        "↑/↓ select request · type response if required · Enter resolve · Esc cancel",
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Attention ")),
        area,
    );
}

fn render_confirmation(frame: &mut Frame<'_>, area: Rect, state: &TuiState, apply: bool) {
    let Some(details) = state.details.as_ref() else {
        return;
    };
    let action = if apply { "APPLY" } else { "DISCARD" };
    let mut lines = vec![
        Line::from(Span::styled(
            format!("Confirm {action}"),
            Style::default()
                .fg(if apply { Color::Green } else { Color::Red })
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("Run:  {}", details.id)),
        Line::from(format!(
            "Task: {}",
            details
                .task
                .as_deref()
                .unwrap_or("<legacy input unavailable>")
        )),
        Line::from(format!(
            "Repo: {}",
            details
                .repository
                .as_deref()
                .map_or("unavailable".to_owned(), |path| path.display().to_string())
        )),
    ];
    if apply {
        if let Some(diff) = state.diff.as_ref() {
            lines.push(Line::from(format!("Files: {}", diff.changed_files.len())));
            for file in diff.changed_files.iter().take(8) {
                lines.push(Line::from(format!(
                    "  {}{}",
                    file.path,
                    if file.binary { " [binary]" } else { "" }
                )));
            }
        }
        lines.push(Line::from(
            "Review [d] diff first when needed. Enter confirms apply.",
        ));
    } else {
        lines.push(Line::from(
            "Discard is logical disposition; owned cleanup follows application semantics.",
        ));
        lines.push(Line::from("Enter confirms discard."));
    }
    lines.push(Line::from("Esc cancels"));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {action} ")),
        ),
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

fn stage_line(stage: &StageSummary) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{} ", stage_glyph(stage.status)),
            stage_style(stage.status),
        ),
        Span::raw(format!("{:<22}", enum_text(stage.kind))),
        Span::styled(
            stage.configured_provider.clone(),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

fn status_heading(status: RunStatus) -> Line<'static> {
    let label = match status {
        RunStatus::NeedsUser => "⚠ NEEDS YOU",
        RunStatus::Interrupted => "↻ INTERRUPTED",
        RunStatus::Paused => "‖ PAUSED",
        RunStatus::Failed => "✗ FAILED",
        RunStatus::Completed => "✓ COMPLETED",
        RunStatus::Applied => "✓ APPLIED",
        RunStatus::Running => "● RUNNING",
        RunStatus::Created | RunStatus::Preparing | RunStatus::Ready => "○ WAITING",
        RunStatus::Discarded => "DISCARDED",
    };
    Line::from(Span::styled(
        label,
        status_style(status).add_modifier(Modifier::BOLD),
    ))
}

const fn run_glyph(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Completed | RunStatus::Applied => "✓",
        RunStatus::Running => "●",
        RunStatus::NeedsUser => "⚠",
        RunStatus::Failed => "✗",
        RunStatus::Paused => "‖",
        RunStatus::Interrupted => "↻",
        RunStatus::Created | RunStatus::Preparing | RunStatus::Ready => "○",
        RunStatus::Discarded => "×",
    }
}

const fn stage_glyph(status: StageStatus) -> &'static str {
    match status {
        StageStatus::Completed => "✓",
        StageStatus::Running => "●",
        StageStatus::NeedsUser => "⚠",
        StageStatus::Failed => "✗",
        StageStatus::Paused => "‖",
        StageStatus::Interrupted => "↻",
        StageStatus::Pending | StageStatus::Ready | StageStatus::Skipped => "○",
    }
}

const fn status_style(status: RunStatus) -> Style {
    match status {
        RunStatus::Completed | RunStatus::Applied => Style::new().fg(Color::Green),
        RunStatus::Running => Style::new().fg(Color::Cyan),
        RunStatus::NeedsUser | RunStatus::Ready => Style::new().fg(Color::Yellow),
        RunStatus::Failed => Style::new().fg(Color::Red),
        RunStatus::Paused | RunStatus::Interrupted => Style::new().fg(Color::Magenta),
        RunStatus::Created | RunStatus::Preparing | RunStatus::Discarded => {
            Style::new().fg(Color::DarkGray)
        }
    }
}

const fn stage_style(status: StageStatus) -> Style {
    match status {
        StageStatus::Completed => Style::new().fg(Color::Green),
        StageStatus::Running => Style::new().fg(Color::Cyan),
        StageStatus::NeedsUser | StageStatus::Ready => Style::new().fg(Color::Yellow),
        StageStatus::Failed => Style::new().fg(Color::Red),
        StageStatus::Paused | StageStatus::Interrupted => Style::new().fg(Color::Magenta),
        StageStatus::Pending | StageStatus::Skipped => Style::new().fg(Color::DarkGray),
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
    use crate::app::{RouteSummary, RunListItem, StageSummary, UsageSummary};
    use crate::domain::{Role, RunId, StageId, StageKind, WorkflowKind};

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

    #[test]
    fn empty_runs_and_small_terminal_render_without_panicking() {
        let state = TuiState::new(std::path::Path::new("/repo"));
        assert!(render_text(&state, 90, 24).contains("No runs yet"));
        assert!(render_text(&state, 49, 9).contains("Terminal too small"));
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
            updated_at: Utc
                .with_ymd_and_hms(2026, 8, 17, 12, 0, 0)
                .single()
                .unwrap(),
        }]);
        state.details = Some(RunDetails {
            id,
            task: Some("OAuth provider".to_owned()),
            workflow: WorkflowKind::Standard,
            status: RunStatus::NeedsUser,
            repository: Some(std::path::PathBuf::from("/repo")),
            workspace_status: Some(crate::workspace::WorkspaceStatus::Ready),
            base_commit: Some("abc".to_owned()),
            profile: "recommended".to_owned(),
            profile_version: "recommended_v1".to_owned(),
            routes: Vec::new(),
            revision: crate::store::RunRevision::initial(),
            created_at: Utc
                .with_ymd_and_hms(2026, 8, 17, 12, 0, 0)
                .single()
                .unwrap(),
            updated_at: Utc
                .with_ymd_and_hms(2026, 8, 17, 12, 1, 0)
                .single()
                .unwrap(),
            stages: Vec::new(),
            attention: Vec::new(),
            usage: UsageSummary::default(),
        });
        let text = render_text(&state, 120, 30);
        assert!(text.contains("OAuth provider"));
        assert!(text.contains("NEEDS YOU"));
        state.overlay = Some(Overlay::Help);
        assert!(render_text(&state, 120, 30).contains("Help · Esc closes"));
    }

    #[test]
    fn detail_renders_ready_review_specialized_stages_routes_and_actual_target() {
        let mut state = TuiState::new(std::path::Path::new("/repo"));
        let id = RunId::from_u128(2);
        let make_summary = |id: &str, kind, role| StageSummary {
            requested_effort: crate::domain::EffortSetting::NativeDefault,
            id: StageId::new(id).unwrap(),
            kind,
            role,
            status: StageStatus::Completed,
            configured_provider: "codex".to_owned(),
            configured_model: None,
            actual_provider: Some("codex".to_owned()),
            actual_model: Some("gpt-fixture".to_owned()),
            provider_session_record: Some("session-record".to_owned()),
            native_session: Some("native-session".to_owned()),
            provider_session_status: Some("completed".to_owned()),
            process_status: Some("exited".to_owned()),
        };
        state.screen = Screen::RunDetail;
        state.selected_run = Some(id);
        state.replace_details(RunDetails {
            id,
            task: Some("Completed implementation".to_owned()),
            workflow: WorkflowKind::Standard,
            status: RunStatus::Completed,
            repository: Some(std::path::PathBuf::from("/repo")),
            workspace_status: Some(crate::workspace::WorkspaceStatus::Ready),
            base_commit: Some("abc".to_owned()),
            profile: "recommended".to_owned(),
            profile_version: "recommended_v1".to_owned(),
            routes: vec![RouteSummary {
                role: Role::Implementer,
                configured_provider: "codex".to_owned(),
                configured_model: None,
                reason: "recommended_role_assignment".to_owned(),
            }],
            revision: crate::store::RunRevision::initial(),
            created_at: Utc
                .with_ymd_and_hms(2026, 8, 17, 12, 0, 0)
                .single()
                .unwrap(),
            updated_at: Utc
                .with_ymd_and_hms(2026, 8, 17, 12, 1, 0)
                .single()
                .unwrap(),
            stages: vec![
                make_summary(
                    "quality_review",
                    StageKind::CodeQualityReview,
                    Role::CodeQualityReviewer,
                ),
                make_summary("spec_review", StageKind::SpecReview, Role::SpecReviewer),
            ],
            attention: Vec::new(),
            usage: UsageSummary {
                input_units: 123,
                output_units: 14,
                ..UsageSummary::default()
            },
        });

        let text = render_text(&state, 160, 40);
        assert!(text.contains("READY TO REVIEW"));
        assert!(text.contains("code_quality_review"));
        assert!(text.contains("spec_review"));
        assert!(text.contains("Configured  codex / native default"));
        assert!(text.contains("Actual      codex / gpt-fixture"));
        assert!(text.contains("recommended_role_assignment"));
    }
}
