use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::result::{EvalMetrics, EvalResultError, EvalResultV1, EvalStatus};

/// Loads and validates every result file under supplied files or directories.
///
/// # Errors
/// Rejects missing paths, empty result sets, unsupported schemas, malformed files, or I/O errors.
pub fn load_results(paths: &[PathBuf]) -> Result<Vec<EvalResultV1>, EvalReportError> {
    if paths.is_empty() {
        return Err(EvalReportError::NoPaths);
    }
    let mut files = Vec::new();
    for path in paths {
        collect_result_files(path, &mut files)?;
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Err(EvalReportError::NoResults);
    }
    files
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path)?;
            EvalResultV1::from_json(&bytes)
                .map_err(|source| EvalReportError::InvalidResult { path, source })
        })
        .collect()
}

/// Renders role-oriented target evidence without choosing a winner.
///
/// # Errors
/// Rejects an empty result set.
#[allow(
    clippy::too_many_lines,
    reason = "single formatter keeps role sections and comparison columns visibly aligned"
)]
pub fn render_report(results: &[EvalResultV1]) -> Result<String, EvalReportError> {
    if results.is_empty() {
        return Err(EvalReportError::NoResults);
    }
    let mut groups = BTreeMap::<String, Vec<&EvalResultV1>>::new();
    for result in results {
        groups
            .entry(result.target.label())
            .or_default()
            .push(result);
    }
    let mut output = String::new();
    let mut summaries = Vec::new();
    for (target, group) in &groups {
        let summary = TargetSummary::from_results(group);
        writeln!(output, "TARGET\n{target}").expect("String write cannot fail");
        if group.iter().any(|result| result.synthetic) {
            writeln!(output, "SYNTHETIC — not routing evidence").expect("String write cannot fail");
        }
        writeln!(output, "\nImplementer").expect("String write cannot fail");
        for result in group.iter().filter(|result| {
            matches!(result.metrics, Some(EvalMetrics::Implementer(_)))
                || result.role == crate::domain::Role::Implementer
        }) {
            writeln!(
                output,
                "  {:36} {}",
                result.case_id,
                status_text(result.status)
            )
            .expect("String write cannot fail");
        }
        writeln!(
            output,
            "  pass                          {}/{}",
            summary.implementer_passed, summary.implementer_total
        )
        .expect("String write cannot fail");
        writeln!(
            output,
            "  plan mismatch                 {}/{}",
            summary.plan_mismatch_passed, summary.plan_mismatch_total
        )
        .expect("String write cannot fail");
        writeln!(output, "\nCode Quality Reviewer").expect("String write cannot fail");
        writeln!(
            output,
            "  planted findings              {}/{}",
            summary.quality_found, summary.quality_total
        )
        .expect("String write cannot fail");
        writeln!(
            output,
            "  false positives               {} (must-fix {})",
            summary.quality_false_positives, summary.quality_must_fix_false_positives
        )
        .expect("String write cannot fail");
        writeln!(output, "\nSpecification Reviewer").expect("String write cannot fail");
        writeln!(
            output,
            "  Missing                       {}/{}",
            summary.spec_missing_found, summary.spec_missing_total
        )
        .expect("String write cannot fail");
        writeln!(
            output,
            "  Wrong                         {}/{}",
            summary.spec_wrong_found, summary.spec_wrong_total
        )
        .expect("String write cannot fail");
        writeln!(
            output,
            "  Unrequested                   {}/{}",
            summary.spec_unrequested_found, summary.spec_unrequested_total
        )
        .expect("String write cannot fail");
        writeln!(
            output,
            "  false positives               {}",
            summary.spec_false_positives
        )
        .expect("String write cannot fail");
        writeln!(
            output,
            "\nMedian latency                 {} ms\nMedian usage                   {} input / {} output\nInfrastructure failures        {}\n",
            summary.median_latency,
            summary.median_input,
            summary.median_output,
            summary.infrastructure_failures
        )
        .expect("String write cannot fail");
        for result in group
            .iter()
            .filter(|result| result.status != EvalStatus::Passed)
        {
            writeln!(
                output,
                "  {} rep {}: {}{}",
                result.case_id,
                result.repetition,
                status_text(result.status),
                result
                    .detail
                    .as_deref()
                    .map_or(String::new(), |detail| format!(" — {detail}"))
            )
            .expect("String write cannot fail");
        }
        output.push('\n');
        summaries.push((target.clone(), summary));
    }
    if summaries.len() > 1 {
        writeln!(output, "COMPARISON").expect("String write cannot fail");
        writeln!(
            output,
            "Target | Implementer | Quality recall | Quality FP | Spec recall | Spec FP"
        )
        .expect("String write cannot fail");
        for (target, summary) in summaries {
            writeln!(
                output,
                "{} | {}/{} | {}/{} | {} | {}/{} | {}",
                target,
                summary.implementer_passed,
                summary.implementer_total,
                summary.quality_found,
                summary.quality_total,
                summary.quality_false_positives,
                summary.spec_found(),
                summary.spec_total(),
                summary.spec_false_positives
            )
            .expect("String write cannot fail");
        }
    }
    Ok(output)
}

