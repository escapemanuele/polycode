use anyhow::Result;
use clap::CommandFactory;

use crate::app::{
    ApplyOutcome, ExecutionReport, ExecutionSelection, QuiescentState, RunDetails, RunService,
    RuntimeProviderFactory, UniformProvider,
};
use crate::domain::{DomainEventKind, StageStatus, WorkflowKind};
use crate::process::ProcessBackend;

use super::{Cli, Command, EvalCommand, EvalRunArgs, RunArgs, UpdateArgs};

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
        Some(Command::VerifyReleaseTag { tag }) => verify_release_tag(tag),
        Some(Command::RegisterOfficialInstall { executable, asset }) => {
            register_official_install(executable, asset.as_deref())
        }
        Some(Command::InstallSourceOf { executable }) => install_source_of(executable.as_deref()),
        Some(Command::Tui) => anyhow::bail!("TUI dispatch must be handled before CLI commands"),
        Some(Command::Eval { command }) => eval(command),
        Some(Command::Update(args)) => update(*args),
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
        Some(Command::Stop { run_id }) => {
            let report = service()?.stop_run(*run_id)?;
            println!("Run stopped. Workspace and results are preserved.");
            println!("Resume it with `polycode resume {run_id}`.");
            print_report(&report);
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
        Some(Command::Fix { run_id }) => {
            print_report(&service()?.request_fix(*run_id)?);
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
    let effort = parse_effort(args.effort.as_deref())?;
    let report =
        service()?.start_run(workflow, args.task.clone(), &args.repo, selection, effort)?;
    print_report(&report);
    Ok(())
}

/// CLI effort words. `native` (or omission) preserves the runtime's own
/// configured default; anything unknown fails closed.
fn parse_effort(value: Option<&str>) -> Result<crate::domain::EffortSetting> {
    use crate::domain::EffortSetting;
    match value {
        None | Some("native") => Ok(EffortSetting::NativeDefault),
        Some("low") => Ok(EffortSetting::LOW),
        Some("medium") => Ok(EffortSetting::MEDIUM),
        Some("high") => Ok(EffortSetting::HIGH),
        Some(other) => {
            anyhow::bail!("unsupported effort {other:?}; supported: native, low, medium, high")
        }
    }
}

/// Bootstrap hook: records an installed executable as officially managed so
/// self-update becomes available for it. Validation lives in the update
/// module, so the installer never reproduces the receipt schema, the data
/// directory rules, or the path semantics in shell.
fn register_official_install(executable: &std::path::Path, asset: Option<&str>) -> Result<()> {
    let asset = asset.map_or_else(
        || crate::update::target_asset_name().unwrap_or("unknown"),
        |asset| asset,
    );
    let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    // A failed receipt write is returned as an error, so install.sh sees a
    // non-zero exit and tells the user automatic updates were not registered
    // rather than implying they were.
    let receipt = crate::update::register_official_install(executable, asset, now)?;
    println!(
        "registered {} as an official Polycode {} installation",
        receipt.executable.display(),
        receipt.version
    );
    Ok(())
}

/// Bootstrap hook: reports how an executable would be classified, without
/// executing it. The installer uses this to decide whether a file already at
/// the destination is a Polycode installation it may replace.
fn install_source_of(executable: Option<&std::path::Path>) -> Result<()> {
    let executable = match executable {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe()?,
    };
    println!("{}", crate::update::classify_path(&executable)?.label());
    Ok(())
}

/// Release-pipeline gate. Read-only and offline: it compares a candidate tag
/// against the version this checkout compiles as, and fails the process when
/// they disagree, so a mismatched tag cannot reach publication.
fn verify_release_tag(tag: &str) -> Result<()> {
    let version = crate::update::verify_release_tag(tag)?;
    println!("release tag {tag} matches package version {version}");
    Ok(())
}

/// Reports update status and, when explicitly confirmed, installs an
/// official release over this executable.
///
/// `--check` never mutates anything. Without it the command still refuses to
/// install unless the installation is one Polycode recognizes as its own and
/// the user confirms — `--yes` is the only way to skip the prompt, and a
/// non-interactive invocation without it reports instead of guessing.
fn update(args: UpdateArgs) -> Result<()> {
    let service = crate::update::UpdateService::from_environment()?;
    let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    // Both forms are explicitly typed by the user, so both get a real check.
    // Answering `--check` from a day-old cache made Polycode report "up to
    // date" minutes after a release was published.
    let status = service.check_now(now);
    println!("Current version: {}", crate::update::CURRENT_VERSION);
    let crate::update::UpdateStatus::Available(info) = &status else {
        match status {
            crate::update::UpdateStatus::Current => println!("Polycode is up to date."),
            _ if crate::update::checks_disabled() => println!(
                "Update checks are disabled ({}).",
                crate::update::DISABLE_ENVIRONMENT_VARIABLE
            ),
            _ => println!("Update status is unavailable right now."),
        }
        return Ok(());
    };
    println!(
        "Update available: {} \u{2192} {}",
        info.current_version, info.available_version
    );
    if !info.release_url.is_empty() {
        println!("Release: {}", info.release_url);
    }
    let source = crate::update::detect_install_source()?;
    println!("Install source: {}", source.label());
    let strategy = source.strategy();
    if !strategy.is_automatic() {
        println!("{}", strategy.guidance());
        return Ok(());
    }
    if manual_update(args.check) == ManualUpdate::Report {
        println!("Automatic installation is supported. Run `polycode update` to install.");
        return Ok(());
    }
    if !args.yes && !confirm_install(&info.available_version.to_string())? {
        println!("Not installing.");
        return Ok(());
    }
    let installed = install_update(info, now)?;
    println!("{}", installed.restart_notice());
    if let Some(warning) = installed.registration_warning() {
        println!("{warning}");
    }
    Ok(())
}

/// What an explicitly typed update command may do to the executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManualUpdate {
    /// Report only. `--check` can never reach the installer.
    Report,
    /// Offer installation, still subject to confirmation and every
    /// install-source, checksum, and version rule.
    Offer,
}

