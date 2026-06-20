//! Incremental dependency graph cache for near-O(1) LSP feedback loops.
//!
//! Stores per-file [`FileLayout`] snapshots and a reverse import index so that a
//! single edit only re-parses the changed file and re-validates it plus immediate
//! dependents (files that import it).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::RwLock as FastRwLock;

use crate::ast_engine::{AstEngine, AstExtractor, FileLayout};
use crate::error::AddResult;
use crate::validator::{normalize_path, resolve_import_path, Validator, Violation};

/// Result of an incremental cache update.
#[derive(Debug, Default)]
pub struct CacheUpdateResult {
    pub diagnostics: Vec<(PathBuf, Vec<Violation>)>,
    pub files_reparsed: usize,
    pub files_revalidated: usize,
}

/// In-memory structural layout cache with reverse dependency tracking.
pub struct ArchitectureCache {
    layouts: DashMap<PathBuf, FileLayout>,
    importers: DashMap<PathBuf, HashSet<PathBuf>>,
    validator: Arc<Validator>,
    /// Fast lock coordinating invalidation bursts during concurrent LSP edits.
    invalidation_lock: FastRwLock<()>,
}

impl ArchitectureCache {
    pub fn new(validator: Validator) -> Self {
        Self {
            layouts: DashMap::new(),
            importers: DashMap::new(),
            validator: Arc::new(validator),
            invalidation_lock: FastRwLock::new(()),
        }
    }

    pub fn with_shared_validator(validator: Arc<Validator>) -> Self {
        Self {
            layouts: DashMap::new(),
            importers: DashMap::new(),
            validator,
            invalidation_lock: FastRwLock::new(()),
        }
    }

    pub fn layout_count(&self) -> usize {
        self.layouts.len()
    }

    pub fn get_layout(&self, path: &Path) -> Option<FileLayout> {
        self.layouts.get(&Self::cache_key(path)).map(|e| e.clone())
    }

    /// Remove a file from the cache (e.g. on `textDocument/didClose`).
    pub fn remove_file(&self, path: &Path) {
        let key = Self::cache_key(path);
        if let Some((_, layout)) = self.layouts.remove(&key) {
            self.remove_outgoing_edges(&key, &layout);
        }
        self.importers.remove(&key);
    }

    /// Re-parse the changed file, refresh the dependency graph, and re-validate
    /// the changed file plus its immediate dependents.
    pub fn process_change(
        &self,
        changed_path: &Path,
        source: &str,
    ) -> AddResult<CacheUpdateResult> {
        let _invalidation = self.invalidation_lock.write();

        let key = Self::cache_key(changed_path);
        let mut engine = AstEngine::for_path(changed_path)?;
        let new_layout = engine.extract_file(changed_path, source)?;

        if let Some(old) = self.layouts.get(&key) {
            self.remove_outgoing_edges(&key, &old);
        }

        self.add_outgoing_edges(&key, &new_layout);
        self.layouts.insert(key.clone(), new_layout);

        let mut affected = HashSet::new();
        affected.insert(key.clone());
        for variant in extension_variants(&key) {
            if let Some(deps) = self.importers.get(&variant) {
                affected.extend(deps.iter().cloned());
            }
        }

        let mut result = CacheUpdateResult {
            files_reparsed: 1,
            files_revalidated: affected.len(),
            ..Default::default()
        };

        for path in affected {
            let Some(layout) = self.layouts.get(&path).map(|e| e.clone()) else {
                continue;
            };
            let violations = self.validator.validate_file_layout(&layout)?;
            result.diagnostics.push((path, violations));
        }

        Ok(result)
    }

    fn cache_key(path: &Path) -> PathBuf {
        PathBuf::from(normalize_path(path))
    }

    fn add_outgoing_edges(&self, importer: &Path, layout: &FileLayout) {
        for import in &layout.imports {
            for target in import_target_keys(&layout.file_path, &import.path) {
                self.importers
                    .entry(target)
                    .or_default()
                    .insert(importer.to_path_buf());
            }
        }
    }

    fn remove_outgoing_edges(&self, importer: &Path, layout: &FileLayout) {
        for import in &layout.imports {
            for target in import_target_keys(&layout.file_path, &import.path) {
                if let Some(mut set) = self.importers.get_mut(&target) {
                    set.remove(importer);
                    if set.is_empty() {
                        drop(set);
                        self.importers.remove(&target);
                    }
                }
            }
        }
    }
}

/// Build index keys for an import target, including extension variants.
fn import_target_keys(file_path: &Path, import_path: &str) -> Vec<PathBuf> {
    let Some(resolved) = resolve_import_path(file_path, import_path) else {
        return Vec::new();
    };

    let base = PathBuf::from(resolved);
    let mut keys = vec![base.clone()];

    if base.extension().is_none() {
        keys.push(base.with_extension("ts"));
        keys.push(base.with_extension("tsx"));
    }

    keys
}

fn extension_variants(path: &Path) -> Vec<PathBuf> {
    let mut variants = vec![path.to_path_buf()];
    let normalized = normalize_path(path);

    if normalized.ends_with(".ts") {
        variants.push(PathBuf::from(normalized.trim_end_matches(".ts")));
    } else if normalized.ends_with(".tsx") {
        variants.push(PathBuf::from(normalized.trim_end_matches(".tsx")));
    } else {
        variants.push(PathBuf::from(format!("{normalized}.ts")));
        variants.push(PathBuf::from(format!("{normalized}.tsx")));
    }

    variants
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::{ArchitecturalRule, ArchitectureConfig, LayerDefinition};

    fn test_validator() -> Validator {
        let config = ArchitectureConfig {
            layers: HashMap::from([
                (
                    "presentation".into(),
                    LayerDefinition {
                        path_patterns: vec!["**/ui/**".into()],
                        allowed_dependencies: None,
                        enforce_interface_boundary: false,
                        interface_port_layer: None,
                        port_path_patterns: vec![],
                    },
                ),
                (
                    "domain".into(),
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
            ]),
            rules: vec![],
        };
        Validator::new(config.compile().expect("compile"))
    }

    #[test]
    fn revalidates_immediate_dependents_on_change() {
        let cache = ArchitectureCache::new(test_validator());

        cache
            .process_change(
                Path::new("src/domain/User.ts"),
                r#"export class User {}"#,
            )
            .expect("seed domain");

        cache
            .process_change(
                Path::new("src/ui/Home.tsx"),
                r#"import { User } from "../domain/User";"#,
            )
            .expect("seed ui");

        assert_eq!(cache.layout_count(), 2);
        assert!(cache.importers.contains_key(&PathBuf::from("src/domain/User.ts")));

        let update = cache
            .process_change(
                Path::new("src/domain/User.ts"),
                r#"import { Widget } from "../ui/Widget"; export class User {}"#,
            )
            .expect("update domain");

        assert_eq!(update.files_reparsed, 1);
        assert_eq!(update.files_revalidated, 2);
    }
}
