//! Imports from other local environments. Only Laragon so far ([`laragon`]).
//!
//! - [`laragon`]: [`LaragonImporter::scan`] → [`ImportPlan`] (read-only) and
//!   [`LaragonImporter::apply`] (`apply` module) → [`ApplyOutcome`] with the structured
//!   [`ImportReport`].
//! - `conf`: readers for the Apache/Nginx vhost files; `ini`: `usr/laragon.ini`.

mod apply;
pub mod conf;
pub mod ini;
mod laragon;
mod messages;
mod plan;

pub use apply::{ApplyEnv, ApplyOutcome, CaseConvertFn};
pub use laragon::LaragonImporter;
pub use plan::{
    ImportOptions, ImportPlan, ImportReport, PlannedDatabase, PlannedParked, PlannedPrefs,
    PlannedSite, PlannedTool, ReportItem, Skipped, kind,
};
