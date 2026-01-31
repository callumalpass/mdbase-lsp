use tracing::{debug, warn};
use tower_lsp::lsp_types::*;

use crate::collection_utils;
use crate::state::BackendState;
use crate::text;

use mdbase::types::schema::FieldDef;

/// Provide completions at the given position.
///
/// TODO: Implement:
/// - Field name completions (when cursor is at start of a frontmatter line)
/// - Enum value completions (when cursor is after a known enum field's colon)
/// - Link target completions (when cursor is inside [[ ]] or []())
/// - Tag completions (when cursor is after #)
pub fn provide(
    state: &BackendState,
    uri: &Url,
    position: Position,
) -> Option<CompletionResponse> {
    let collection = match state.get_collection() {
        Some(c) => c,
        None => {
            warn!(uri = %uri, "completion: no collection available");
            return None;
        }
    };
    let text = match state.document_text(uri) {
        Some(t) => t,
        None => {
            warn!(uri = %uri, "completion: no document text");
            return None;
        }
    };
    let line_idx = position.line as usize;
    let line_text = text.lines().nth(line_idx).unwrap_or("").to_string();
    let column = position.character as usize;

    let in_frontmatter = text::is_in_frontmatter(&text, line_idx);
    if !in_frontmatter {
        debug!(uri = %uri, line = line_idx, "completion: not in frontmatter");
    }
    if in_frontmatter {
        let colon_idx = line_text.find(':');
        let is_field_name_pos = colon_idx.is_none() || column <= colon_idx.unwrap_or(0);

        let parsed = text::parse_frontmatter(&text);

        // When typing a new field name the incomplete line makes YAML invalid.
        // Remove the current line and re-parse so we can still offer field names.
        if (parsed.parse_error || parsed.mapping_error) && is_field_name_pos {
            debug!(uri = %uri, "completion: frontmatter invalid, trying with current line removed");
            let patched = text::parse_frontmatter(&remove_line(&text, line_idx));
            if patched.parse_error || patched.mapping_error {
                debug!(uri = %uri, "completion: still invalid after removing line");
                return None;
            }
            let rel_path = uri.to_file_path().ok()
                .and_then(|p| p.strip_prefix(&collection.root).ok().map(|r| r.to_string_lossy().to_string().replace('\\', "/")));
            let type_names = collection.determine_types_for_path(&patched.json, rel_path.as_deref());
            debug!(uri = %uri, ?type_names, "completion: resolved types (patched)");
            let existing: std::collections::HashSet<String> = patched.json.as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            let fields = fields_for_types(&collection, &type_names);
            let items: Vec<CompletionItem> = fields.into_iter()
                .filter(|(name, _)| !existing.contains(name))
                .map(|(name, def)| {
                    let mut item = CompletionItem::new_simple(name.clone(), field_detail(&def));
                    item.kind = Some(CompletionItemKind::FIELD);
                    item
                }).collect();
            return Some(CompletionResponse::Array(items));
        }

        if parsed.parse_error || parsed.mapping_error {
            debug!(uri = %uri, "completion: frontmatter parse/mapping error (value position)");
            return None;
        }

        let rel_path = uri.to_file_path().ok()
            .and_then(|p| p.strip_prefix(&collection.root).ok().map(|r| r.to_string_lossy().to_string().replace('\\', "/")));
        let type_names = collection.determine_types_for_path(&parsed.json, rel_path.as_deref());
        debug!(uri = %uri, ?type_names, "completion: resolved types");

        if is_field_name_pos {
            let existing: std::collections::HashSet<String> = parsed.json.as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            let fields = fields_for_types(&collection, &type_names);
            let items: Vec<CompletionItem> = fields.into_iter()
                .filter(|(name, _)| !existing.contains(name))
                .map(|(name, def)| {
                    let mut item = CompletionItem::new_simple(name.clone(), field_detail(&def));
                    item.kind = Some(CompletionItemKind::FIELD);
                    item
                }).collect();
            return Some(CompletionResponse::Array(items));
        }

        if let Some(field_name) = text::field_name_from_line(&line_text) {
            debug!(uri = %uri, field_name = %field_name, "completion: looking up field def for value completion");
            if let Some(field_def) = field_def_for_types(&collection, &type_names, &field_name) {
                debug!(uri = %uri, field_name = %field_name, field_type = %field_def.field_type, has_values = field_def.values.is_some(), "completion: found field def");
                if let Some(values) = &field_def.values {
                    let items = values.iter().map(|v| CompletionItem {
                        label: v.clone(),
                        kind: Some(CompletionItemKind::ENUM_MEMBER),
                        ..Default::default()
                    }).collect();
                    return Some(CompletionResponse::Array(items));
                }
                if field_def.field_type == "boolean" {
                    let items = vec![
                        CompletionItem::new_simple("true".to_string(), "boolean".to_string()),
                        CompletionItem::new_simple("false".to_string(), "boolean".to_string()),
                    ];
                    return Some(CompletionResponse::Array(items));
                }
                if is_link_field(&field_def) {
                    let target_type = link_target_type(&field_def);
                    let items = link_target_completions(&collection, target_type.as_deref());
                    return Some(CompletionResponse::Array(items));
                }
            } else {
                debug!(uri = %uri, field_name = %field_name, "completion: no field def found");
            }
        } else {
            debug!(uri = %uri, line_text = %line_text, "completion: could not extract field name from line");
        }
    } else if column > 0 {
        let prefix = line_text.chars().take(column).collect::<String>();
        if prefix.ends_with('#') {
            let items = tag_completions(&collection);
            return Some(CompletionResponse::Array(items));
        }
    }

    None
}

