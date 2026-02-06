use std::sync::atomic::Ordering;
use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{info, warn};

use crate::state::{BackendState, DocumentState};

pub struct MdbaseLanguageServer {
    client: Client,
    state: Arc<BackendState>,
}

impl MdbaseLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(BackendState::new()),
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
                    info!(root = %path.display(), "collection root from workspace folder");
                    let mut root = self.state.collection_root.write().unwrap();
                    *root = Some(path);
                }
            }
        } else if let Some(root_uri) = &params.root_uri {
            if let Ok(path) = root_uri.to_file_path() {
                info!(root = %path.display(), "collection root from root_uri");
                let mut root = self.state.collection_root.write().unwrap();
                *root = Some(path);
            }
        } else {
            warn!("no workspace folder or root_uri provided");
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::INCREMENTAL),
                        will_save_wait_until: Some(true),
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(false),
                        })),
                        ..Default::default()
                    },
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        ":".into(), // after field name
                        "[".into(), // wikilink start
                        "(".into(), // markdown link ](
                        "#".into(), // tag
                    ]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: Default::default(),
                }),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        "mdbase.createFile".to_string(),
                        "mdbase.typeInfo".to_string(),
                        "mdbase.validateCollection".to_string(),
                        "mdbase.queryCollection".to_string(),
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

        // Build the file index in the background
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            if let Some(collection) = state.get_collection() {
                state.file_index.rebuild(&collection);
            }
        });
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        self.state.documents.insert(
            uri.clone(),
            DocumentState::new(ropey::Rope::from_str(&text)),
        );
        if let Some(collection) = self.state.get_collection() {
            if let Some(rel_path) = crate::collection_utils::rel_path_from_uri(&collection, &uri) {
                self.state
                    .file_index
                    .upsert_from_text(&collection, rel_path, &text);
            }
        }
        // Immediate diagnostics on open
        crate::diagnostics::publish(&self.client, &self.state, &uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // Apply incremental changes to the rope
        if let Some(mut doc) = self.state.documents.get_mut(&uri) {
            for change in params.content_changes {
                if let Some(range) = change.range {
                    let start = offset_from_position(&doc.rope, range.start);
                    let end = offset_from_position(&doc.rope, range.end);
                    doc.rope.remove(start..end);
                    doc.rope.insert(start, &change.text);
                } else {
                    doc.rope = ropey::Rope::from_str(&change.text);
                }
            }
            doc.invalidate_frontmatter();
        }

        // Debounced diagnostics: bump generation, spawn delayed task
        let gen = self.state.bump_generation(&uri);
        let counter = self.state.generation_counter(&uri);
        let client = self.client.clone();
        let state = Arc::clone(&self.state);
        let uri_clone = uri.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            if counter.load(Ordering::SeqCst) == gen {
                crate::diagnostics::publish(&client, &state, &uri_clone).await;
            }
        });
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = &params.text_document.uri;
        if let Some(collection) = self.state.get_collection() {
            if let Some(rel_path) = crate::collection_utils::rel_path_from_uri(&collection, uri) {
                self.state.file_index.remove_path(&rel_path);
            }
        }
        self.state.documents.remove(uri);
        self.state.diagnostics_generation.remove(uri);
    }

    async fn will_save_wait_until(
        &self,
        params: WillSaveTextDocumentParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        if !uri.path().ends_with(".md") {
            return Ok(None);
        }

        let Some(collection) = self.state.get_collection() else {
            return Ok(None);
        };
        let Some(text) = self.state.document_text(uri) else {
            return Ok(None);
        };

        let parsed = crate::text::parse_frontmatter(&text);
        if parsed.parse_error || parsed.mapping_error {
            return Ok(None);
        }

        let rel_path = uri.to_file_path().ok().and_then(|p: std::path::PathBuf| {
            p.strip_prefix(&collection.root)
                .ok()
                .map(|r: &std::path::Path| r.to_string_lossy().to_string().replace('\\', "/"))
        });
        let type_names = collection.determine_types_for_path(&parsed.json, rel_path.as_deref());

        // Collect unique NowOnWrite field names across all matched types
        let mut now_fields = Vec::new();
        for type_name in &type_names {
            if let Some(type_def) = collection.types.get(type_name) {
                for (field_name, field_def) in &type_def.fields {
                    if matches!(
                        field_def.generated,
                        Some(mdbase::types::schema::GeneratedStrategy::NowOnWrite)
                    ) && !now_fields.contains(field_name)
                    {
                        now_fields.push(field_name.clone());
                    }
                }
            }
        }

        if now_fields.is_empty() {
            return Ok(None);
        }

        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let bounds = crate::text::frontmatter_bounds(&text);

        let mut edits = Vec::new();
        for field_name in &now_fields {
            if let Some(edit) = make_now_on_write_edit(&text, bounds, field_name, &now) {
                edits.push(edit);
            }
        }

        Ok(if edits.is_empty() { None } else { Some(edits) })
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        if !uri.path().ends_with(".md") {
            return;
        }

        // Re-sync the in-memory rope from the file on disk
        if let Ok(file_path) = uri.to_file_path() {
            if let Ok(new_text) = std::fs::read_to_string(&file_path) {
                self.state.documents.insert(
                    uri.clone(),
                    DocumentState::new(ropey::Rope::from_str(&new_text)),
                );
            }
        }

        // Reload collection when a type definition changes.
        if uri.path().contains("/_types/") {
            info!("type file changed, reloading collection");
            self.state.invalidate_collection();
        }

        // Cancel any pending debounced diagnostics from did_change
        self.state.bump_generation(&uri);

        // Immediate diagnostics on save
        crate::diagnostics::publish(&self.client, &self.state, &uri).await;

        // Incrementally update file index for this saved file.
        if let Some(collection) = self.state.get_collection() {
            if let Some(rel_path) = crate::collection_utils::rel_path_from_uri(&collection, &uri) {
                if let Some(text) = self.state.document_text(&uri) {
                    self.state
                        .file_index
                        .upsert_from_text(&collection, rel_path, &text);
                }
            }
        }
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

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        Ok(crate::references::provide(&self.state, params))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        Ok(crate::code_actions::provide(&self.state, params))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        Ok(crate::references::prepare_rename(&self.state, params))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        Ok(crate::references::rename(&self.state, params))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        Ok(crate::symbols::workspace_symbols(
            &self.state,
            &params.query,
        ))
    }

    async fn document_link(&self, params: DocumentLinkParams) -> Result<Option<Vec<DocumentLink>>> {
        let uri = &params.text_document.uri;
        Ok(crate::document_links::provide(&self.state, uri))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<serde_json::Value>> {
        crate::commands::execute(&self.client, &self.state, &params).await
    }
}

