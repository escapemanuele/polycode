use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use chrono::{DateTime, Utc};

use crate::app::{RunDetails, StageSummary};
use crate::domain::{AttentionKind, RunStatus, StageKind, StageStatus};

use super::state::{Overlay, Screen, TuiState, UiMessageKind};
use super::{format, markdown, mascot};

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
        let mut lines = Vec::new();
        // Decoration yields to content: the mascot appears only when the
        // empty state has room for it.
        if area.width >= 60 && area.height >= 14 {
            lines.extend(mascot::mascot_lines(mascot::MascotState::Idle, None));
            lines.push(Line::from(""));
        }
        lines.push(Line::from("No runs yet."));
        lines.push(Line::from(""));
        lines.push(Line::from("[n] Start your first run"));
        frame.render_widget(
            Paragraph::new(lines)
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
    render_pipeline(frame, columns[0], state, details);
    if state.technical {
        render_technical(frame, columns[1], state, details);
    } else {
        render_hero(frame, columns[1], state, details);
    }
}

/// Left rail: the run's stages in workflow order with their semantic
/// durations, and POD in whatever room is left over.
fn render_pipeline(frame: &mut Frame<'_>, area: Rect, state: &TuiState, details: &RunDetails) {
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    let mut lines = vec![status_heading(details.status)];
    // Operational identity: what this run is and where, without the full
    // path — technical mode keeps that.
    lines.push(Line::from(Span::styled(
        format::truncate_title(
            details.task.as_deref().unwrap_or("<legacy input>"),
            area.width.saturating_sub(4) as usize,
        ),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    let mut identity = details.repository.as_deref().map_or_else(
        || enum_text(details.workflow),
        |path| {
            format!(
                "{} · {}",
                enum_text(details.workflow),
                format::repository_name(path)
            )
        },
    );
    if let Some(span) = format::elapsed(details.started_at, details.finished_at, now) {
        identity.push_str(" · ");
        identity.push_str(&format::format_duration(span));
    }
    lines.push(Line::from(Span::styled(
        identity,
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));
    // Connector segments cost one row per gap; they are the first thing to
    // go when the rail is short.
    let stage_rows = details.stages.len() * 2 - details.stages.len().min(1);
    let connectors = area.height as usize > stage_rows + 6;
    for (index, stage) in details.stages.iter().enumerate() {
        if connectors && index > 0 {
            lines.push(Line::from(Span::styled(
                "  │",
                Style::default().fg(Color::DarkGray),
            )));
        }
        let mut line = pipeline_line(stage, area.width, now);
        if index == state.selected_stage_index {
            line = line.style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );
        }
        lines.push(line);
    }
    let inner_height = area.height.saturating_sub(2) as usize;
    let mascot_rows = mascot::MASCOT_HEIGHT as usize;
    if area.width >= mascot::MASCOT_WIDTH + 20 && inner_height > lines.len() + mascot_rows {
        while lines.len() < inner_height - mascot_rows {
            lines.push(Line::from(""));
        }
        let selected = details.stages.get(state.selected_stage_index);
        lines.extend(mascot::mascot_lines(
            mascot::mascot_state(Some(details.status), selected.map(|stage| stage.status)),
            selected.map(|stage| mascot::mascot_activity(stage.role)),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" PIPELINE ")
                .title_style(Style::default().add_modifier(Modifier::BOLD)),
        ),
        area,
    );
}

/// One rail row: status glyph, human stage name, and duration when the run
/// carries evidence for one. Pending stages show no fabricated `0s`.
fn pipeline_line(stage: &StageSummary, width: u16, now: DateTime<Utc>) -> Line<'static> {
    let name = stage_title(stage.kind);
    let duration = format::elapsed(stage.started_at, stage.finished_at, now)
        .map(format::format_duration)
        .unwrap_or_default();
    // Narrow rails drop the duration column rather than wrapping the name.
    let name_width = (width as usize).saturating_sub(duration.len() + 6);
    Line::from(vec![
        Span::styled(
            format!("{} ", stage_glyph(stage.status)),
            stage_style(stage.status),
        ),
        Span::raw(format!("{name:<name_width$}")),
        Span::styled(duration, Style::default().fg(Color::DarkGray)),
    ])
}

/// Right panel, operational view: what is happening, for how long, on which
/// runtime, what needs the user, what came out, and what can be done next.
fn render_hero(frame: &mut Frame<'_>, area: Rect, state: &TuiState, details: &RunDetails) {
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    let Some(selected) = details.stages.get(state.selected_stage_index) else {
        frame.render_widget(
            Paragraph::new("No stage selected")
                .block(Block::default().borders(Borders::ALL).title(" STAGE ")),
            area,
        );
        return;
    };
    let applyable = state.run_is_applyable();
    let mut lines = Vec::new();
    if applyable {
        lines.extend(completed_hero(details, now));
    } else {
        lines.extend(stage_hero(selected, now));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        runtime_summary(selected),
        Style::default(),
    )));
    // Attention outranks every remaining section.
    if let Some(attention) = details.attention.first() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "⚠ ACTION REQUIRED",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            attention.summary.clone(),
            Style::default().fg(Color::Yellow),
        )));
        lines.push(Line::from(Span::styled(
            "[u] Review and resolve",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            activity_message(selected.status),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    // After the pivot the panel speaks for the run, so the result section
    // says which stage's artifact it is offering.
    lines.push(if applyable {
        Line::from(Span::styled(
            format!("RESULT · {}", stage_title(selected.kind).to_uppercase()),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        section("RESULT")
    });
    lines.extend(result_lines(state, selected));
    lines.push(Line::from(""));
    lines.push(section("RESOURCES"));
    lines.push(Line::from(Span::styled(
        resource_summary(details),
        Style::default(),
    )));
    lines.push(Line::from(""));
    lines.extend(hero_actions(applyable, selected.status));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(if applyable {
                    " RUN ".to_owned()
                } else {
                    format!(" {} ", stage_title(selected.kind).to_uppercase())
                })
                .title_style(Style::default().add_modifier(Modifier::BOLD)),
        ),
        area,
    );
}

fn stage_hero(stage: &StageSummary, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let clock = format::elapsed(stage.started_at, stage.finished_at, now)
        .map(format::format_clock)
        .unwrap_or_default();
    vec![Line::from(vec![
        Span::styled(
            format!(
                "{} {}",
                stage_glyph(stage.status),
                hero_status(stage.status)
            ),
            stage_style(stage.status).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(clock, Style::default().add_modifier(Modifier::BOLD)),
    ])]
}

/// Once the run is applyable the panel stops monitoring and starts offering
/// review.
fn completed_hero(details: &RunDetails, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let clock = format::elapsed(details.started_at, details.finished_at, now)
        .map(format::format_clock)
        .unwrap_or_default();
    let completed = details
        .stages
        .iter()
        .filter(|stage| stage.status == StageStatus::Completed)
        .count();
    vec![
        Line::from(vec![
            Span::styled(
                "✓ RUN COMPLETE",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(clock, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(Span::styled(
            format!("{completed} of {} stages completed", details.stages.len()),
            Style::default().fg(Color::DarkGray),
        )),
    ]
}

const fn hero_status(status: StageStatus) -> &'static str {
    match status {
        StageStatus::Running => "RUNNING",
        StageStatus::NeedsUser => "NEEDS YOU",
        StageStatus::Completed => "COMPLETED",
        StageStatus::Failed => "FAILED",
        StageStatus::Paused => "PAUSED",
        StageStatus::Interrupted => "INTERRUPTED",
        StageStatus::Ready => "READY",
        StageStatus::Pending => "PENDING",
        StageStatus::Skipped => "SKIPPED",
    }
}

const fn activity_message(status: StageStatus) -> &'static str {
    match status {
        StageStatus::Running => "Agent is working…",
        StageStatus::Pending | StageStatus::Ready => "Waiting for the previous stage",
        StageStatus::Completed => "Stage finished",
        StageStatus::Failed => "Stage failed — [l] inspect logs, [t] retry",
        StageStatus::Paused | StageStatus::Interrupted => "Stage suspended — [r] resume",
        StageStatus::NeedsUser => "Waiting on you",
        StageStatus::Skipped => "Stage skipped by the workflow",
    }
}

/// Operational runtime line: which agent is doing the work, at what effort.
/// Configured and actual targets are only both shown when they disagree.
fn runtime_summary(stage: &StageSummary) -> String {
    let configured = format!(
        "{} · {}",
        stage.configured_provider,
        stage
            .configured_model
            .as_deref()
            .unwrap_or("native default")
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

fn result_lines(state: &TuiState, selected: &StageSummary) -> Vec<Line<'static>> {
    if state.stages_with_artifacts.contains(&selected.id) {
        return vec![
            Line::from(Span::styled(
                "✓ Verified artifact available",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "[Enter/o] Open result",
                Style::default().fg(Color::Green),
            )),
        ];
    }
    // Expected absence stays informational; only a completed stage without an
    // artifact is a real problem, and opening it reports that as an error.
    let text = match selected.status {
        StageStatus::Running => "Not available yet — stage is still running",
        StageStatus::Pending | StageStatus::Ready => "Not available yet — stage has not started",
        StageStatus::Failed => "No completed result",
        _ => "No verified artifact",
    };
    vec![Line::from(Span::styled(
        text,
        Style::default().fg(Color::DarkGray),
    ))]
}

/// Provider-native units, compactly. Never normalized across providers and
/// never presented as cost.
fn resource_summary(details: &RunDetails) -> String {
    use std::fmt::Write as _;
    let mut summary = format!(
        "{} in · {} out",
        format::format_units(details.usage.input_units),
        format::format_units(details.usage.output_units)
    );
    for (label, value) in [
        ("cache read", details.usage.cache_read_units),
        ("cache write", details.usage.cache_write_units),
        ("reasoning out", details.usage.reasoning_output_units),
    ] {
        if let Some(value) = value {
            let _ = write!(summary, " · {} {label}", format::format_units(value));
        }
    }
    summary
}

/// Actions offered by the panel, gated on canonical state: apply and discard
/// appear only for a run the workspace layer would accept.
fn hero_actions(applyable: bool, status: StageStatus) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if applyable {
        lines.push(Line::from(Span::styled(
            "READY TO REVIEW",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(vec![
            Span::styled("[d] Review diff   ", Style::default()),
            Span::styled("[a] Apply changes   ", Style::default().fg(Color::Green)),
            Span::styled("[X] Discard", Style::default().fg(Color::Red)),
        ]));
    } else {
        let mut actions = "[o] result   [l] logs   [d] diff".to_owned();
        if status == StageStatus::Failed {
            actions.push_str("   [t] retry");
        }
        lines.push(Line::from(Span::styled(
            actions,
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(Span::styled(
        "[i] technical details",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

fn section(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        title,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Human stage name for operational rows; technical mode keeps the raw
/// serialized kind.
const fn stage_title(kind: StageKind) -> &'static str {
    match kind {
        StageKind::Research => "Research",
        StageKind::Architecture => "Architecture",
        StageKind::Implementation => "Implementation",
        StageKind::CodeQualityReview => "Quality review",
        StageKind::SpecReview => "Spec review",
        StageKind::Review => "Review",
        StageKind::IndependentReview => "Independent review",
        StageKind::DeepAnalysis => "Deep analysis",
        StageKind::Synthesis => "Synthesis",
        StageKind::Decision => "Decision",
        StageKind::Fix => "Fix",
    }
}

/// Right panel, technical view: every diagnostic the operational view hides,
/// reorganized rather than removed.
#[allow(
    clippy::too_many_lines,
    reason = "one diagnostic panel keeps identity, runtime, usage, and provenance aligned"
)]
fn render_technical(frame: &mut Frame<'_>, area: Rect, state: &TuiState, details: &RunDetails) {
    let Some(selected) = details.stages.get(state.selected_stage_index) else {
        frame.render_widget(
            Paragraph::new("No stage selected")
                .block(Block::default().borders(Borders::ALL).title(" TECHNICAL ")),
            area,
        );
        return;
    };
    let mut lines = vec![
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
    ];
    // Per-stage execution evidence: provider latency stays here and is never
    // presented as the stage's wall-clock elapsed time.
    if let Some(evidence) = state.evidence.as_ref() {
        lines.push(Line::from(format!(
            "Invocations {}",
            evidence.invocation_count
        )));
        lines.push(Line::from(format!(
            "Latency     {}",
            evidence.latency_ms.map_or_else(
                || "unavailable".to_owned(),
                |ms| format!("{ms} ms provider")
            )
        )));
        lines.push(Line::from(format!(
            "Prompt      {}",
            evidence.injected_prompt_bytes.map_or_else(
                || "unavailable".to_owned(),
                |bytes| format!("{bytes} injected bytes")
            )
        )));
        if let Some(version) = evidence.provider_cli_version.as_deref() {
            lines.push(Line::from(format!("CLI         {version}")));
        }
    }
    lines.extend([
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
            "Base        {}",
            details.base_commit.as_deref().unwrap_or("unavailable")
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
    ]);
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
        Line::from(Span::styled(
            "[i] operational view",
            Style::default().fg(Color::DarkGray),
        )),
    ]);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" TECHNICAL ")
                .title_style(Style::default().fg(Color::DarkGray)),
        ),
        area,
    );
}

fn render_artifact(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let (title, lines) = state.artifact.as_ref().map_or_else(
        || {
            (
                " Artifact ".to_owned(),
                vec![Line::from("Artifact unavailable")],
            )
        },
        |artifact| {
            let mode = if state.artifact_raw {
                "raw · [m] rendered"
            } else {
                "rendered · [m] raw"
            };
            let lines = if state.artifact_raw {
                artifact
                    .text
                    .lines()
                    .map(|line| Line::from(line.to_owned()))
                    .collect()
            } else {
                markdown::render_markdown(&artifact.text)
            };
            (
                format!(
                    " {} · attempt {} · {mode} ",
                    artifact.summary.stage_id, artifact.summary.attempt
                ),
                lines,
            )
        },
    );
    frame.render_widget(
        Paragraph::new(lines)
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

/// Key hints per screen; the compact variant is chosen deterministically
/// when the full hints do not fit, so navigation is never hidden. Run Detail
/// hints are additionally gated on canonical state so the footer never
/// advertises an action the domain would refuse.
fn footer_hints(screen: Screen, width: u16, state: &TuiState) -> &'static str {
    let (full, compact) = match screen {
        Screen::Runs => (
            "↑↓/jk run · Enter open · n new · ? help · q quit/detach",
            "↑↓ · Enter open · n new · ? help · q quit",
        ),
        Screen::RunDetail => detail_hints(state),
        Screen::Artifact => (
            "↑↓/PgUp/PgDn scroll · m raw/rendered · Esc run detail · q quit/detach",
            "↑↓ scroll · m raw · Esc run detail",
        ),
        Screen::Logs | Screen::Diff => (
            "↑↓/PgUp/PgDn scroll · Esc run detail · q quit/detach",
            "↑↓ scroll · Esc run detail",
        ),
        Screen::NewRun => (
            "Tab/Shift-Tab fields · ←→ choices/edit · Enter start · Esc cancel",
            "Tab fields · Enter start · Esc cancel",
        ),
    };
    if full.chars().count() <= width as usize {
        full
    } else {
        compact
    }
}

/// Run Detail hints in three shapes: attention first when the run needs the
/// user, review actions once the run is applyable, monitoring otherwise.
/// Apply and discard never appear outside the applyable state.
fn detail_hints(state: &TuiState) -> (&'static str, &'static str) {
    let needs_user = state
        .details
        .as_ref()
        .is_some_and(|details| !details.attention.is_empty());
    if needs_user {
        return (
            "u ATTENTION · ↑↓ stage · Enter/o result · l logs · d diff · Esc runs · i details · ? help",
            "u ATTENTION · ↑↓ stage · Esc runs · ? help",
        );
    }
    if state.run_is_applyable() {
        return (
            "d diff · a apply · X discard · ↑↓ stage · Enter/o result · l logs · Esc runs · i details · ? help",
            "d diff · a apply · X discard · Esc runs",
        );
    }
    (
        "↑↓ stage · Enter/o result · l logs · d diff · r resume · t retry · Esc runs · i details · ? help",
        "↑↓ stage · o result · Esc runs · i details",
    )
}

fn message_presentation(kind: UiMessageKind) -> (&'static str, Style) {
    match kind {
        UiMessageKind::Info => ("ℹ", Style::default().fg(Color::Cyan)),
        UiMessageKind::Success => ("✓", Style::default().fg(Color::Green)),
        UiMessageKind::Warning => ("⚠", Style::default().fg(Color::Yellow)),
        UiMessageKind::Error => (
            "✗",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let mut lines = Vec::new();
    if let Some(message) = state.message.as_ref() {
        let (glyph, style) = message_presentation(message.kind);
        lines.push(Line::from(vec![
            Span::styled(format!("{glyph} "), style),
            Span::styled(message.text.clone(), style),
            Span::styled("  · x dismiss", Style::default().fg(Color::DarkGray)),
        ]));
    }
    lines.push(Line::from(Span::styled(
        footer_hints(state.screen, area.width, state),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::TOP)),
        area,
    );
}

fn render_overlay(frame: &mut Frame<'_>, area: Rect, state: &TuiState, overlay: Overlay) {
    let popup = centered_rect(78, 70, area);
    frame.render_widget(Clear, popup);
    match overlay {
        Overlay::Help => frame.render_widget(
            Paragraph::new(
                "Global\n  ↑/↓ or j/k  navigate\n  Enter        open/confirm\n  Esc          back/close\n  n            new run\n  R            runs screen\n  x            dismiss notification\n  ?            help\n  q / Ctrl-C   quit/detach\n\nRun\n  Enter/o open selected stage result\n  r resume/recover\n  t retry selected failed stage\n  u resolve selected attention\n  l raw logs (read-only)\n  d workspace diff (read-only)\n  a apply (confirmation)\n  X discard (confirmation)\n\nArtifact viewer\n  m toggle raw/rendered Markdown",
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
    let selected_kind = details
        .attention
        .get(state.attention_index)
        .map(|attention| attention.kind);
    if selected_kind == Some(AttentionKind::Permission) {
        lines.push(Line::from(Span::styled(
            "Permission request — no text response required",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(
            "↑/↓ select request · Enter approve/resolve · Esc cancel",
        ));
    } else {
        lines.push(Line::from(format!(
            "Response: {}",
            field_display(&state.attention_response, true)
        )));
        lines.push(Line::from(
            "↑/↓ select request · type response · Enter submit · Esc cancel",
        ));
    }
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
    use crate::domain::{EffortSetting, Role, RunId, StageId, StageKind, WorkflowKind};

    const POD_SHELL: &str = "▄██████████▄";

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
            configured_model: None,
            actual_provider: Some("codex".to_owned()),
            actual_model: None,
            provider_session_record: Some("session-record".to_owned()),
            native_session: Some("native-session-id".to_owned()),
            provider_session_status: Some("completed".to_owned()),
            process_status: Some("exited".to_owned()),
            started_at: None,
            finished_at: None,
        }
    }

    fn details(status: RunStatus, stages: Vec<StageSummary>) -> RunDetails {
        RunDetails {
            id: RunId::from_u128(3),
            task: Some("Add OAuth provider support".to_owned()),
            workflow: WorkflowKind::Standard,
            status,
            repository: Some(std::path::PathBuf::from("/Users/e/Code/wp-calypso-2")),
            workspace_status: Some(crate::workspace::WorkspaceStatus::Ready),
            base_commit: Some("abc1234".to_owned()),
            profile: "recommended".to_owned(),
            profile_version: "recommended_v2".to_owned(),
            routes: vec![RouteSummary {
                role: Role::Implementer,
                configured_provider: "codex".to_owned(),
                configured_model: None,
                reason: "recommended_role_assignment".to_owned(),
            }],
            revision: crate::store::RunRevision::initial(),
            created_at: at(12, 0, 0),
            updated_at: at(12, 5, 0),
            stages,
            attention: Vec::new(),
            usage: UsageSummary {
                input_units: 12_288,
                output_units: 33_154,
                cache_read_units: Some(2_310_442),
                ..UsageSummary::default()
            },
            started_at: None,
            finished_at: None,
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
            updated_at: at(12, 0, 0),
        }]);
        state.details = Some(details(RunStatus::NeedsUser, Vec::new()));
        let text = render_text(&state, 120, 30);
        assert!(text.contains("OAuth provider"));
        assert!(text.contains("NEEDS YOU"));
        state.overlay = Some(Overlay::Help);
        assert!(render_text(&state, 120, 30).contains("Help · Esc closes"));
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
    fn running_hero_leads_with_state_clock_runtime_and_activity() {
        let text = render_text(&running_state(), 160, 40);
        assert!(text.contains("IMPLEMENTATION"), "stage names the panel");
        assert!(text.contains("RUNNING"));
        assert!(text.contains("Agent is working…"));
        assert!(text.contains("codex"), "runtime summary is present");
        assert!(text.contains("native default"));
        assert!(
            !text.contains("Kind        "),
            "operational view drops technical field rows"
        );
    }

    #[test]
    fn operational_view_hides_diagnostics_and_technical_view_shows_them() {
        let mut state = running_state();
        let operational = render_text(&state, 160, 40);
        assert!(!operational.contains("native-session-id"), "no native ids");
        assert!(!operational.contains("session-record"));
        assert!(
            !operational.contains("/Users/e/Code/wp-calypso-2"),
            "operational view keeps the full path out"
        );
        assert!(operational.contains("[i] technical details"));

        state.technical = true;
        let technical = render_text(&state, 160, 40);
        assert!(technical.contains("TECHNICAL"), "mode is labelled");
        assert!(technical.contains("native-session-id".get(..8).unwrap()));
        assert!(technical.contains("/Users/e/Code/wp-calypso-2"));
        assert!(technical.contains("recommended_v2"));
        assert!(technical.contains("recommended_role_assignment"));
        assert!(technical.contains("abc1234"), "base commit stays available");
        assert!(technical.contains("[i] operational view"));
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

    #[test]
    fn resources_stay_provider_native_and_compact() {
        let summary = resource_summary(&details(RunStatus::Running, Vec::new()));
        assert!(summary.contains("12.2k in"));
        assert!(summary.contains("33.1k out"));
        assert!(summary.contains("2.3M cache read"));
        assert!(!summary.contains('$'), "usage never implies cost");
    }

    #[test]
    fn needs_user_dominates_the_hero_and_precedes_resources() {
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
        let resources = text.find("RESOURCES").unwrap();
        assert!(action < resources, "attention outranks resource evidence");
        assert!(
            render_text(&state, 160, 40).contains("u ATTENTION"),
            "footer prioritizes the attention shortcut"
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
    }

    #[test]
    fn footer_advertises_apply_only_when_the_run_is_applyable() {
        let running = running_state();
        let hints = footer_hints(Screen::RunDetail, 200, &running);
        assert!(!hints.contains("a apply"), "no apply hint while running");
        assert!(hints.contains("Esc runs"));

        let completed = completed_state();
        let hints = footer_hints(Screen::RunDetail, 200, &completed);
        assert!(hints.contains("a apply"));
        assert!(hints.contains("X discard"));

        // Narrow terminals abbreviate but never invent unavailable actions.
        let compact = footer_hints(Screen::RunDetail, 55, &running);
        assert!(compact.chars().count() <= 55);
        assert!(!compact.contains("a apply"));
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
        assert!(text.contains("✓ Verified artifact available"));
        assert!(text.contains("[Enter/o] Open result"));

        let mut failed = running_state();
        failed.details.as_mut().unwrap().stages[1].status = StageStatus::Failed;
        let text = render_text(&failed, 160, 40);
        assert!(text.contains("No completed result"));
        assert!(text.contains("[t] retry"), "failed stage offers recovery");
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
        assert!(text.contains("</>"), "coding accent while running");
        assert!(!render_text(&running, 70, 24).contains(POD_SHELL));
    }

    #[test]
    fn operational_information_survives_every_supported_size() {
        for (width, height) in [(160, 40), (100, 30), (70, 24)] {
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
                text.contains("Esc runs"),
                "actions survive at {width}x{height}"
            );
        }
        // POD is the first thing to go, and only after operational content fits.
        assert!(render_text(&running_state(), 160, 40).contains(POD_SHELL));
        assert!(!render_text(&running_state(), 70, 24).contains(POD_SHELL));
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
        assert!(text.contains("Permission request — no text response required"));
        assert!(text.contains("Enter approve/resolve"));
        assert!(!text.contains("Response:"), "no implied response field");

        state.details.as_mut().unwrap().attention[0].kind = AttentionKind::Question;
        let text = render_text(&state, 120, 30);
        assert!(
            text.contains("Response:"),
            "question keeps editable response"
        );
        assert!(text.contains("Enter submit"));
    }

    #[test]
    fn confirmation_overlays_keep_existing_semantics() {
        let mut state = completed_state();
        state.overlay = Some(Overlay::ApplyConfirm);
        assert!(render_text(&state, 120, 30).contains("Enter confirms apply"));
        state.overlay = Some(Overlay::DiscardConfirm);
        assert!(render_text(&state, 120, 30).contains("Enter confirms discard"));
    }
}
