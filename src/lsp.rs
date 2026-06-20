use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use architectural_drift_detector::{
    AddResult, ArchitectureConfig, ArchitectureCache, Validator, Violation,
    ViolationSeverity,
};

pub async fn run_server(config_path: PathBuf) -> AddResult<()> {
    let config = Arc::new(ArchitectureConfig::from_yaml_file(&config_path)?.compile()?);
    let validator = Validator::with_shared(Arc::clone(&config));
    let cache = Arc::new(ArchitectureCache::with_shared_validator(Arc::new(validator)));

    let (service, socket) = LspService::new(move |client| AddLanguageServer {
        client,
        cache: Arc::clone(&cache),
        documents: Arc::new(RwLock::new(HashMap::new())),
    });

    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;

    Ok(())
}

struct AddLanguageServer {
    client: Client,
    cache: Arc<ArchitectureCache>,
    documents: Arc<RwLock<HashMap<Url, String>>>,
}

#[tower_lsp::async_trait]
impl LanguageServer for AddLanguageServer {
    async fn initialize(&self, _: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "architectural-drift-detector".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "ADD language server initialized (incremental cache enabled)")
            .await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.handle_document_change(params.text_document.uri, params.text_document.text)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let content = params
            .content_changes
            .into_iter()
            .last()
            .map(|change| change.text)
            .unwrap_or_default();

        self.handle_document_change(params.text_document.uri, content)
            .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(path) = uri_to_path(&uri) {
            self.cache.remove_file(&path);
        }
        self.documents.write().await.remove(&uri);
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }
}

impl AddLanguageServer {
    async fn handle_document_change(&self, uri: Url, text: String) {
        let path = match uri_to_path(&uri) {
            Some(path) => path,
            None => return,
        };

        if !is_supported_source(&path) {
            return;
        }

        self.documents.write().await.insert(uri.clone(), text.clone());

        let cache = Arc::clone(&self.cache);
        let client = self.client.clone();

        tokio::spawn(async move {
            let analysis =
                tokio::task::spawn_blocking(move || cache.process_change(&path, &text)).await;

            match analysis {
                Ok(Ok(update)) => {
                    for (affected_path, violations) in update.diagnostics {
                        let affected_uri = match Url::from_file_path(&affected_path) {
                            Ok(uri) => uri,
                            Err(_) => continue,
                        };
                        let diagnostics = violations_to_diagnostics(&affected_uri, &violations);
                        client
                            .publish_diagnostics(affected_uri, diagnostics, None)
                            .await;
                    }
                }
                Ok(Err(err)) => {
                    let _ = client
                        .log_message(MessageType::ERROR, format!("analysis failed: {err}"))
                        .await;
                }
                Err(err) => {
                    let _ = client
                        .log_message(MessageType::ERROR, format!("task join failed: {err}"))
                        .await;
                }
            }
        });
    }
}

fn uri_to_path(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path().ok()
}

fn is_supported_source(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ts" | "tsx"))
}

fn violations_to_diagnostics(uri: &Url, violations: &[Violation]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::with_capacity(violations.len());

    for violation in violations {
        let line = violation.line.saturating_sub(1) as u32;
        let character = violation.column.saturating_sub(1) as u32;
        let position = Position { line, character };

        let mut related = Vec::new();
        for (index, step) in violation.remediation.iter().enumerate() {
            related.push(DiagnosticRelatedInformation {
                location: Location {
                    uri: uri.clone(),
                    range: Range {
                        start: position,
                        end: position,
                    },
                },
                message: format!("{}. {step}", index + 1),
            });
        }

        let severity = match violation.severity {
            ViolationSeverity::High => DiagnosticSeverity::ERROR,
            ViolationSeverity::Normal => DiagnosticSeverity::WARNING,
        };

        diagnostics.push(Diagnostic {
            range: Range {
                start: position,
                end: position,
            },
            severity: Some(severity),
            code: Some(NumberOrString::String(violation.rule.clone())),
            code_description: None,
            source: Some("architectural-drift-detector".into()),
            message: format!(
                "{} (import `{}`)",
                violation.message, violation.import_path
            ),
            related_information: if related.is_empty() {
                None
            } else {
                Some(related)
            },
            tags: if violation.is_high_severity() {
                Some(vec![DiagnosticTag::UNNECESSARY])
            } else {
                None
            },
            data: None,
        });
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn maps_violation_positions_to_zero_based_lsp() {
        let uri = Url::parse("file:///test.ts").expect("test url");
        let diagnostics = violations_to_diagnostics(
            &uri,
            &[Violation {
                rule: "test-rule".into(),
                file: PathBuf::from("src/ui/App.tsx"),
                line: 3,
                column: 5,
                import_path: "../data/repo".into(),
                message: "not allowed".into(),
                remediation: vec!["fix it".into()],
                severity: ViolationSeverity::Normal,
            }],
        );

        assert_eq!(diagnostics[0].range.start.line, 2);
        assert_eq!(diagnostics[0].range.start.character, 4);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::WARNING));
    }

    #[test]
    fn maps_high_severity_to_error() {
        let uri = Url::parse("file:///test.ts").expect("test url");
        let diagnostics = violations_to_diagnostics(
            &uri,
            &[Violation {
                rule: "interface-bypass-violation".into(),
                file: PathBuf::from("src/ui/App.tsx"),
                line: 1,
                column: 1,
                import_path: "../data/repo".into(),
                message: "bypass".into(),
                remediation: vec![],
                severity: ViolationSeverity::High,
            }],
        );

        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
    }
}