/// Convert an LSP Position to a byte offset in a Rope.
fn offset_from_position(rope: &ropey::Rope, pos: Position) -> usize {
    let line_start = rope.line_to_char(pos.line as usize);
    line_start + pos.character as usize
}

/// Build a TextEdit that sets a NowOnWrite field value in YAML frontmatter.
/// If the field already exists, replace its value; otherwise insert before closing `---`.
fn make_now_on_write_edit(
    text: &str,
    bounds: Option<(usize, usize)>,
    field_name: &str,
    value: &str,
) -> Option<TextEdit> {
    let (fm_start, fm_end) = bounds?;

    // Look for an existing line like `fieldName: ...`
    for (line_idx, line) in text.lines().enumerate() {
        if line_idx < fm_start || line_idx > fm_end {
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(field_name) {
            if rest.starts_with(':') {
                let new_line = format!("{}: {}", field_name, value);
                return Some(TextEdit {
                    range: Range {
                        start: Position::new(line_idx as u32, 0),
                        end: Position::new(line_idx as u32, line.len() as u32),
                    },
                    new_text: new_line,
                });
            }
        }
    }

    // Field not present — insert a new line before the closing `---`
    let closing_line = (fm_end + 1) as u32;
    Some(TextEdit {
        range: Range {
            start: Position::new(closing_line, 0),
            end: Position::new(closing_line, 0),
        },
        new_text: format!("{}: {}\n", field_name, value),
    })
}
