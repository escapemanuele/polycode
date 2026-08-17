//! Experimental role-specific evaluation harness.
//!
//! Evaluation uses isolated production orchestration, then writes separate
//! versioned evidence files. Normal runs and Recommended routing never read it.

mod case;
mod report;
mod result;
mod runner;
mod scorer;
mod suite;

pub use case::{EvalCase, ROLE_CORE_SUITE_VERSION};
pub use report::{EvalReportError, load_results, render_report};
pub use result::{
    EVAL_RESULT_SCHEMA_VERSION, EvalMetrics, EvalProvider, EvalResultError, EvalResultV1,
    EvalStatus, EvalTarget, EvalUsage, ImplementerMetrics, QualityMetrics, SpecMetrics,
};
pub use runner::{EvalRunOptions, EvalRunSummary, EvalRunner, EvalRunnerError};
pub use suite::{EvalSuite, EvalSuiteError};
