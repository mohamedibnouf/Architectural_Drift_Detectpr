use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use walkdir::WalkDir;

use architectural_drift_detector::{
    AddError, AddResult, ArchitectureConfig, AstEngine, AstExtractor, HtmlReportGenerator,
    MarkdownReportGenerator, ReportGenerator, StructuralLayout, Validator,
};

mod lsp;

#[derive(Debug, Parser)]
#[command(
    name = "add",
    version,
    about = "Architectural Drift Detector — Tree-sitter powered architecture validation"
)]
struct Cli {
    /// Run as a Language Server Protocol service over stdin/stdout.
    #[arg(long)]
    lsp: bool,

    /// Target TypeScript/TSX file or directory to analyze (CLI mode).
    target: Option<PathBuf>,

    /// Path to architecture rules file.
    #[arg(short, long, default_value = "architecture.yaml")]
    config: PathBuf,

    /// Output format (CLI mode).
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Terminal)]
    format: OutputFormat,

    /// Write a single-file HTML architecture dashboard to the given path.
    #[arg(long, value_name = "PATH")]
    output_html: Option<PathBuf>,

    /// Write a GitHub Actions compatible Markdown summary to the given path.
    #[arg(long, value_name = "PATH")]
    output_markdown: Option<PathBuf>,

    /// Do not fail the process when violations are found (non-blocking CI check).
    #[arg(long)]
    no_fail: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Terminal,
}

fn main() -> ExitCode {
    match run() {
        Ok(exit_code) => exit_code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(2)
        }
    }
}

fn run() -> AddResult<ExitCode> {
    let cli = Cli::parse();

    if cli.lsp {
        run_lsp_server(&cli.config)?;
        return Ok(ExitCode::SUCCESS);
    }

    let target = cli
        .target
        .as_ref()
        .ok_or_else(|| AddError::TargetNotFound(PathBuf::from("<missing target>")))?;

    let config = ArchitectureConfig::from_yaml_file(&cli.config)?.compile()?;
    let source_files = collect_source_files(target)?;

    let mut layout = StructuralLayout::default();
    let mut ts_engine = AstEngine::typescript()?;
    let mut tsx_engine = AstEngine::tsx()?;

    for path in &source_files {
        let source = fs::read_to_string(path).map_err(|source| AddError::IoRead {
            path: path.clone(),
            source,
        })?;

        let file_layout = if is_tsx_path(path) {
            tsx_engine.extract_file(path, &source)?
        } else {
            ts_engine.extract_file(path, &source)?
        };

        layout.files.push(file_layout);
    }

    let report = Validator::new(config).validate_layout(&layout)?;

    match cli.format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&report).map_err(|err| {
                AddError::ReportRender(format!("failed to serialize report: {err}"))
            })?;
            println!("{json}");
        }
        OutputFormat::Terminal => render_terminal_report(&report, &mut io::stdout())?,
    }

    write_ci_reports(&report, &cli)?;

    if cli.no_fail || report.violations.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn write_ci_reports(
    report: &architectural_drift_detector::ValidationReport,
    cli: &Cli,
) -> AddResult<()> {
    if let Some(path) = &cli.output_html {
        HtmlReportGenerator.write(report, path)?;
        eprintln!("HTML report written to {}", path.display());
    }

    if let Some(path) = &cli.output_markdown {
        MarkdownReportGenerator.write(report, path)?;
        eprintln!("Markdown report written to {}", path.display());
    }

    Ok(())
}

fn run_lsp_server(config_path: &Path) -> AddResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| AddError::ReportRender(format!("tokio runtime: {err}")))?;

    runtime.block_on(lsp::run_server(config_path.to_path_buf()))
}

fn collect_source_files(target: &Path) -> AddResult<Vec<PathBuf>> {
    if !target.exists() {
        return Err(AddError::TargetNotFound(target.to_path_buf()));
    }

    let files = if target.is_file() {
        if is_supported_source(target) {
            vec![target.to_path_buf()]
        } else {
            Vec::new()
        }
    } else {
        WalkDir::new(target)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .filter(|path| is_supported_source(path))
            .collect()
    };

    if files.is_empty() {
        return Err(AddError::NoSourceFiles(target.to_path_buf()));
    }

    Ok(files)
}

fn is_supported_source(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ts" | "tsx"))
}

fn is_tsx_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tsx"))
}

fn render_terminal_report(
    report: &architectural_drift_detector::ValidationReport,
    out: &mut impl Write,
) -> AddResult<()> {
    writeln!(
        out,
        "Architectural Drift Detector — {} file(s), {} explicit rule(s), {} allowlist layer(s)",
        report.files_checked, report.rules_evaluated, report.allowlist_layers_evaluated
    )?;

    if report.violations.is_empty() {
        writeln!(out, "No architectural violations detected.")?;
        return Ok(());
    }

    writeln!(out, "\n{} violation(s) found:\n", report.violations.len())?;

    for (index, violation) in report.violations.iter().enumerate() {
        writeln!(
            out,
            "{}. [{}] {}",
            index + 1,
            violation.rule,
            violation.file.display()
        )?;
        writeln!(
            out,
            "   at {}:{} — import `{}`",
            violation.line, violation.column, violation.import_path
        )?;
        writeln!(out, "   {}", violation.message)?;

        if !violation.remediation.is_empty() {
            writeln!(out, "   remediation:")?;
            for step in &violation.remediation {
                writeln!(out, "     - {step}")?;
            }
        }

        writeln!(out)?;
    }

    Ok(())
}
