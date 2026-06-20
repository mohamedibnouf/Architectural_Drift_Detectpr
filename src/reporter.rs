//! CI/CD visual reporting for architectural validation results.
//!
//! Generates single-file HTML dashboards and GitHub Actions Markdown summaries
//! from a project-wide [`ValidationReport`].

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::error::{AddError, AddResult};
use crate::validator::{ValidationReport, Violation, ViolationSeverity};

const HTML_TEMPLATE: &str = include_str!("templates/report.html");

/// Penalty applied per high-severity violation when computing health score.
const HIGH_SEVERITY_PENALTY: u32 = 8;
/// Penalty applied per warning-level violation when computing health score.
const WARNING_SEVERITY_PENALTY: u32 = 3;

/// Aggregated metrics derived from a validation report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportSummary {
    pub files_scanned: usize,
    pub total_violations: usize,
    pub high_errors: usize,
    pub warnings: usize,
    pub health_score: u8,
}

impl ReportSummary {
    pub fn from_report(report: &ValidationReport) -> Self {
        let high_errors = report
            .violations
            .iter()
            .filter(|v| v.severity == ViolationSeverity::High)
            .count();
        let warnings = report.violations.len().saturating_sub(high_errors);
        let health_score = compute_health_score(high_errors, warnings);

        Self {
            files_scanned: report.files_checked,
            total_violations: report.violations.len(),
            high_errors,
            warnings,
            health_score,
        }
    }

    pub fn health_color(&self) -> &'static str {
        if self.health_score >= 90 {
            "#22c55e"
        } else if self.health_score >= 70 {
            "#f59e0b"
        } else {
            "#ef4444"
        }
    }
}

/// Exports validation results into a deliverable artifact.
pub trait ReportGenerator {
    fn format_name(&self) -> &'static str;

    fn render(&self, report: &ValidationReport) -> AddResult<String>;

