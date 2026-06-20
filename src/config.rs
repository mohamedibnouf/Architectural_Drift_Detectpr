use std::collections::HashMap;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::error::{AddError, AddResult};

/// Root configuration loaded from `architecture.yaml`.
#[derive(Debug, Clone, Deserialize)]
pub struct ArchitectureConfig {
    #[serde(default)]
    pub layers: HashMap<String, LayerDefinition>,

    #[serde(default)]
    pub rules: Vec<ArchitecturalRule>,
}

/// Declares how files are assigned to a named architectural layer.
#[derive(Debug, Clone, Deserialize)]
pub struct LayerDefinition {
    /// Glob patterns matched against normalized file paths (forward slashes).
    #[serde(default)]
    pub path_patterns: Vec<String>,

    /// When set, files in this layer may only import modules that resolve to these layers.
    #[serde(default)]
    pub allowed_dependencies: Option<Vec<String>>,

    /// When true, imports must route through domain port/interface modules.
    #[serde(default)]
    pub enforce_interface_boundary: bool,

    /// Layer key that owns `port_path_patterns` (defaults to `domain` when present).
    #[serde(default)]
    pub interface_port_layer: Option<String>,

    /// Glob patterns for permitted port/interface modules (typically on the domain layer).
    #[serde(default)]
    pub port_path_patterns: Vec<String>,
}

/// A declarative constraint evaluated by the rule engine.
#[derive(Debug, Clone, Deserialize)]
pub struct ArchitecturalRule {
    pub name: String,

    #[serde(default)]
    pub description: Option<String>,

    /// Layer keys defined in `layers` whose files are subject to this rule.
    pub source_layers: Vec<String>,

    /// Import path glob patterns that must not appear in `source_layers`.
    pub forbidden_import_patterns: Vec<String>,

    pub message: String,

    #[serde(default)]
    pub remediation: Vec<String>,
}

/// Pre-compiled glob matchers for hot-path evaluation.
#[derive(Debug, Clone)]
pub struct CompiledArchitectureConfig {
    pub raw: ArchitectureConfig,
    layer_matchers: HashMap<String, GlobSet>,
    layer_allowlists: HashMap<String, Vec<String>>,
    layer_port_matchers: HashMap<String, GlobSet>,
    layer_interface_enforcement: HashMap<String, String>,
    concrete_impl_matchers: GlobSet,
    rule_matchers: Vec<CompiledRule>,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    name: String,
    description: Option<String>,
    source_layer_matchers: Vec<GlobSet>,
    forbidden_import_matchers: GlobSet,
    message: String,
    remediation: Vec<String>,
}

/// Default glob patterns that indicate a concrete implementation bypass.
const DEFAULT_CONCRETE_IMPL_PATTERNS: &[&str] = &[
    "**/repositories/**",
    "**/*Repository*",
    "**/infrastructure/**",
    "**/data/**",
    "**/implementations/**",
    "**/adapters/**",
];

impl ArchitectureConfig {
    pub fn from_yaml_str(yaml: &str) -> AddResult<Self> {
        Ok(serde_yaml::from_str(yaml)?)
    }

