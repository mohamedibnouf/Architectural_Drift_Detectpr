use std::path::PathBuf;

use thiserror::Error;

pub type AddResult<T> = Result<T, AddError>;

#[derive(Debug, Error)]
pub enum AddError {
    #[error("failed to read file `{path}`: {source}")]
    IoRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to read configuration `{path}`: {source}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse architecture configuration: {0}")]
    ConfigParse(#[from] serde_yaml::Error),

    #[error("tree-sitter language setup failed: {0}")]
    LanguageSetup(String),

    #[error("tree-sitter parse failed for `{path}`")]
    ParseFailed { path: PathBuf },

    #[error("invalid tree-sitter query: {0}")]
    QueryCompile(String),

    #[error("invalid UTF-8 in source at byte offset {offset}")]
    InvalidUtf8 { offset: usize },

    #[error("target path does not exist: `{0}`")]
    TargetNotFound(PathBuf),

    #[error("no TypeScript source files found under `{0}`")]
    NoSourceFiles(PathBuf),

    #[error("failed to write report `{path}`: {source}")]
    ReportWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to render report: {0}")]
    ReportRender(String),
}