    fn write(&self, report: &ValidationReport, path: &Path) -> AddResult<()> {
        let content = self.render(report)?;
        fs::write(path, content.as_bytes()).map_err(|source| AddError::ReportWrite {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// Single-file responsive HTML dashboard exporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct HtmlReportGenerator;

impl ReportGenerator for HtmlReportGenerator {
    fn format_name(&self) -> &'static str {
        "html"
    }

    fn render(&self, report: &ValidationReport) -> AddResult<String> {
        let summary = ReportSummary::from_report(report);
        let generated_at = chrono_lite_timestamp();

        let mut output = HTML_TEMPLATE.to_owned();
        replace_placeholder(&mut output, "{{GENERATED_AT}}", &escape_html(&generated_at));
        replace_placeholder(&mut output, "{{FILES_SCANNED}}", &summary.files_scanned.to_string());
        replace_placeholder(&mut output, "{{TOTAL_VIOLATIONS}}", &summary.total_violations.to_string());
        replace_placeholder(&mut output, "{{HIGH_COUNT}}", &summary.high_errors.to_string());
        replace_placeholder(&mut output, "{{WARNING_COUNT}}", &summary.warnings.to_string());
        replace_placeholder(&mut output, "{{HEALTH_SCORE}}", &summary.health_score.to_string());
        replace_placeholder(&mut output, "{{HEALTH_COLOR}}", summary.health_color());
        replace_placeholder(
            &mut output,
            "{{VIOLATIONS_TABLE}}",
            &render_violations_table(&report.violations),
        );

        Ok(output)
    }
}

/// GitHub Actions step-summary compatible Markdown exporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownReportGenerator;

impl ReportGenerator for MarkdownReportGenerator {
    fn format_name(&self) -> &'static str {
        "markdown"
    }

    fn render(&self, report: &ValidationReport) -> AddResult<String> {
        let summary = ReportSummary::from_report(report);
        let mut md = String::with_capacity(report.violations.len() * 192 + 512);

        writeln!(md, "# Architectural Drift Report").map_err(fmt_err)?;
        writeln!(md).map_err(fmt_err)?;
        writeln!(md, "## Summary").map_err(fmt_err)?;
        writeln!(md).map_err(fmt_err)?;
        writeln!(md, "| Metric | Value |").map_err(fmt_err)?;
        writeln!(md, "| --- | --- |").map_err(fmt_err)?;
        writeln!(md, "| Files Scanned | {} |", summary.files_scanned).map_err(fmt_err)?;
        writeln!(md, "| Total Violations | {} |", summary.total_violations).map_err(fmt_err)?;
        writeln!(
            md,
            "| Architecture Health | {}% |",
            summary.health_score
        )
        .map_err(fmt_err)?;
        writeln!(md, "| High Errors | {} |", summary.high_errors).map_err(fmt_err)?;
        writeln!(md, "| Warnings | {} |", summary.warnings).map_err(fmt_err)?;
        writeln!(md, "| Rules Evaluated | {} |", report.rules_evaluated).map_err(fmt_err)?;
        writeln!(
            md,
            "| Allowlist Layers | {} |",
            report.allowlist_layers_evaluated
        )
        .map_err(fmt_err)?;

        if summary.total_violations == 0 {
            writeln!(md).map_err(fmt_err)?;
            writeln!(md, "> No architectural violations detected.").map_err(fmt_err)?;
            return Ok(md);
        }

        writeln!(md).map_err(fmt_err)?;
        writeln!(md, "## Violations by Severity").map_err(fmt_err)?;
        writeln!(md).map_err(fmt_err)?;
        writeln!(md, "- **High errors:** {}", summary.high_errors).map_err(fmt_err)?;
        writeln!(md, "- **Warnings:** {}", summary.warnings).map_err(fmt_err)?;

        writeln!(md).map_err(fmt_err)?;
        writeln!(md, "<details>").map_err(fmt_err)?;
        writeln!(md, "<summary>Violation details ({} total)</summary>", summary.total_violations)
            .map_err(fmt_err)?;
        writeln!(md).map_err(fmt_err)?;

        for (index, violation) in report.violations.iter().enumerate() {
            let severity = severity_label(violation.severity);
            let file = violation.file.display();
            writeln!(
                md,
                "### {index}. [{severity}] `{rule}` — `{file}`",
                index = index + 1,
                severity = severity,
                rule = violation.rule,
                file = file
            )
            .map_err(fmt_err)?;
            writeln!(
                md,
                "- **Location:** line {}, column {}",
                violation.line, violation.column
            )
            .map_err(fmt_err)?;
            writeln!(md, "- **Import:** `{}`", violation.import_path).map_err(fmt_err)?;
            writeln!(md, "- **Message:** {}", violation.message).map_err(fmt_err)?;

            if !violation.remediation.is_empty() {
                writeln!(md, "- **Remediation:**").map_err(fmt_err)?;
                for step in &violation.remediation {
                    writeln!(md, "  - {step}").map_err(fmt_err)?;
                }
            }

            writeln!(md).map_err(fmt_err)?;
        }

        writeln!(md, "</details>").map_err(fmt_err)?;
        Ok(md)
    }
}

fn compute_health_score(high_errors: usize, warnings: usize) -> u8 {
    let penalty = high_errors
        .saturating_mul(HIGH_SEVERITY_PENALTY as usize)
        .saturating_add(warnings.saturating_mul(WARNING_SEVERITY_PENALTY as usize));
    100u8.saturating_sub(penalty.min(100) as u8)
}

fn render_violations_table(violations: &[Violation]) -> String {
    if violations.is_empty() {
        return r#"<div class="p-12 text-center text-slate-400">No architectural violations detected. Your codebase is architecturally healthy.</div>"#.into();
    }

    let mut table = String::with_capacity(violations.len() * 512 + 512);
    table.push_str(r#"<table class="w-full min-w-[800px] text-left text-sm"><thead class="border-b border-slate-800 bg-slate-800/50 text-xs uppercase tracking-wider text-slate-400"><tr>"#);
    table.push_str("<th class=\"px-4 py-3\">Severity</th><th class=\"px-4 py-3\">Rule</th><th class=\"px-4 py-3\">File</th><th class=\"px-4 py-3\">Location</th><th class=\"px-4 py-3\">Import</th><th class=\"px-4 py-3\">Message</th><th class=\"px-4 py-3\">Remediation</th>");
    table.push_str("</tr></thead><tbody id=\"violations-body\" class=\"divide-y divide-slate-800\">");

    for violation in violations {
        let severity_key = if violation.severity == ViolationSeverity::High {
            "high"
        } else {
            "normal"
        };
        let sev_label = severity_label(violation.severity);
        let badge_classes = if violation.severity == ViolationSeverity::High {
            "rounded px-2 py-0.5 text-xs font-semibold uppercase bg-red-500/20 text-red-400"
        } else {
            "rounded px-2 py-0.5 text-xs font-semibold uppercase bg-amber-500/20 text-amber-400"
        };

        table.push_str(&format!("<tr data-severity=\"{severity_key}\" class=\"hover:bg-slate-800/40\">"));
        table.push_str(&format!(
            "<td class=\"px-4 py-3\"><span class=\"{badge_classes}\">{sev_label}</span></td>"
        ));
        table.push_str(&format!(
            "<td class=\"px-4 py-3 font-mono text-xs text-blue-400\">{}</td>",
            escape_html(&violation.rule)
        ));
        table.push_str(&format!(
            "<td class=\"px-4 py-3 font-mono text-xs break-all\">{}</td>",
            escape_html(&violation.file.display().to_string())
        ));
        table.push_str(&format!(
            "<td class=\"px-4 py-3 text-slate-300\">{}:{}</td>",
            violation.line, violation.column
        ));
        table.push_str(&format!(
            "<td class=\"px-4 py-3 font-mono text-xs break-all\">{}</td>",
            escape_html(&violation.import_path)
        ));
        table.push_str(&format!(
            "<td class=\"px-4 py-3 text-slate-300\">{}</td>",
            escape_html(&violation.message)
        ));
        table.push_str("<td class=\"px-4 py-3 text-slate-400\"><ul class=\"list-disc pl-4 space-y-1\">");
        for step in &violation.remediation {
            table.push_str(&format!("<li class=\"text-xs\">{}</li>", escape_html(step)));
        }
        table.push_str("</ul></td></tr>");
    }

    table.push_str("</tbody></table>");
    table
}

fn severity_label(severity: ViolationSeverity) -> &'static str {
    match severity {
        ViolationSeverity::High => "High",
        ViolationSeverity::Normal => "Warning",
    }
}

fn replace_placeholder(template: &mut String, placeholder: &str, value: &str) {
    if let Some(index) = template.find(placeholder) {
        template.replace_range(index..index + placeholder.len(), value);
    }
}

fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn chrono_lite_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("UTC {secs}")
}

fn fmt_err(err: std::fmt::Error) -> AddError {
    AddError::ReportRender(err.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::validator::Violation;

    fn sample_report() -> ValidationReport {
        ValidationReport {
            files_checked: 10,
            rules_evaluated: 2,
            allowlist_layers_evaluated: 1,
            violations: vec![
                Violation {
                    rule: "interface-bypass-violation".into(),
                    file: PathBuf::from("src/ui/Home.tsx"),
                    line: 3,
                    column: 1,
                    import_path: "../data/UserRepo".into(),
                    message: "Concrete import bypasses port".into(),
                    remediation: vec!["Use domain port".into()],
                    severity: ViolationSeverity::High,
                },
                Violation {
                    rule: "layer-allowlist".into(),
                    file: PathBuf::from("src/ui/Panel.tsx"),
                    line: 5,
                    column: 1,
                    import_path: "../infra/cache".into(),
                    message: "Layer not allowed".into(),
                    remediation: vec!["Remove import".into()],
                    severity: ViolationSeverity::Normal,
                },
            ],
        }
    }

    #[test]
    fn computes_health_score_with_severity_weighting() {
        let summary = ReportSummary::from_report(&sample_report());
        assert_eq!(summary.high_errors, 1);
        assert_eq!(summary.warnings, 1);
        assert_eq!(summary.health_score, 89);
    }

    #[test]
    fn html_report_contains_summary_and_violations() {
        let html = HtmlReportGenerator.render(&sample_report()).expect("html");
        assert!(html.contains("Architecture Health"));
        assert!(html.contains("interface-bypass-violation"));
        assert!(html.contains("id=\"search\""));
        assert!(html.contains("tailwindcss.com"));
        assert!(html.contains("89%") || html.contains(">89<"));
    }

    #[test]
    fn markdown_report_is_github_actions_compatible() {
        let md = MarkdownReportGenerator.render(&sample_report()).expect("md");
        assert!(md.contains("# Architectural Drift Report"));
        assert!(md.contains("| Architecture Health | 89% |"));
        assert!(md.contains("<details>"));
        assert!(md.contains("interface-bypass-violation"));
    }

    #[test]
    fn empty_report_renders_clean_health() {
        let report = ValidationReport {
            files_checked: 5,
            ..Default::default()
        };
        let summary = ReportSummary::from_report(&report);
        assert_eq!(summary.health_score, 100);
        let html = HtmlReportGenerator.render(&report).expect("html");
        assert!(html.contains("No architectural violations"));
    }
}
