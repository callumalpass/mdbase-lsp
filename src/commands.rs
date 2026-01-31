use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::Client;

use crate::state::BackendState;

/// Execute a custom workspace command.
pub async fn execute(
    client: &Client,
    state: &BackendState,
    params: &ExecuteCommandParams,
) -> Result<Option<serde_json::Value>> {
    let args: &[serde_json::Value] = &params.arguments;
    match params.command.as_str() {
        "mdbase.createFile" => create_file(client, state, args).await,
        "mdbase.validateCollection" => validate_collection(client, state).await,
        _ => {
            client
                .log_message(
                    MessageType::WARNING,
                    format!("Unknown command: {}", params.command),
                )
                .await;
            Ok(None)
        }
    }
}

/// Create a new file scaffolded from a type definition.
///
/// TODO: Implement:
/// 1. Read type name from arguments
/// 2. Load type schema from mdbase-rs
/// 3. Generate frontmatter with required fields, defaults, generated values
/// 4. Create the file and open it in the editor
async fn create_file(
    client: &Client,
    state: &BackendState,
    args: &[serde_json::Value],
) -> Result<Option<serde_json::Value>> {
    let collection = match state.get_collection() {
        Some(c) => c,
        None => {
            client.log_message(MessageType::ERROR, "mdbase collection not loaded").await;
            return Ok(None);
        }
    };

    let input = args.get(0).cloned().unwrap_or_else(|| serde_json::json!({}));
    let result = collection.create(&input);
    if let Some(path) = result.get("path").and_then(|v| v.as_str()) {
        let full_path = collection.root.join(path);
        if let Ok(uri) = Url::from_file_path(full_path) {
            let _ = client.show_document(ShowDocumentParams {
                uri,
                external: Some(false),
                take_focus: Some(true),
                selection: None,
            }).await;
        }
    }
    Ok(Some(result))
}

/// Validate the entire collection and report results.
///
/// TODO: Implement:
/// 1. Scan all markdown files in collection root
/// 2. Validate each against its matched types
/// 3. Return summary as JSON
async fn validate_collection(
    client: &Client,
    state: &BackendState,
) -> Result<Option<serde_json::Value>> {
    let collection = match state.get_collection() {
        Some(c) => c,
        None => {
            client.log_message(MessageType::ERROR, "mdbase collection not loaded").await;
            return Ok(None);
        }
    };

    let result = collection.validate_op(&serde_json::json!({}));
    Ok(Some(result))
}