fn fields_for_types(collection: &mdbase::Collection, type_names: &[String]) -> Vec<(String, FieldDef)> {
    let mut fields = std::collections::HashMap::new();
    if type_names.is_empty() {
        for type_def in collection.types.values() {
            for (name, def) in &type_def.fields {
                fields.entry(name.clone()).or_insert_with(|| def.clone());
            }
        }
    } else {
        for type_name in type_names {
            if let Some(type_def) = collection.types.get(type_name) {
                for (name, def) in &type_def.fields {
                    fields.entry(name.clone()).or_insert_with(|| def.clone());
                }
            }
        }
    }
    let mut result: Vec<(String, FieldDef)> = fields.into_iter().collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

fn field_def_for_types(
    collection: &mdbase::Collection,
    type_names: &[String],
    field_name: &str,
) -> Option<FieldDef> {
    if type_names.is_empty() {
        for type_def in collection.types.values() {
            if let Some(def) = type_def.fields.get(field_name) {
                return Some(def.clone());
            }
        }
        None
    } else {
        for type_name in type_names {
            if let Some(type_def) = collection.types.get(type_name) {
                if let Some(def) = type_def.fields.get(field_name) {
                    return Some(def.clone());
                }
            }
        }
        None
    }
}

fn field_detail(def: &FieldDef) -> String {
    let mut parts = vec![def.field_type.clone()];
    if def.required {
        parts.push("required".to_string());
    }
    if def.deprecated.is_some() {
        parts.push("deprecated".to_string());
    }
    parts.join(", ")
}

fn is_link_field(def: &FieldDef) -> bool {
    if def.field_type == "link" {
        return true;
    }
    if def.field_type == "list" {
        if let Some(item) = &def.items {
            return item.field_type == "link";
        }
    }
    false
}

fn link_target_type(def: &FieldDef) -> Option<String> {
    if def.field_type == "link" {
        def.target.clone()
    } else if def.field_type == "list" {
        def.items.as_ref().and_then(|i| i.target.clone())
    } else {
        None
    }
}

fn link_target_completions(
    collection: &mdbase::Collection,
    target_type: Option<&str>,
) -> Vec<CompletionItem> {
    let files = collection_utils::scan_collection_files(collection);
    let mut items = Vec::new();

    for path in files {
        let rel_path = match path.strip_prefix(&collection.root) {
            Ok(p) => p.to_string_lossy().to_string().replace('\\', "/"),
            Err(_) => continue,
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let parsed = text::parse_frontmatter(&content);
        if parsed.parse_error || parsed.mapping_error {
            continue;
        }
        if let Some(tt) = target_type {
            let types = collection.determine_types_for_path(&parsed.json, Some(&rel_path));
            if !types.iter().any(|t| t.eq_ignore_ascii_case(tt)) {
                continue;
            }
        }

        let label = rel_path.clone();
        items.push(CompletionItem {
            label,
            kind: Some(CompletionItemKind::FILE),
            ..Default::default()
        });
    }

    items
}

/// Remove a single line from text by index, preserving all other lines.
fn remove_line(text: &str, line_idx: usize) -> String {
    text.lines()
        .enumerate()
        .filter(|(i, _)| *i != line_idx)
        .map(|(_, l)| l)
        .collect::<Vec<_>>()
        .join("\n")
}

fn tag_completions(collection: &mdbase::Collection) -> Vec<CompletionItem> {
    let files = collection_utils::scan_collection_files(collection);
    let mut tags: Vec<String> = Vec::new();

    for path in files {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let parsed_doc = mdbase::frontmatter::parser::parse_document(&content);
        let fm_json = text::parse_frontmatter(&content);
        if let Some(arr) = fm_json.json.get("tags").and_then(|v| v.as_array()) {
            for tag_val in arr {
                if let Some(tag) = tag_val.as_str() {
                    if !tags.contains(&tag.to_string()) {
                        tags.push(tag.to_string());
                    }
                }
            }
        } else if let Some(tag) = fm_json.json.get("tags").and_then(|v| v.as_str()) {
            if !tags.contains(&tag.to_string()) {
                tags.push(tag.to_string());
            }
        }
        let body_tags = mdbase::expressions::evaluator::extract_tags_from_body(&parsed_doc.body);
        for tag in body_tags {
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }

    tags.sort();
    tags.into_iter()
        .map(|tag| CompletionItem {
            label: tag,
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        })
        .collect()
}
