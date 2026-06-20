use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::ast_engine::{FileLayout, StructuralLayout};
use crate::config::CompiledArchitectureConfig;
use crate::error::AddResult;

pub const LAYER_ALLOWLIST_RULE: &str = "layer-allowlist";
pub const INTERFACE_BYPASS_RULE: &str = "interface-bypass-violation";

/// Relative severity used for IDE presentation and filtering.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum ViolationSeverity {
    Normal,
    High,
}

/// A single architectural rule violation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Violation {
    pub rule: String,
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
    pub import_path: String,
    pub message: String,
    pub remediation: Vec<String>,
    pub severity: ViolationSeverity,
}

impl Violation {
    pub fn is_high_severity(&self) -> bool {
        self.severity == ViolationSeverity::High
    }
}

/// Outcome of validating a structural layout.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ValidationReport {
    pub violations: Vec<Violation>,
    pub files_checked: usize,
    pub rules_evaluated: usize,
    pub allowlist_layers_evaluated: usize,
}

/// Compares extracted layout facts against declarative architecture rules.
pub struct Validator {
    config: Arc<CompiledArchitectureConfig>,
}

impl Validator {
    pub fn new(config: CompiledArchitectureConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    pub fn with_shared(config: Arc<CompiledArchitectureConfig>) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &CompiledArchitectureConfig {
        self.config.as_ref()
    }

    pub fn shared_config(&self) -> Arc<CompiledArchitectureConfig> {
        Arc::clone(&self.config)
    }

    pub fn validate_layout(&self, layout: &StructuralLayout) -> AddResult<ValidationReport> {
        let rules: Vec<_> = self.config.compiled_rules().collect();
        let allowlist_layers_evaluated = self.config.layers_with_allowlists().count();
        let mut violations = Vec::new();

        for file in &layout.files {
            let normalized = normalize_path(&file.file_path);
            violations.extend(self.validate_file(&normalized, file, &rules));
        }

        violations.shrink_to_fit();

        Ok(ValidationReport {
            violations,
            files_checked: layout.files.len(),
            rules_evaluated: rules.len(),
            allowlist_layers_evaluated,
        })
    }

    pub fn validate_file_layout(&self, file: &FileLayout) -> AddResult<Vec<Violation>> {
        let rules: Vec<_> = self.config.compiled_rules().collect();
        let normalized = normalize_path(&file.file_path);
        Ok(self.validate_file(&normalized, file, &rules))
    }

    fn validate_file(
        &self,
        normalized_path: &str,
        file: &FileLayout,
        rules: &[crate::config::CompiledRuleView<'_>],
    ) -> Vec<Violation> {
        let mut violations = Vec::new();

        for rule in rules {
            if !rule_applies_to_file(rule, normalized_path) {
                continue;
            }

            for import in &file.imports {
                if rule.forbidden_import_matchers.is_match(&import.path) {
                    violations.push(Violation {
                        rule: rule.name.to_owned(),
                        file: file.file_path.clone(),
                        line: import.line,
                        column: import.column,
                        import_path: import.path.clone(),
                        message: rule.message.to_owned(),
                        remediation: rule.remediation.to_vec(),
                        severity: ViolationSeverity::Normal,
                    });
                }
            }
        }

        let source_layer = self.config.layer_for_path(normalized_path);
        if let Some(source_layer) = source_layer {
            if let Some(allowed) = self.config.allowed_dependencies_for_layer(source_layer) {
                for import in &file.imports {
                    let Some(resolved) = resolve_import_path(&file.file_path, &import.path) else {
                        continue;
                    };

                    let Some(target_layer) = self.config.layer_for_path(&resolved) else {
                        continue;
                    };

                    if target_layer == source_layer {
                        continue;
                    }

                    if allowed.iter().any(|layer| layer == target_layer) {
                        continue;
                    }

                    violations.push(Violation {
                        rule: LAYER_ALLOWLIST_RULE.to_owned(),
                        file: file.file_path.clone(),
                        line: import.line,
                        column: import.column,
                        import_path: import.path.clone(),
                        message: format!(
                            "Layer `{source_layer}` may only depend on [{}], but import resolves to layer `{target_layer}`.",
                            allowed.join(", ")
                        ),
                        remediation: vec![
                            format!(
                                "Remove the dependency on `{target_layer}` or add it to `{source_layer}.allowed_dependencies`."
                            ),
                            "Move shared contracts to an allowed layer.".into(),
                        ],
                        severity: ViolationSeverity::Normal,
                    });
                }
            }

            violations.extend(self.check_interface_boundary(
                source_layer,
                normalized_path,
                file,
            ));
        }

        violations
    }

    fn check_interface_boundary(
        &self,
        source_layer: &str,
        _normalized_path: &str,
        file: &FileLayout,
    ) -> Vec<Violation> {
        let Some(port_layer) = self.config.interface_port_layer_for(source_layer) else {
            return Vec::new();
        };

        let Some(port_matchers) = self.config.port_matchers_for_layer(port_layer) else {
            return Vec::new();
        };

        let concrete_matchers = self.config.concrete_impl_matchers();
        let mut violations = Vec::new();

        for import in &file.imports {
            let resolved = resolve_import_path(&file.file_path, &import.path);
            let resolved_str = resolved.as_deref().unwrap_or(import.path.as_str());

            let matches_port = port_matchers.is_match(resolved_str) || port_matchers.is_match(&import.path);
            if matches_port {
                continue;
            }

            let target_layer = self.config.layer_for_path(resolved_str);
            if target_layer == Some(source_layer) {
                continue;
            }

            let is_concrete = concrete_matchers.is_match(resolved_str)
                || concrete_matchers.is_match(&import.path);
            let bypasses_infrastructure = target_layer == Some("infrastructure");
            let bypasses_domain_impl = target_layer == Some(port_layer) && is_concrete;

            if bypasses_infrastructure || bypasses_domain_impl || is_concrete {
                violations.push(Violation {
                    rule: INTERFACE_BYPASS_RULE.to_owned(),
                    file: file.file_path.clone(),
                    line: import.line,
                    column: import.column,
                    import_path: import.path.clone(),
                    message: format!(
                        "Layer `{source_layer}` must depend on `{port_layer}` ports/interfaces, not concrete implementations (`{}`).",
                        import.path
                    ),
                    remediation: vec![
                        format!(
                            "Import from `{port_layer}` port modules (e.g. `domain/ports/*` or `domain/interfaces/*`)."
                        ),
                        "Inject concrete implementations at the composition root.".into(),
                        "Replace direct Repository/Adapter imports with their interface definitions.".into(),
                    ],
                    severity: ViolationSeverity::High,
                });
            }
        }

        violations
    }
}

fn rule_applies_to_file(
    rule: &crate::config::CompiledRuleView<'_>,
    normalized_path: &str,
) -> bool {
    rule.source_layer_matchers
        .iter()
        .any(|matcher| matcher.is_match(normalized_path))
}

/// Normalize paths for glob matching: forward slashes, no leading `./`.
pub fn normalize_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    raw.trim_start_matches("./").to_owned()
}

/// Resolve a relative or project-root import path against the importing file.
pub fn resolve_import_path(file_path: &Path, import_path: &str) -> Option<String> {
    if import_path.starts_with('.') {
        let base = file_path.parent()?;
        let joined = base.join(import_path);
        return Some(normalize_path_components(joined));
    }

    if import_path.starts_with('/') {
        let joined = PathBuf::from(import_path.trim_start_matches('/'));
        return Some(normalize_path_components(joined));
    }

    None
}

fn normalize_path_components(path: PathBuf) -> String {
    let mut parts = Vec::new();

    for component in path.components() {
        match component {
            Component::Normal(segment) => parts.push(segment.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {}
        }
    }

    parts.join("/")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::ast_engine::{FileLayout, ImportRef, StructuralLayout};
    use crate::config::{ArchitecturalRule, ArchitectureConfig, LayerDefinition};

    fn sample_config() -> CompiledArchitectureConfig {
        let config = ArchitectureConfig {
            layers: HashMap::from([
                (
                    "presentation".to_owned(),
                    LayerDefinition {
                        path_patterns: vec!["**/presentation/**".into(), "**/ui/**".into()],
                        allowed_dependencies: Some(vec!["domain".into()]),
                        enforce_interface_boundary: true,
                        interface_port_layer: Some("domain".into()),
                        port_path_patterns: vec![],
                    },
                ),
                (
                    "domain".to_owned(),
                    LayerDefinition {
                        path_patterns: vec!["**/domain/**".into()],
                        allowed_dependencies: None,
                        enforce_interface_boundary: false,
                        interface_port_layer: None,
                        port_path_patterns: vec![
                            "**/domain/ports/**".into(),
                            "**/domain/interfaces/**".into(),
                        ],
                    },
                ),
                (
                    "infrastructure".to_owned(),
                    LayerDefinition {
                        path_patterns: vec!["**/data/**".into(), "**/infrastructure/**".into()],
                        allowed_dependencies: None,
                        enforce_interface_boundary: false,
                        interface_port_layer: None,
                        port_path_patterns: vec![],
                    },
                ),
            ]),
            rules: vec![ArchitecturalRule {
                name: "no-presentation-to-infrastructure".into(),
                description: None,
                source_layers: vec!["presentation".into()],
                forbidden_import_patterns: vec!["**/data/**".into(), "**/infrastructure/**".into()],
                message: "Presentation/UI layers cannot import infrastructure paths.".into(),
                remediation: vec!["Use a domain-facing port instead.".into()],
            }],
        };

        config.compile().expect("compile")
    }

    #[test]
    fn flags_forbidden_import_from_ui_layer() {
        let validator = Validator::new(sample_config());
        let layout = StructuralLayout {
            files: vec![FileLayout {
                file_path: PathBuf::from("src/ui/HomePage.tsx"),
                imports: vec![ImportRef {
                    path: "../data/infrastructure/UserRepo".into(),
                    line: 3,
                    column: 1,
                }],
            }],
        };

        let report = validator.validate_layout(&layout).expect("validate");
        assert!(report.violations.iter().any(|v| v.rule == "no-presentation-to-infrastructure"));
    }

    #[test]
    fn flags_disallowed_layer_dependency_via_allowlist() {
        let validator = Validator::new(sample_config());
        let layout = StructuralLayout {
            files: vec![FileLayout {
                file_path: PathBuf::from("src/ui/HomePage.tsx"),
                imports: vec![ImportRef {
                    path: "../infrastructure/cache".into(),
                    line: 2,
                    column: 1,
                }],
            }],
        };

        let report = validator.validate_layout(&layout).expect("validate");
        assert!(report
            .violations
            .iter()
            .any(|v| v.rule == LAYER_ALLOWLIST_RULE));
    }

    #[test]
    fn allows_import_targeting_permitted_layer() {
        let validator = Validator::new(sample_config());
        let layout = StructuralLayout {
            files: vec![FileLayout {
                file_path: PathBuf::from("src/ui/HomePage.tsx"),
                imports: vec![ImportRef {
                    path: "../domain/ports/IUserRepository".into(),
                    line: 2,
                    column: 1,
                }],
            }],
        };

        let report = validator.validate_layout(&layout).expect("validate");
        assert!(report.violations.is_empty());
    }

    #[test]
    fn flags_interface_bypass_for_concrete_repository() {
        let validator = Validator::new(sample_config());
        let layout = StructuralLayout {
            files: vec![FileLayout {
                file_path: PathBuf::from("src/ui/HomePage.tsx"),
                imports: vec![ImportRef {
                    path: "../domain/repositories/UserRepository".into(),
                    line: 2,
                    column: 1,
                }],
            }],
        };

        let report = validator.validate_layout(&layout).expect("validate");
        let bypass = report
            .violations
            .iter()
            .find(|v| v.rule == INTERFACE_BYPASS_RULE)
            .expect("interface bypass");
        assert!(bypass.is_high_severity());
    }

    #[test]
    fn resolves_relative_import_paths() {
        let resolved =
            resolve_import_path(Path::new("src/ui/Home.tsx"), "../domain/models/User").expect("resolved");
        assert_eq!(resolved, "src/domain/models/User");
    }
}
