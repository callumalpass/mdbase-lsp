use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::Client;

use crate::state::BackendState;

/// Execute a custom workspace command.
pub async fn execute(
    client: &Client,
    _state: &BackendState,
    params: &ExecuteCommandParams,
) -> Result<Option<serde_json::Value>> {
    match params.command.as_str() {
        "mdbase.createFile" => create_file(client, _state, &params.arguments).await,
        "mdbase.validateCollection" => validate_collection(client, _state).await,
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
    _client: &Client,
    _state: &BackendState,
    _args: &[serde_json::Value],
) -> Result<Option<serde_json::Value>> {
    // Stub
    Ok(None)
}

/// Validate the entire collection and report results.
///
/// TODO: Implement:
/// 1. Scan all markdown files in collection root
/// 2. Validate each against its matched types
/// 3. Return summary as JSON
async fn validate_collection(
    _client: &Client,
    _state: &BackendState,
) -> Result<Option<serde_json::Value>> {
    // Stub
    Ok(None)
}
