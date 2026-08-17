use anyhow::Result;
use clap::CommandFactory;

use crate::app::{
    ApplyOutcome, ExecutionReport, ExecutionSelection, QuiescentState, RunDetails, RunService,
    RuntimeProviderFactory, UniformProvider,
};
use crate::domain::{DomainEventKind, StageStatus, WorkflowKind};
use crate::process::ProcessBackend;

use super::{Cli, Command, RunArgs};

pub fn execute(command: Option<&Command>) -> Result<()> {
    match command {
        Some(Command::RunProcess { manifest }) => {
            crate::process::run_managed_process(manifest)?;
            Ok(())
        }
        Some(Command::ExecProcess { manifest }) => {
            crate::process::exec_managed_process(manifest)?;
            Ok(())
        }
        Some(Command::Tui) => anyhow::bail!("TUI dispatch must be handled before CLI commands"),
        Some(Command::Doctor) => doctor(),
        Some(Command::Runs) => runs(),
        Some(Command::Fast(args)) => start(WorkflowKind::Fast, args),
        Some(Command::Standard(args)) => start(WorkflowKind::Standard, args),
        Some(Command::Deep(args)) => start(WorkflowKind::Deep, args),
        Some(Command::Review(args)) => start(WorkflowKind::Review, args),
        Some(Command::Status { run_id }) => {
            print_details(&service()?.inspect_run(*run_id)?);
            Ok(())
        }
        Some(Command::Resume { run_id }) => {
            print_report(&service()?.resume_run(*run_id)?);
            Ok(())
        }
        Some(Command::Retry { run_id, stage_id }) => {
            print_report(&service()?.retry_stage(*run_id, stage_id)?);
            Ok(())
        }
        Some(Command::Resolve {
            run_id,
            attention_id,
            response,
        }) => {
            print_report(&service()?.resolve_attention_with_response(
                *run_id,
                *attention_id,
                response.as_deref(),
            )?);
            Ok(())
        }
        Some(Command::Apply { run_id }) => {
            let (outcome, report) = service()?.apply_run(*run_id)?;
            match outcome {
                ApplyOutcome::Applied => println!("Changes applied to source repository."),
                ApplyOutcome::NoChanges => println!("No workspace changes to apply."),
            }
            print_report(&report);
            Ok(())
        }
        Some(Command::Discard { run_id }) => {
            print_report(&service()?.discard_run(*run_id)?);
            Ok(())
        }
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

fn service() -> Result<RunService<RuntimeProviderFactory>> {
    Ok(RunService::from_environment(RuntimeProviderFactory)?)
}

fn start(workflow: WorkflowKind, args: &RunArgs) -> Result<()> {
    let selection = match (args.provider.as_deref(), args.profile.as_deref()) {
        (Some(provider), None) => Some(ExecutionSelection::Uniform(UniformProvider::try_from(
            provider,
        )?)),
        (None, Some("recommended")) => Some(ExecutionSelection::Recommended),
        (None, Some(other)) => {
            anyhow::bail!("unsupported profile {other:?}; supported profiles: recommended")
        }
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap rejects conflicting selection flags"),
    };
    let report = service()?.start_run(workflow, args.task.clone(), &args.repo, selection)?;
    print_report(&report);
    Ok(())
}

fn doctor() -> Result<()> {
    let config_file = crate::config::config_file()?;
    let database_file = crate::store::database_file()?;

    println!("Polycode doctor");
    println!("  status: Milestone 10 Ratatui local control room");
    println!("  config: {}", config_file.display());
    println!("  database: {}", database_file.display());
    if database_file.exists() {
        let store = crate::store::SqliteStore::open(&database_file)?;
        println!("  database schema: {}", store.schema_version()?);
    } else {
        println!("  database schema: not initialized");
    }
    match crate::providers::claude::ClaudeInstallation::discover() {
        Ok(installation) => {
            println!("  Claude Code: available ({})", installation.version());
            println!(
                "  Claude auth: {}{}",
                if installation.authenticated() {
                    "ready"
                } else {
                    "not authenticated"
                },
                installation
                    .auth_method()
                    .map_or(String::new(), |method| format!(" ({method})"))
            );
        }
        Err(crate::providers::claude::ClaudeProviderError::NotFound) => {
            println!("  Claude Code: not found on PATH");
            println!(
                "  guidance: install/configure Claude Code, verify `claude` works, then rerun `polycode doctor`"
            );
        }
        Err(error) => println!("  Claude Code: error ({error})"),
    }
    let suspicious = crate::providers::claude::suspicious_secret_environment();
    if suspicious.is_empty() {
        println!("  secret environment: no known provider credential overrides detected");
    } else {
        println!(
            "  secret environment: set variables: {}",
            suspicious.join(", ")
        );
    }
    match crate::providers::codex::CodexInstallation::discover() {
        Ok(installation) => {
            println!("  Codex CLI: available ({})", installation.version());
            println!(
                "  Codex auth: {}{}",
                if installation.authenticated() {
                    "ready"
                } else {
                    "not authenticated"
                },
                installation
                    .auth_method()
                    .map_or(String::new(), |method| format!(" ({method})"))
            );
        }
        Err(crate::providers::codex::CodexProviderError::NotFound) => {
            println!("  Codex CLI: not found on PATH");
            println!(
                "  guidance: install/configure Codex CLI, authenticate with native `codex login`, then rerun `polycode doctor`"
            );
        }
        Err(error) => println!("  Codex CLI: error ({error})"),
    }
    let codex_environment = crate::providers::codex::suspicious_codex_environment();
    if !codex_environment.is_empty() {
        println!(
            "  Codex environment overrides: {}",
            codex_environment.join(", ")
        );
    }
    println!("  fake provider: available (deterministic development/testing)");
    let tmux = crate::process::TmuxBackend::new(std::env::current_exe()?);
    match tmux.availability() {
        Ok(availability) => println!("  tmux: available ({})", availability.version),
        Err(crate::process::ProcessError::TmuxNotFound) => println!("  tmux: unavailable"),
        Err(error) => println!("  tmux: error ({error})"),
    }
    Ok(())
}

fn runs() -> Result<()> {
    let items = service()?.list_runs()?;
    if items.is_empty() {
        println!("No runs.");
        return Ok(());
    }
    for run in items {
        println!(
            "{}  {}  {}  {}  {}  {}",
            run.id,
            enum_text(run.workflow),
            enum_text(run.status),
            run.task_summary,
            run.repository
                .as_deref()
                .map_or("-".to_owned(), |path| path.display().to_string()),
            run.updated_at.to_rfc3339()
        );
    }
    Ok(())
}

fn print_report(report: &ExecutionReport) {
    for event in &report.committed_events {
        print_event(
            event.sequence,
            event.stage_id.as_ref().map(ToString::to_string),
            &event.kind,
        );
    }
    println!();
    print_details(&report.details);
    match &report.outcome {
        QuiescentState::NeedsUser => {
            println!("Resolve each attention request with `polycode resolve`.");
        }
        QuiescentState::Failed => println!("Retry a failed stage with `polycode retry`."),
        QuiescentState::Paused | QuiescentState::Interrupted => {
            println!(
                "Continue recovery with `polycode resume {}`.",
                report.details.id
            );
        }
        QuiescentState::WaitingForProvider { stage_id } => {
            println!("Stage {stage_id} is waiting for provider progress; resume later.");
        }
        _ => {}
    }
}

fn print_event(sequence: u64, stage: Option<String>, kind: &DomainEventKind) {
    let scope = stage.map_or_else(|| "run".to_owned(), |stage| format!("stage {stage}"));
    match kind {
        DomainEventKind::ProviderProgress { message, .. } => {
            println!("[{sequence}] {scope}: {message}");
        }
        DomainEventKind::ProviderUsageUpdated {
            input_units,
            output_units,
            ..
        } => println!(
            "[{sequence}] {scope}: usage +{input_units} input units, +{output_units} output units"
        ),
        DomainEventKind::ProviderFailed { reason, .. } => println!(
            "[{sequence}] {scope}: provider failed{}",
            reason
                .as_deref()
                .map_or(String::new(), |reason| format!(": {reason}"))
        ),
        _ => println!("[{sequence}] {scope}: {}", event_name(kind)),
    }
}

fn print_details(details: &RunDetails) {
    println!("Run        {}", details.id);
    println!("Workflow   {}", enum_text(details.workflow));
    println!("Status     {}", enum_text(details.status));
    println!(
        "Profile    {} ({})",
        details.profile, details.profile_version
    );
    println!(
        "Repository {}",
        details
            .repository
            .as_deref()
            .map_or("unavailable".to_owned(), |path| path.display().to_string())
    );
    println!(
        "Workspace  {}",
        details
            .workspace_status
            .map_or("unavailable".to_owned(), |status| format!("{status:?}")
                .to_lowercase())
    );
    println!(
        "Base       {}",
        details.base_commit.as_deref().unwrap_or("unavailable")
    );
    println!("Revision   {}", details.revision.value());
    println!("Created    {}", details.created_at.to_rfc3339());
    println!("Updated    {}", details.updated_at.to_rfc3339());
    println!();
    println!("Task");
    println!(
        "{}",
        details
            .task
            .as_deref()
            .unwrap_or("<legacy input unavailable>")
    );
    println!();
    println!("Routing");
    for route in &details.routes {
        println!(
            "{}  {}  {}  {}",
            enum_text(route.role),
            route.configured_provider,
            route
                .configured_model
                .as_deref()
                .unwrap_or("native default"),
            route.reason
        );
    }
    println!();
    println!("Stages");
    for stage in &details.stages {
        println!(
            "{} {} ({}) · role={} · configured={}/{} · actual={}/{} · session={} · native={} · conversation={} · process={}",
            stage_mark(stage.status),
            stage.id,
            enum_text(stage.status),
            enum_text(stage.role),
            stage.configured_provider,
            stage
                .configured_model
                .as_deref()
                .unwrap_or("native default"),
            stage.actual_provider.as_deref().unwrap_or("not started"),
            stage.actual_model.as_deref().unwrap_or("unconfirmed"),
            stage
                .provider_session_record
                .as_deref()
                .unwrap_or("unavailable"),
            stage.native_session.as_deref().unwrap_or("unavailable"),
            stage
                .provider_session_status
                .as_deref()
                .unwrap_or("unavailable"),
            stage.process_status.as_deref().unwrap_or("unavailable")
        );
    }
    println!();
    println!("Attention");
    if details.attention.is_empty() {
        println!("none");
    } else {
        for request in &details.attention {
            println!(
                "{} · {} · {} · {}",
                request.id,
                request.stage_id,
                enum_text(request.kind),
                request.summary
            );
        }
    }
    println!();
    println!(
        "Usage      {} input units · {} output units",
        details.usage.input_units, details.usage.output_units
    );
}

fn stage_mark(status: StageStatus) -> &'static str {
    match status {
        StageStatus::Completed => "✓",
        StageStatus::Failed
        | StageStatus::NeedsUser
        | StageStatus::Paused
        | StageStatus::Interrupted => "!",
        StageStatus::Running | StageStatus::Ready => ">",
        StageStatus::Pending | StageStatus::Skipped => "·",
    }
}

fn enum_text(value: impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn event_name(kind: &DomainEventKind) -> &'static str {
    match kind {
        DomainEventKind::RunCreated { .. } => "run created",
        DomainEventKind::RunPreparationStarted => "workspace preparation started",
        DomainEventKind::RunPrepared => "workspace ready",
        DomainEventKind::RunStarted => "run started",
        DomainEventKind::RunPaused => "run paused",
        DomainEventKind::RunInterrupted => "run interrupted",
        DomainEventKind::RunResumed => "run resumed",
        DomainEventKind::RunRecovered => "run recovered",
        DomainEventKind::RunCompleted => "run completed",
        DomainEventKind::RunFailed => "run failed",
        DomainEventKind::RunApplied => "run applied",
        DomainEventKind::RunDiscarded => "run discarded",
        DomainEventKind::StageReady { .. } => "ready",
        DomainEventKind::StageStarted => "started",
        DomainEventKind::StagePaused => "paused",
        DomainEventKind::StageInterrupted => "interrupted",
        DomainEventKind::StageResumed => "resumed",
        DomainEventKind::StageRecovered => "recovered",
        DomainEventKind::StageCompleted => "completed",
        DomainEventKind::StageSkipped => "skipped",
        DomainEventKind::StageFailed => "failed",
        DomainEventKind::StageRetryScheduled => "retry scheduled",
        DomainEventKind::NeedsUser { .. } => "attention requested",
        DomainEventKind::AttentionResolved { .. } => "attention resolved",
        DomainEventKind::AttentionCancelled { .. } => "attention cancelled",
        DomainEventKind::ProviderStarted { .. } => "provider started",
        DomainEventKind::ProviderNeedsUser { .. } => "provider awaiting user",
        DomainEventKind::ProviderPaused { .. } => "provider paused",
        DomainEventKind::ProviderInterrupted { .. } => "provider interrupted",
        DomainEventKind::ProviderCompleted { .. } => "provider completed",
        DomainEventKind::ProviderFailed { .. } => "provider failed",
        DomainEventKind::UsageUpdated => "usage updated",
        DomainEventKind::ProviderProgress { .. } | DomainEventKind::ProviderUsageUpdated { .. } => {
            unreachable!("formatted separately")
        }
        DomainEventKind::ProviderResumed { .. } => "provider resumed",
    }
}
