pub mod ast_engine;
pub mod cache;
pub mod config;
pub mod error;
pub mod reporter;
pub mod validator;

pub use ast_engine::{AstEngine, FileLayout, ImportRef, StructuralLayout};
pub use cache::{ArchitectureCache, CacheUpdateResult};
pub use config::{ArchitectureConfig, CompiledArchitectureConfig};
pub use error::{AddError, AddResult};
pub use reporter::{
    HtmlReportGenerator, MarkdownReportGenerator, ReportGenerator, ReportSummary,
};
pub use validator::{
    normalize_path, resolve_import_path, ValidationReport, Validator, Violation,
    ViolationSeverity, INTERFACE_BYPASS_RULE, LAYER_ALLOWLIST_RULE,
};