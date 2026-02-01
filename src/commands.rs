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
        "mdbase.typeInfo" => type_info(state, args).await,
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

/// Return metadata about a type, including which fields need user input.
///
/// A "prompt field" is one that is required, has no default value, and has
/// no generated strategy anywhere in the extends chain.
async fn type_info(
    state: &BackendState,
    args: &[serde_json::Value],
) -> Result<Option<serde_json::Value>> {
    let collection = match state.get_collection() {
        Some(c) => c,
        None => return Ok(None),
    };

    let input = args.get(0).cloned().unwrap_or_else(|| serde_json::json!({}));
    let type_name = match input.get("type").and_then(|v| v.as_str()) {
        Some(t) => t.to_lowercase(),
        None => return Ok(None),
    };

    let type_def = match collection.types.get(&type_name) {
        Some(t) => t,
        None => return Ok(None),
    };

    let mut prompt_fields = Vec::new();
    for (field_name, field_def) in &type_def.fields {
        if field_def.required
            && field_def.default.is_none()
            && !has_generated_in_chain(&collection, &type_name, field_name)
        {
            let mut info = serde_json::json!({
                "name": field_name,
                "type": field_def.field_type,
            });
            if let Some(desc) = &field_def.description {
                info["description"] = serde_json::json!(desc);
            }
            if let Some(ref values) = field_def.values {
                info["values"] = serde_json::json!(values);
            }
            prompt_fields.push(info);
        }
    }

    Ok(Some(serde_json::json!({ "prompt_fields": prompt_fields })))
}

/// Create a new file scaffolded from a type definition.
///
/// Pre-generates values for fields with generated strategies and derives
/// the file path from the inherited filename_pattern when none is provided.
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
                if let Some(value) = generate_field_value(&collection, &tn_lower, field_name) {
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
                    if let Some(path) = derive_path_from_pattern(&pattern, &fm_obj) {
                        input
                            .as_object_mut()
                            .unwrap()
                            .insert("path".to_string(), serde_json::json!(path));
                    }
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

/// Check whether a field has a generated strategy anywhere in the extends chain.
fn has_generated_in_chain(collection: &Collection, type_name: &str, field_name: &str) -> bool {
    let mut current = Some(type_name.to_string());
    while let Some(name) = current {
        let type_def = match collection.types.get(&name) {
            Some(t) => t,
            None => break,
        };
        if let Some(field_def) = type_def.fields.get(field_name) {
            if field_def.generated.is_some() {
                return true;
            }
        }
        current = type_def.extends.clone();
    }
    false
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
