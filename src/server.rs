use std::collections::HashSet;
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
        let mut roots = Vec::new();
        if let Some(folders) = &params.workspace_folders {
            for folder in folders {
                if let Ok(path) = folder.uri.to_file_path() {
                    info!(root = %path.display(), "workspace folder");
                    roots.push(path);
                }
            }
        } else if let Some(root_uri) = &params.root_uri {
            if let Ok(path) = root_uri.to_file_path() {
                info!(root = %path.display(), "collection root from root_uri");
                roots.push(path);
            }
        } else {
            warn!("no workspace folder or root_uri provided");
        }
        self.state.set_workspace_roots(roots);

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
                        "-".into(), // list item values in frontmatter
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
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: None,
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

        // Preload contexts/indexes for configured workspace roots.
        for root in self.state.workspace_roots_snapshot() {
            let state = Arc::clone(&self.state);
            tokio::task::spawn_blocking(move || {
                let _ = state.context_for_root(&root);
            });
        }

        if let Err(err) = register_file_watchers(&self.client).await {
            warn!(error = %err, "failed to register didChangeWatchedFiles");
        }
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
        upsert_or_remove_index_entry(&self.state, &uri, &text);
        // Immediate diagnostics on open
        crate::diagnostics::publish(&self.client, &self.state, &uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let mut latest_text: Option<String> = None;
        // Apply incremental changes to the rope
        {
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
                latest_text = Some(doc.rope.to_string());
            }
        }

        if let Some(text) = latest_text.as_deref() {
            upsert_or_remove_index_entry(&self.state, &uri, text);
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
        if let Some((ctx, rel_path)) = self.state.context_and_rel_path_for_uri(uri) {
            if crate::collection_utils::should_index_rel_path(&ctx.collection, &rel_path) {
                let abs_path = ctx.collection.root().join(&rel_path);
                if let Ok(on_disk_text) = std::fs::read_to_string(&abs_path) {
                    ctx.file_index
                        .upsert_from_text(&ctx.collection, rel_path, &on_disk_text);
                } else {
                    ctx.file_index.remove_path(&rel_path);
                }
            } else {
                ctx.file_index.remove_path(&rel_path);
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

        let Some((ctx, rel_path)) = self.state.context_and_rel_path_for_uri(uri) else {
            return Ok(None);
        };
        let Some(text) = self.state.document_text(uri) else {
            return Ok(None);
        };

        let parsed = crate::text::parse_frontmatter(&text);
        if parsed.parse_error || parsed.mapping_error {
            return Ok(None);
        }

        let type_names = ctx
            .collection
            .determine_types_for_path(&parsed.json, Some(&rel_path));

        // Collect unique NowOnWrite field names across all matched types
        let mut now_fields = Vec::new();
        for type_name in &type_names {
            if let Some(type_def) = ctx.collection.types().get(type_name) {
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

        // Reload collection when a type or data contract definition changes.
        let changed_control_file = self
            .state
            .context_and_rel_path_for_uri(&uri)
            .is_some_and(|(ctx, rel_path)| is_registry_control_file(&ctx.collection, &rel_path));
        if changed_control_file {
            info!("type or data contract file changed, reloading collection");
            self.state.invalidate_for_uri(&uri);
        }

        // Cancel any pending debounced diagnostics from did_change
        self.state.bump_generation(&uri);

        // Immediate diagnostics on save
        crate::diagnostics::publish(&self.client, &self.state, &uri).await;

        // Incrementally update file index for this saved file.
        if let Some(text) = self.state.document_text(&uri) {
            upsert_or_remove_index_entry(&self.state, &uri, &text);
        }
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        let mut roots = self.state.workspace_roots_snapshot();

        for removed in &params.event.removed {
            if let Ok(path) = removed.uri.to_file_path() {
                roots.retain(|r| r != &path);
                self.state.invalidate_root(&path);
            }
        }

        for added in &params.event.added {
            if let Ok(path) = added.uri.to_file_path() {
                if !roots.contains(&path) {
                    roots.push(path.clone());
                }
            }
        }

        self.state.set_workspace_roots(roots.clone());
        for root in roots {
            let state = Arc::clone(&self.state);
            tokio::task::spawn_blocking(move || {
                let _ = state.context_for_root(&root);
            });
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let mut roots_to_invalidate = HashSet::new();

        for change in params.changes {
            let Ok(path) = change.uri.to_file_path() else {
                continue;
            };

            let is_config = path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("mdbase.yaml"))
                .unwrap_or(false);
            let is_registry_file = self
                .state
                .root_for_path(&path)
                .and_then(|root| {
                    let relative = path
                        .strip_prefix(&root)
                        .ok()?
                        .to_string_lossy()
                        .replace('\\', "/");
                    let context = self.state.context_for_root(&root)?;
                    Some(is_registry_control_file(&context.collection, &relative))
                })
                .unwrap_or_else(|| {
                    path_has_component(&path, "_types") || path_has_component(&path, "_contracts")
                });

            if is_config {
                if let Some(parent) = path.parent() {
                    roots_to_invalidate.insert(parent.to_path_buf());
                }
                continue;
            }
            if is_registry_file {
                if let Some(root) = self.state.root_for_path(&path) {
                    roots_to_invalidate.insert(root);
                }
                continue;
            }

            let Ok(uri) = Url::from_file_path(&path) else {
                continue;
            };

            let Some((ctx, rel_path)) = self.state.context_and_rel_path_for_uri(&uri) else {
                continue;
            };

            if !crate::collection_utils::should_index_rel_path(&ctx.collection, &rel_path) {
                ctx.file_index.remove_path(&rel_path);
                continue;
            }

            match change.typ {
                FileChangeType::DELETED => {
                    ctx.file_index.remove_path(&rel_path);
                }
                FileChangeType::CHANGED | FileChangeType::CREATED => {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        ctx.file_index
                            .upsert_from_text(&ctx.collection, rel_path, &text);
                    }
                }
                _ => {}
            }
        }

        for root in roots_to_invalidate {
            self.state.invalidate_root(&root);
            let state = Arc::clone(&self.state);
            tokio::task::spawn_blocking(move || {
                let _ = state.context_for_root(&root);
            });
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

async fn register_file_watchers(client: &Client) -> Result<()> {
    let options = DidChangeWatchedFilesRegistrationOptions {
        watchers: vec![
            FileSystemWatcher {
                glob_pattern: GlobPattern::String("**/mdbase.yaml".to_string()),
                kind: None,
            },
            FileSystemWatcher {
                glob_pattern: GlobPattern::String("**/_types/**/*".to_string()),
                kind: None,
            },
            FileSystemWatcher {
                glob_pattern: GlobPattern::String("**/_contracts/**/*".to_string()),
                kind: None,
            },
            FileSystemWatcher {
                glob_pattern: GlobPattern::String("**/*.md".to_string()),
                kind: None,
            },
        ],
    };

    client
        .register_capability(vec![Registration {
            id: "mdbase-watchers".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: Some(serde_json::to_value(options).unwrap_or_default()),
        }])
        .await
}

fn upsert_or_remove_index_entry(state: &BackendState, uri: &Url, text: &str) {
    let Some((ctx, rel_path)) = state.context_and_rel_path_for_uri(uri) else {
        return;
    };
    if crate::collection_utils::should_index_rel_path(&ctx.collection, &rel_path) {
        ctx.file_index
            .upsert_from_text(&ctx.collection, rel_path, text);
    } else {
        ctx.file_index.remove_path(&rel_path);
    }
}

fn path_has_component(path: &std::path::Path, component: &str) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| s.eq_ignore_ascii_case(component))
            .unwrap_or(false)
    })
}

fn is_registry_control_file(collection: &mdbase::Collection, relative_path: &str) -> bool {
    [
        collection.settings().types_folder.as_str(),
        collection.settings().contracts_folder.as_str(),
    ]
    .iter()
    .any(|folder| relative_path == *folder || relative_path.starts_with(&format!("{folder}/")))
}

/// Convert an LSP `Position` (UTF-16 columns) to a rope char offset.
fn offset_from_position(rope: &ropey::Rope, pos: Position) -> usize {
    let max_line = rope.len_lines().saturating_sub(1);
    let line_idx = (pos.line as usize).min(max_line);
    let line_start = rope.line_to_char(line_idx);
    let line = rope.line(line_idx);
    line_start + char_offset_from_utf16_col(line, pos.character as usize)
}

fn char_offset_from_utf16_col(line: ropey::RopeSlice<'_>, utf16_col: usize) -> usize {
    let mut utf16_count = 0usize;
    for (char_idx, ch) in line.chars().enumerate() {
        if utf16_count == utf16_col {
            return char_idx;
        }
        utf16_count += ch.len_utf16();
        if utf16_count > utf16_col {
            // Invalid mid-surrogate position: clamp forward to avoid splitting.
            return char_idx + 1;
        }
    }
    line.len_chars()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_from_position_handles_utf16_columns() {
        let rope = ropey::Rope::from_str("a😀b\n");
        assert_eq!(offset_from_position(&rope, Position::new(0, 0)), 0);
        assert_eq!(offset_from_position(&rope, Position::new(0, 1)), 1);
        assert_eq!(offset_from_position(&rope, Position::new(0, 3)), 2);
        assert_eq!(offset_from_position(&rope, Position::new(0, 4)), 3);
    }

    #[test]
    fn offset_from_position_clamps_line_out_of_bounds() {
        let rope = ropey::Rope::from_str("abc");
        assert_eq!(offset_from_position(&rope, Position::new(99, 2)), 2);
    }
}
