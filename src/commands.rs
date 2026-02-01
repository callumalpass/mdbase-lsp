use mdbase::types::schema::GeneratedStrategy;
use mdbase::Collection;
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
/// Pre-generates values for fields with generated strategies, derives the
/// file path from the inherited filename_pattern when no path is provided,
/// and populates required fields that lack defaults with placeholders.
async fn create_file(
    client: &Client,
    state: &BackendState,
    args: &[serde_json::Value],
) -> Result<Option<serde_json::Value>> {
    let collection = match state.get_collection() {
        Some(c) => c,
        None => {
            client
                .log_message(MessageType::ERROR, "mdbase collection not loaded")
                .await;
            return Ok(None);
        }
    };

    let mut input = args
        .get(0)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    if let Some(type_name) = input.get("type").and_then(|v| v.as_str()) {
        let tn_lower = type_name.to_lowercase();
        if let Some(type_def) = collection.types.get(&tn_lower) {
            let fm = input
                .get("frontmatter")
                .or_else(|| input.get("fields"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let mut fm_obj = fm.as_object().cloned().unwrap_or_default();

            // Pre-generate values for fields with generated strategies.
            // Walk the extends chain so that overridden fields (e.g. person
            // redefining zettelid without `generated`) still pick up the
            // ancestor's strategy.
            for (field_name, _) in &type_def.fields {
                if fm_obj.contains_key(field_name) {
                    continue;
                }
                if let Some(value) =
                    generate_field_value(&collection, &tn_lower, field_name)
                {
                    fm_obj.insert(field_name.clone(), value);
                }
            }

            // Derive path from filename_pattern if none provided.
            let has_path = input
                .get("path")
                .and_then(|v| v.as_str())
                .map_or(false, |s| !s.is_empty());
            if !has_path {
                if let Some(pattern) = find_filename_pattern(&collection, &tn_lower) {
                    if let Some(path) =
                        derive_path_from_pattern(&pattern, &fm_obj)
                    {
                        input
                            .as_object_mut()
                            .unwrap()
                            .insert("path".to_string(), serde_json::json!(path));
                    }
                }
            }

            // Fill remaining required fields that have no default and no
            // generated strategy with type-appropriate placeholders.
            for (field_name, field_def) in &type_def.fields {
                if field_def.required
                    && field_def.default.is_none()
                    && field_def.generated.is_none()
                    && !fm_obj.contains_key(field_name)
                {
                    fm_obj.insert(
                        field_name.clone(),
                        placeholder_for_type(&field_def.field_type),
                    );
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
            let _ = client
                .show_document(ShowDocumentParams {
                    uri,
                    external: Some(false),
                    take_focus: Some(true),
                    selection: None,
                })
                .await;
        }
    }
    Ok(Some(result))
}

/// Walk the extends chain to find the first `filename_pattern`.
fn find_filename_pattern(collection: &Collection, type_name: &str) -> Option<String> {
    let mut current = Some(type_name.to_string());
    while let Some(name) = current {
        let type_def = collection.types.get(&name)?;
        if let Some(ref pattern) = type_def.filename_pattern {
            return Some(pattern.clone());
        }
        current = type_def.extends.clone();
    }
    None
}

/// Walk the extends chain to find a `generated` strategy for a field and
/// produce the value.  The child type may override a field without keeping
/// the ancestor's strategy, so we check each ancestor in turn.
fn generate_field_value(
    collection: &Collection,
    type_name: &str,
    field_name: &str,
) -> Option<serde_json::Value> {
    let mut current = Some(type_name.to_string());
    while let Some(name) = current {
        let type_def = collection.types.get(&name)?;
        if let Some(field_def) = type_def.fields.get(field_name) {
            if let Some(strategy) = &field_def.generated {
                return match strategy {
                    GeneratedStrategy::Ulid => {
                        Some(serde_json::json!(ulid::Ulid::new().to_string()))
                    }
                    GeneratedStrategy::Uuid => {
                        Some(serde_json::json!(uuid::Uuid::new_v4().to_string()))
                    }
                    GeneratedStrategy::Now => {
                        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
                        Some(serde_json::json!(ts.to_string()))
                    }
                    // NowOnWrite and Derived are handled by collection.create()
                    _ => None,
                };
            }
        }
        current = type_def.extends.clone();
    }
    None
}

/// Substitute `{field}` placeholders in a filename pattern.
fn derive_path_from_pattern(
    pattern: &str,
    fm: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let mut result = pattern.to_string();
    let mut i = 0;
    while let Some(start) = result[i..].find('{') {
        let start = i + start;
        let end = result[start..].find('}')? + start;
        let field = &result[start + 1..end];
        let value = match fm.get(field) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(v) => v.to_string().trim_matches('"').to_string(),
            None => return None,
        };
        result = format!("{}{}{}", &result[..start], value, &result[end + 1..]);
        i = start + value.len();
    }
    Some(result)
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
            client
                .log_message(MessageType::ERROR, "mdbase collection not loaded")
                .await;
            return Ok(None);
        }
    };

    let result = collection.validate_op(&serde_json::json!({}));
    Ok(Some(result))
}
