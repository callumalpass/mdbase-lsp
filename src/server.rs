use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::state::BackendState;

pub struct MdbaseLanguageServer {
    client: Client,
    state: BackendState,
}

impl MdbaseLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: BackendState::new(),
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for MdbaseLanguageServer {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Determine collection root from workspace folders
        if let Some(folders) = &params.workspace_folders {
            if let Some(folder) = folders.first() {
                if let Ok(path) = folder.uri.to_file_path() {
                    let mut root = self.state.collection_root.write().unwrap();
                    *root = Some(path);
                }
            }
        } else if let Some(root_uri) = &params.root_uri {
            if let Ok(path) = root_uri.to_file_path() {
                let mut root = self.state.collection_root.write().unwrap();
                *root = Some(path);
            }
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        ":".into(),  // after field name
                        "[".into(),  // wikilink start
                        "#".into(),  // tag
                    ]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        "mdbase.createFile".into(),
                        "mdbase.validateCollection".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "mdbase LSP initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        self.state.documents.insert(uri.clone(), ropey::Rope::from_str(&text));
        crate::diagnostics::publish(&self.client, &self.state, &uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // For incremental sync, apply changes to the rope
        if let Some(mut doc) = self.state.documents.get_mut(&uri) {
            for change in params.content_changes {
                if let Some(range) = change.range {
                    let start = offset_from_position(&doc, range.start);
                    let end = offset_from_position(&doc, range.end);
                    doc.remove(start..end);
                    doc.insert(start, &change.text);
                } else {
                    *doc = ropey::Rope::from_str(&change.text);
                }
            }
        }
        crate::diagnostics::publish(&self.client, &self.state, &uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.state.documents.remove(&params.text_document.uri);
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        Ok(crate::completions::provide(&self.state, uri, pos))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(crate::hover::provide(&self.state, uri, pos))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(crate::goto::definition(&self.state, uri, pos))
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<serde_json::Value>> {
        crate::commands::execute(&self.client, &self.state, &params).await
    }
}

/// Convert an LSP Position to a byte offset in a Rope.
fn offset_from_position(rope: &ropey::Rope, pos: Position) -> usize {
    let line_start = rope.line_to_char(pos.line as usize);
    line_start + pos.character as usize
}
