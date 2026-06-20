use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::error::{AddError, AddResult};

/// A resolved import extracted from source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRef {
    pub path: String,
    pub line: usize,
    pub column: usize,
}

/// Structural facts about a single source file.
#[derive(Debug, Clone, Default)]
pub struct FileLayout {
    pub file_path: PathBuf,
    pub imports: Vec<ImportRef>,
}

/// Aggregated structural layout for one or more files.
#[derive(Debug, Clone, Default)]
pub struct StructuralLayout {
    pub files: Vec<FileLayout>,
}

/// Trait abstraction for AST extraction backends (TypeScript today, more languages later).
pub trait AstExtractor {
    fn extract_file(&mut self, path: &Path, source: &str) -> AddResult<FileLayout>;
}

/// Tree-sitter powered extractor for TypeScript and TSX.
pub struct AstEngine {
    parser: Parser,
    import_query: Query,
    query_cursor: QueryCursor,
    source_capture: u32,
}

const IMPORT_QUERY: &str = r#"
(import_statement
  source: (string) @import.source) @import.statement

(export_statement
  source: (string) @import.source) @export.reexport

(import_call
  source: (string) @import.source) @import.dynamic

(call_expression
  function: (import)
  arguments: (arguments (string) @import.source)) @import.dynamic_call

(call_expression
  function: (identifier) @import.require_fn
  arguments: (arguments (string) @import.source)
  (#eq? @import.require_fn "require"))
"#;

impl AstEngine {
    pub fn typescript() -> AddResult<Self> {
        Self::with_language(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
    }

    pub fn tsx() -> AddResult<Self> {
        Self::with_language(tree_sitter_typescript::LANGUAGE_TSX.into())
    }

    pub fn for_path(path: &Path) -> AddResult<Self> {
        if is_tsx_path(path) {
            Self::tsx()
        } else {
            Self::typescript()
        }
    }

    fn with_language(language: tree_sitter::Language) -> AddResult<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .map_err(|_| AddError::LanguageSetup("unsupported tree-sitter language".into()))?;

        let import_query = Query::new(&language, IMPORT_QUERY)
            .map_err(|err| AddError::QueryCompile(err.to_string()))?;

        let source_capture = import_query
            .capture_index_for_name("import.source")
            .ok_or_else(|| AddError::QueryCompile("missing capture `import.source`".into()))?;

        Ok(Self {
            parser,
            import_query,
            query_cursor: QueryCursor::new(),
            source_capture,
        })
    }

    fn collect_imports(&mut self, root: Node<'_>, source: &str) -> AddResult<Vec<ImportRef>> {
        let bytes = source.as_bytes();
        let mut imports = Vec::new();
        let mut seen_nodes = HashSet::new();
        let mut matches = self.query_cursor.matches(&self.import_query, root, bytes);

        while let Some(query_match) = matches.next() {
            for capture in query_match.captures {
                if capture.index != self.source_capture {
                    continue;
                }

                let node = capture.node;
                if !seen_nodes.insert(node.start_byte()) {
                    continue;
                }

                let raw = node_text(node, bytes)?;
                let path = strip_quotes(raw);

                if path.is_empty() {
                    continue;
                }

                let start = node.start_position();
                imports.push(ImportRef {
                    path: path.to_owned(),
                    line: start.row + 1,
                    column: start.column + 1,
                });
            }
        }

        imports.shrink_to_fit();
        Ok(imports)
    }
}

impl AstExtractor for AstEngine {
    fn extract_file(&mut self, path: &Path, source: &str) -> AddResult<FileLayout> {
        self.parse(path, source)
    }
}

impl AstEngine {
    /// Parse source and extract structural layout (benchmark and hot-path entry point).
    pub fn parse(&mut self, path: &Path, source: &str) -> AddResult<FileLayout> {
        let tree = self
            .parser
            .parse(source, None)
            .ok_or_else(|| AddError::ParseFailed {
                path: path.to_path_buf(),
            })?;

        let imports = self.collect_imports(tree.root_node(), source)?;

        Ok(FileLayout {
            file_path: path.to_path_buf(),
            imports,
        })
    }
}

fn node_text<'a>(node: Node<'a>, source: &'a [u8]) -> AddResult<&'a str> {
    node.utf8_text(source)
        .map_err(|_| AddError::InvalidUtf8 {
            offset: node.start_byte(),
        })
}

fn strip_quotes(raw: &str) -> &str {
    raw.trim()
        .trim_start_matches(['\'', '"'])
        .trim_end_matches(['\'', '"'])
}

fn is_tsx_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tsx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_static_and_require_imports() {
        let source = r#"
import React from "react";
import { ApiClient } from "../data/infrastructure/client";
const cfg = require("./config/local");
"#;

        let mut engine = AstEngine::typescript().expect("engine");
        let layout = engine
            .extract_file(Path::new("src/ui/App.tsx"), source)
            .expect("extract");

        assert_eq!(layout.imports.len(), 3);
        assert!(layout.imports.iter().any(|i| i.path.contains("infrastructure")));
    }

    #[test]
    fn extracts_dynamic_and_export_from_imports() {
        let source = r#"
export { Button } from "./components/Button";
export * from "../data/infrastructure/models";
const mod = await import("./lazy/Panel");
import("./path/to/module");
"#;

        let mut engine = AstEngine::typescript().expect("engine");
        let layout = engine
            .extract_file(Path::new("src/ui/App.tsx"), source)
            .expect("extract");

        let paths: Vec<_> = layout.imports.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"./components/Button"));
        assert!(paths.contains(&"../data/infrastructure/models"));
        assert!(paths.contains(&"./lazy/Panel"));
        assert!(paths.contains(&"./path/to/module"));
    }
}