fn collect_result_files(path: &Path, output: &mut Vec<PathBuf>) -> Result<(), EvalReportError> {
    if path.is_file() {
        output.push(path.to_path_buf());
        return Ok(());
    }
    if !path.is_dir() {
        return Err(EvalReportError::MissingPath(path.to_path_buf()));
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if child.is_dir() {
            collect_result_files(&child, output)?;
        } else if child.file_name().is_some_and(|name| name == "result.json") {
            output.push(child);
        }
    }
    Ok(())
}

const fn status_text(status: EvalStatus) -> &'static str {
    match status {
        EvalStatus::Passed => "PASS",
        EvalStatus::Failed => "FAIL",
        EvalStatus::InfrastructureFailure => "INFRA",
    }
}

#[derive(Default)]
struct TargetSummary {
    implementer_passed: u32,
    implementer_total: u32,
    plan_mismatch_passed: u32,
    plan_mismatch_total: u32,
    quality_found: u32,
    quality_total: u32,
    quality_false_positives: u32,
    quality_must_fix_false_positives: u32,
    spec_missing_found: u32,
    spec_missing_total: u32,
    spec_wrong_found: u32,
    spec_wrong_total: u32,
    spec_unrequested_found: u32,
    spec_unrequested_total: u32,
    spec_false_positives: u32,
    median_latency: u64,
    median_input: u64,
    median_output: u64,
    infrastructure_failures: u32,
}

impl TargetSummary {
    fn from_results(results: &[&EvalResultV1]) -> Self {
        let mut summary = Self::default();
        let mut latency = Vec::new();
        let mut input = Vec::new();
        let mut output = Vec::new();
        for result in results {
            latency.push(result.latency_ms);
            input.push(result.usage.input_units);
            output.push(result.usage.output_units);
            if result.status == EvalStatus::InfrastructureFailure {
                summary.infrastructure_failures = summary.infrastructure_failures.saturating_add(1);
            }
            match &result.metrics {
                Some(EvalMetrics::Implementer(metrics)) => {
                    summary.implementer_total = summary.implementer_total.saturating_add(1);
                    summary.implementer_passed = summary
                        .implementer_passed
                        .saturating_add(u32::from(result.status == EvalStatus::Passed));
                    if let Some(plan) = metrics.plan_mismatch_behavior {
                        summary.plan_mismatch_total = summary.plan_mismatch_total.saturating_add(1);
                        summary.plan_mismatch_passed =
                            summary.plan_mismatch_passed.saturating_add(u32::from(plan));
                    }
                }
                Some(EvalMetrics::CodeQualityReviewer(metrics)) => {
                    summary.quality_found =
                        summary.quality_found.saturating_add(metrics.defects_found);
                    summary.quality_total =
                        summary.quality_total.saturating_add(metrics.defects_total);
                    summary.quality_false_positives = summary
                        .quality_false_positives
                        .saturating_add(metrics.false_positives);
                    summary.quality_must_fix_false_positives = summary
                        .quality_must_fix_false_positives
                        .saturating_add(metrics.must_fix_false_positives);
                }
                Some(EvalMetrics::SpecReviewer(metrics)) => {
                    summary.spec_missing_found = summary
                        .spec_missing_found
                        .saturating_add(metrics.missing_found);
                    summary.spec_missing_total = summary
                        .spec_missing_total
                        .saturating_add(metrics.missing_total);
                    summary.spec_wrong_found =
                        summary.spec_wrong_found.saturating_add(metrics.wrong_found);
                    summary.spec_wrong_total =
                        summary.spec_wrong_total.saturating_add(metrics.wrong_total);
                    summary.spec_unrequested_found = summary
                        .spec_unrequested_found
                        .saturating_add(metrics.unrequested_found);
                    summary.spec_unrequested_total = summary
                        .spec_unrequested_total
                        .saturating_add(metrics.unrequested_total);
                    summary.spec_false_positives = summary
                        .spec_false_positives
                        .saturating_add(metrics.false_positives);
                }
                None => {}
            }
        }
        summary.median_latency = median(&mut latency);
        summary.median_input = median(&mut input);
        summary.median_output = median(&mut output);
        summary
    }

    const fn spec_found(&self) -> u32 {
        self.spec_missing_found + self.spec_wrong_found + self.spec_unrequested_found
    }

    const fn spec_total(&self) -> u32 {
        self.spec_missing_total + self.spec_wrong_total + self.spec_unrequested_total
    }
}

fn median(values: &mut [u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle]
    } else {
        values[middle - 1].saturating_add(values[middle]) / 2
    }
}

#[derive(Debug, Error)]
pub enum EvalReportError {
    #[error("eval report requires at least one path")]
    NoPaths,
    #[error("no result.json files found")]
    NoResults,
    #[error("eval report path does not exist: {0}")]
    MissingPath(PathBuf),
    #[error("invalid eval result {path}: {source}")]
    InvalidResult {
        path: PathBuf,
        source: EvalResultError,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_handles_even_odd_and_empty_inputs() {
        assert_eq!(median(&mut []), 0);
        assert_eq!(median(&mut [9]), 9);
        assert_eq!(median(&mut [9, 1, 5]), 5);
        assert_eq!(median(&mut [10, 2]), 6);
    }
}