const fn manual_update(check: bool) -> ManualUpdate {
    if check {
        ManualUpdate::Report
    } else {
        ManualUpdate::Offer
    }
}

/// Explicit confirmation before an irreversible action, matching how apply
/// and discard behave. A non-interactive stdin is never treated as consent.
fn confirm_install(version: &str) -> Result<bool> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    if !std::io::stdin().is_terminal() {
        println!("Re-run with `--yes` to install {version} without a prompt.");
        return Ok(false);
    }
    print!("Install {version} now? It applies when Polycode restarts. [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Re-reads the release so the download and its checksums come from one
/// consistent listing rather than from cached metadata.
fn install_update(
    info: &crate::update::UpdateInfo,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<crate::update::Installed> {
    use crate::update::ReleaseSource as _;
    let source = crate::update::GitHubReleases::new(
        crate::update::OFFICIAL_REPOSITORY,
        std::time::Duration::from_secs(10),
    );
    let release = source
        .latest_stable()?
        .filter(|release| release.version == info.available_version)
        .ok_or_else(|| anyhow::anyhow!("release {} is no longer published", info.tag))?;
    let executable = std::env::current_exe()?;
    let downloader = crate::update::HttpDownloader::new(std::time::Duration::from_secs(120));
    Ok(crate::update::install(
        &release,
        &executable,
        &downloader,
        now,
    )?)
}

/// Version and distribution diagnostics. Never touches the network: how
/// Polycode was installed is knowable offline, and internet reachability is
/// not a doctor failure.
fn print_distribution() {
    println!("  version: {}", crate::update::CURRENT_VERSION);
    match crate::update::detect_install_source() {
        Ok(source) => {
            println!("  install source: {}", source.label());
            println!(
                "  automatic update: {}",
                if source.strategy().is_automatic() {
                    "supported"
                } else {
                    "unavailable for this build"
                }
            );
        }
        Err(error) => println!("  install source: undetermined ({error})"),
    }
    println!(
        "  update checks: {}",
        if crate::update::checks_disabled() {
            "disabled"
        } else {
            "enabled (public GitHub release metadata, at most once per day)"
        }
    );
}

fn doctor() -> Result<()> {
    let config_file = crate::config::config_file()?;
    let database_file = crate::store::database_file()?;

    println!("Polycode doctor");
    print_distribution();
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
    // Git is as fundamental as tmux here: every run needs a repository and a
    // managed worktree.
    if let Some(version) = crate::git::git_version(&crate::git::Git::default()) {
        println!("  Git: available ({version})");
    } else {
        println!("  Git: not found on PATH");
        println!("  guidance: install Git, verify `git --version` works, then rerun");
    }
    let tmux = crate::process::TmuxBackend::new(std::env::current_exe()?);
    match tmux.availability() {
        Ok(availability) => println!("  tmux: available ({})", availability.version),
        Err(crate::process::ProcessError::TmuxNotFound) => println!("  tmux: unavailable"),
        Err(error) => println!("  tmux: error ({error})"),
    }
    Ok(())
}

fn eval(command: &EvalCommand) -> Result<()> {
    match command {
        EvalCommand::List => {
            for version in [
                crate::eval::ROLE_CORE_SUITE_VERSION,
                crate::eval::ROLE_CORE_SUITE_VERSION_V2,
                crate::eval::ROLE_CORE_SUITE_VERSION_V3,
            ] {
                let suite = crate::eval::EvalSuite::load(version)?;
                println!("{} · {}", suite.version(), suite.fingerprint());
                for case in suite.cases() {
                    println!(
                        "  {}  role={}  workflow={}",
                        case.id,
                        enum_text(case.target_role),
                        enum_text(case.workflow)
                    );
                }
            }
            println!(
                "Architect, Researcher, and EngineeringLead cases are deferred until deterministic high-signal oracles exist."
            );
            Ok(())
        }
        EvalCommand::Run(args) => run_eval(args),
        EvalCommand::Report { paths } => {
            let results = crate::eval::load_results(paths)?;
            print!("{}", crate::eval::render_report(&results)?);
            Ok(())
        }
    }
}

fn run_eval(args: &EvalRunArgs) -> Result<()> {
    let provider = crate::eval::EvalProvider::try_from(args.provider.as_str())?;
    let target = crate::eval::EvalTarget::new(provider, args.model.clone())?;
    let suite = crate::eval::EvalSuite::load(&args.suite)?;
    let runner = crate::eval::EvalRunner::new(crate::eval::EvalRunOptions {
        target,
        repeat: args.repeat,
        allow_native_usage: args.allow_native_usage,
        output: args.out.clone(),
    })?;
    let summary = runner.run(&suite, |case, ordinal, total| {
        println!(
            "{} · {} / {} · {ordinal}/{total} · {}",
            suite.version(),
            args.provider,
            args.model.as_deref().unwrap_or("native_default"),
            case.id
        );
    })?;
    for result in &summary.results {
        let mark = match result.status {
            crate::eval::EvalStatus::Passed => "✓",
            crate::eval::EvalStatus::Failed => "✗",
            crate::eval::EvalStatus::InfrastructureFailure => "!",
        };
        println!(
            "{mark} {} · repetition {} · {:?}",
            result.case_id, result.repetition, result.status
        );
    }
    println!("Evidence: {}", summary.output_directory.display());
    print!("{}", crate::eval::render_report(&summary.results)?);
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

#[allow(
    clippy::too_many_lines,
    reason = "one status projection keeps configured and actual columns aligned in source order"
)]
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
            "{} {} ({}) · role={} · configured={}/{} · effort={} · actual={}/{} · session={} · native={} · conversation={} · process={}",
            stage_mark(stage.status),
            stage.id,
            enum_text(stage.status),
            enum_text(stage.role),
            stage.configured_provider,
            stage
                .configured_model
                .as_deref()
                .unwrap_or("native default"),
            stage.observed_effort.as_ref().map_or_else(
                || format!("{} requested", stage.requested_effort.label()),
                |observed| {
                    format!(
                        "{} requested → {observed} observed",
                        stage.requested_effort.label()
                    )
                }
            ),
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
    for line in usage_lines(&details.usage) {
        println!("{line}");
    }
}