    pub fn from_yaml_file(path: impl AsRef<Path>) -> AddResult<Self> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path).map_err(|source| AddError::ConfigRead {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_yaml_str(&contents)
    }

    pub fn compile(self) -> AddResult<CompiledArchitectureConfig> {
        let mut layer_matchers = HashMap::with_capacity(self.layers.len());
        let mut layer_allowlists = HashMap::with_capacity(self.layers.len());
        let mut layer_port_matchers = HashMap::with_capacity(self.layers.len());
        let mut layer_interface_enforcement = HashMap::new();

        for (layer_name, layer) in &self.layers {
            let matcher = build_glob_set(&layer.path_patterns).map_err(|err| {
                AddError::QueryCompile(format!("layer `{layer_name}`: {err}"))
            })?;
            layer_matchers.insert(layer_name.clone(), matcher);

            if let Some(allowed) = &layer.allowed_dependencies {
                for dep in allowed {
                    if !self.layers.contains_key(dep) {
                        return Err(AddError::QueryCompile(format!(
                            "layer `{layer_name}` references unknown allowed dependency `{dep}`"
                        )));
                    }
                }
                layer_allowlists.insert(layer_name.clone(), allowed.clone());
            }

            if !layer.port_path_patterns.is_empty() {
                let port_matcher = build_glob_set(&layer.port_path_patterns).map_err(|err| {
                    AddError::QueryCompile(format!("layer `{layer_name}` port patterns: {err}"))
                })?;
                layer_port_matchers.insert(layer_name.clone(), port_matcher);
            }

            if layer.enforce_interface_boundary {
                let port_layer = layer
                    .interface_port_layer
                    .clone()
                    .or_else(|| {
                        if self.layers.contains_key("domain") {
                            Some("domain".to_owned())
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| {
                        AddError::QueryCompile(format!(
                            "layer `{layer_name}` has enforce_interface_boundary but no interface_port_layer"
                        ))
                    })?;

                if !self.layers.contains_key(&port_layer) {
                    return Err(AddError::QueryCompile(format!(
                        "layer `{layer_name}` references unknown interface_port_layer `{port_layer}`"
                    )));
                }

                if !layer_port_matchers.contains_key(&port_layer) {
                    return Err(AddError::QueryCompile(format!(
                        "layer `{port_layer}` must define port_path_patterns for interface enforcement"
                    )));
                }

                layer_interface_enforcement.insert(layer_name.clone(), port_layer);
            }
        }

        let concrete_impl_matchers =
            build_glob_set_str(DEFAULT_CONCRETE_IMPL_PATTERNS).map_err(|err| {
                AddError::QueryCompile(format!("concrete implementation patterns: {err}"))
            })?;

        let mut rule_matchers = Vec::with_capacity(self.rules.len());

        for rule in &self.rules {
            let mut source_layer_matchers = Vec::with_capacity(rule.source_layers.len());
            for layer_name in &rule.source_layers {
                let matcher = layer_matchers.get(layer_name).ok_or_else(|| {
                    AddError::QueryCompile(format!(
                        "rule `{}` references unknown layer `{layer_name}`",
                        rule.name
                    ))
                })?;
                source_layer_matchers.push(matcher.clone());
            }

            let forbidden_import_matchers =
                build_glob_set(&rule.forbidden_import_patterns).map_err(|err| {
                    AddError::QueryCompile(format!("rule `{}`: {err}", rule.name))
                })?;

            rule_matchers.push(CompiledRule {
                name: rule.name.clone(),
                description: rule.description.clone(),
                source_layer_matchers,
                forbidden_import_matchers,
                message: rule.message.clone(),
                remediation: rule.remediation.clone(),
            });
        }

        Ok(CompiledArchitectureConfig {
            raw: self,
            layer_matchers,
            layer_allowlists,
            layer_port_matchers,
            layer_interface_enforcement,
            concrete_impl_matchers,
            rule_matchers,
        })
    }
}

impl CompiledArchitectureConfig {
    pub fn layer_for_path(&self, normalized_path: &str) -> Option<&str> {
        self.layer_matchers
            .iter()
            .find(|(_, matcher)| matcher.is_match(normalized_path))
            .map(|(name, _)| name.as_str())
    }

    pub fn allowed_dependencies_for_layer(&self, layer: &str) -> Option<&[String]> {
        self.layer_allowlists.get(layer).map(Vec::as_slice)
    }

    pub fn layers_with_allowlists(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.layer_allowlists
            .iter()
            .map(|(name, allowed)| (name.as_str(), allowed.as_slice()))
    }

    pub fn interface_port_layer_for(&self, layer: &str) -> Option<&str> {
        self.layer_interface_enforcement
            .get(layer)
            .map(String::as_str)
    }

    pub fn port_matchers_for_layer(&self, layer: &str) -> Option<&GlobSet> {
        self.layer_port_matchers.get(layer)
    }

    pub fn layers_with_interface_enforcement(&self) -> impl Iterator<Item = &str> {
        self.layer_interface_enforcement.keys().map(String::as_str)
    }

    pub fn concrete_impl_matchers(&self) -> &GlobSet {
        &self.concrete_impl_matchers
    }
}

/// Borrowed view of a compiled rule for the validator hot path.
#[derive(Debug, Clone, Copy)]
pub struct CompiledRuleView<'a> {
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub source_layer_matchers: &'a [GlobSet],
    pub forbidden_import_matchers: &'a GlobSet,
    pub message: &'a str,
    pub remediation: &'a [String],
}

impl CompiledArchitectureConfig {
    pub fn compiled_rules(&self) -> impl Iterator<Item = CompiledRuleView<'_>> + '_ {
        self.rule_matchers.iter().map(|rule| CompiledRuleView {
            name: &rule.name,
            description: rule.description.as_deref(),
            source_layer_matchers: &rule.source_layer_matchers,
            forbidden_import_matchers: &rule.forbidden_import_matchers,
            message: &rule.message,
            remediation: &rule.remediation,
        })
    }
}

fn build_glob_set(patterns: &[String]) -> Result<GlobSet, globset::Error> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }
    builder.build()
}

fn build_glob_set_str(patterns: &[&str]) -> Result<GlobSet, globset::Error> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
layers:
  presentation:
    path_patterns:
      - "**/presentation/**"
      - "**/ui/**"
    allowed_dependencies:
      - domain
    enforce_interface_boundary: true
  domain:
    path_patterns:
      - "**/domain/**"
    port_path_patterns:
      - "**/domain/ports/**"
      - "**/domain/interfaces/**"
  infrastructure:
    path_patterns:
      - "**/data/**"
      - "**/infrastructure/**"

rules:
  - name: no-presentation-to-infrastructure
    source_layers: [presentation]
    forbidden_import_patterns:
      - "**/data/**"
      - "**/infrastructure/**"
    message: "Presentation/UI layers cannot import infrastructure paths."
    remediation:
      - "Extract shared contracts into a domain layer."
"#;

    #[test]
    fn parses_and_compiles_sample_config() {
        let config = ArchitectureConfig::from_yaml_str(SAMPLE).expect("yaml parse");
        let compiled = config.compile().expect("compile");
        assert!(compiled
            .layer_for_path("src/presentation/components/Button.tsx")
            .is_some());
        assert_eq!(
            compiled.allowed_dependencies_for_layer("presentation"),
            Some(["domain".to_owned()].as_slice())
        );
        assert_eq!(
            compiled.interface_port_layer_for("presentation"),
            Some("domain")
        );
    }
}
