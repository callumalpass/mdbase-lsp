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
/// Pre-populates required fields that lack defaults/generated strategies
/// with type-appropriate placeholders so they appear in the created file.
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

    let mut input = args.get(0).cloned().unwrap_or_else(|| serde_json::json!({}));

    // Pre-populate required fields that have no default and no generated strategy
    if let Some(type_name) = input.get("type").and_then(|v| v.as_str()) {
        if let Some(type_def) = collection.types.get(&type_name.to_lowercase()) {
            let fm = input
                .get("frontmatter")
                .or_else(|| input.get("fields"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let mut fm_obj = fm.as_object().cloned().unwrap_or_default();

            for (field_name, field_def) in &type_def.fields {
                if field_def.required
                    && field_def.default.is_none()
                    && field_def.generated.is_none()
                    && !fm_obj.contains_key(field_name)
                {
                    fm_obj.insert(field_name.clone(), placeholder_for_type(&field_def.field_type));
                }
            }

            input.as_object_mut().unwrap().insert(
                "frontmatter".to_string(),
                serde_json::Value::Object(fm_obj),
            );
        }
    }

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

/// Return a sensible placeholder value for a required field based on its type.
fn placeholder_for_type(field_type: &str) -> serde_json::Value {
    match field_type {
        "list" => serde_json::json!([]),
        "object" => serde_json::json!({}),
        "boolean" => serde_json::json!(false),
        "integer" => serde_json::json!(0),
        "number" => serde_json::json!(0),
        _ => serde_json::json!(""),
    }
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