/// Provider-native units, one line per runtime that reported any.
///
/// Totals are never summed across runtimes: their input figures do not
/// measure the same thing. A runtime whose input total already contains its
/// cache reads says so in place instead of listing the cache read again as a
/// further quantity.
fn usage_lines(usage: &crate::app::RunUsage) -> Vec<String> {
    use std::fmt::Write as _;
    if usage.is_empty() {
        return vec!["Usage      not reported".to_owned()];
    }
    usage
        .providers()
        .map(|entry| {
            let mut line = format!(
                "Usage      {:<8} {} input units",
                entry.provider, entry.usage.input_units
            );
            let folded_cache_read = entry
                .input_contains_cache_reads()
                .then_some(entry.usage.cache_read_units)
                .flatten();
            if let Some(cached) = folded_cache_read {
                let _ = write!(line, " ({cached} of them cached)");
            }
            let _ = write!(line, " · {} output units", entry.usage.output_units);
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
                    let _ = write!(line, " · {value} {label} units");
                }
            }
            line
        })
        .collect()
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
        DomainEventKind::RunFixRequested { .. } => "fix requested",
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
        DomainEventKind::ProviderRuntimeObserved { .. } => "provider runtime observed",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The `--check` form is report-only by construction: there is one path to
    /// the installer, and this decision closes it.
    #[test]
    fn an_explicit_check_can_never_reach_the_installer() {
        assert_eq!(manual_update(true), ManualUpdate::Report);
        assert_eq!(manual_update(false), ManualUpdate::Offer);
    }

    /// Both typed forms must perform a real check. The cache-aware entry point
    /// exists for background detection only, so its name must not appear here
    /// — answering a typed `--check` from a day-old cache is the bug this
    /// module was fixed for.
    #[test]
    fn typed_update_commands_never_use_the_cache_aware_entry_point() {
        // Only the non-test half of this file is the call graph under test.
        let source = include_str!("commands.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        assert!(
            !code.contains("cached_status"),
            "CLI update commands must call check_now, not cached_status"
        );
        assert!(code.contains("service.check_now(now)"));
    }
}
