use tower_lsp::lsp_types::*;

use crate::state::BackendState;
use crate::text;

/// Provide hover information at the given position.
///
/// TODO: Implement:
/// - Field name hover: show type, constraints, description from type schema
/// - Link hover: show target file's frontmatter preview
/// - Type name hover: show type definition summary
pub fn provide(
    state: &BackendState,
    uri: &Url,
    position: Position,
) -> Option<Hover> {
    let collection = state.get_collection()?;
    let text = state.document_text(uri)?;
    let line_idx = position.line as usize;
    let line_text = text.lines().nth(line_idx).unwrap_or("").to_string();
    let column = position.character as usize;

    if text::is_in_frontmatter(&text, line_idx) {
        let parsed = state.documents.get(uri)
            .map(|doc| doc.frontmatter())
            .unwrap_or_else(|| text::parse_frontmatter(&text));
        if parsed.parse_error || parsed.mapping_error {
            return None;
        }
        let rel_path = uri.to_file_path().ok()
            .and_then(|p| p.strip_prefix(&collection.root).ok().map(|r| r.to_string_lossy().to_string().replace('\\', "/")));
        let type_names = collection.determine_types_for_path(&parsed.json, rel_path.as_deref());

        if let Some(field_name) = text::field_name_from_line(&line_text) {
            let colon_idx = line_text.find(':').unwrap_or(0);
            if column <= colon_idx {
                if let Some(field_def) = field_def_for_types(&collection, &type_names, &field_name) {
                    let mut contents = String::new();
                    contents.push_str(&format!("**{}**: `{}`", field_name, field_def.field_type));
                    if let Some(desc) = field_def.description.as_deref() {
                        contents.push_str(&format!("\n\n{}", desc));
                    }
                    if let Some(deprecated) = field_def.deprecated.as_deref() {
                        contents.push_str(&format!("\n\nDeprecated: {}", deprecated));
                    }
                    return Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: contents,
                        }),
                        range: None,
                    });
                }
            } else {
                if field_name == "type" || field_name == "types" {
                    if let Some(type_name) = text::word_at(&line_text, column) {
                        if let Some(type_def) = collection.types.get(&type_name.to_lowercase()) {
                            let mut contents = String::new();
                            contents.push_str(&format!("**Type** `{}`", type_def.name));
                            if let Some(desc) = type_def.description.as_deref() {
                                contents.push_str(&format!("\n\n{}", desc));
                            }
                            return Some(Hover {
                                contents: HoverContents::Markup(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value: contents,
                                }),
                                range: None,
                            });
                        }
                    }
                }

                if let Some(field_def) = field_def_for_types(&collection, &type_names, &field_name) {
                    if is_link_field(&field_def) {
                        if let Some(rel_path) = rel_path {
                            let resolved = collection.resolve_link(&serde_json::json!({
                                "path": rel_path,
                                "field": field_name,
                            }));
                            if let Some(target) = resolved.get("resolved_path").and_then(|v| v.as_str()) {
                                let read = collection.read(&serde_json::json!({"path": target}));
                                let title = read.get("frontmatter")
                                    .and_then(|fm| fm.get("title"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let types = read.get("types")
                                    .and_then(|v| v.as_array())
                                    .map(|arr| {
                                        arr.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(", ")
                                    })
                                    .unwrap_or_default();
                                let mut contents = String::new();
                                contents.push_str(&format!("**Target** `{}`", target));
                                if !title.is_empty() {
                                    contents.push_str(&format!("\n\nTitle: {}", title));
                                }
                                if !types.is_empty() {
                                    contents.push_str(&format!("\n\nTypes: {}", types));
                                }
                                return Some(Hover {
                                    contents: HoverContents::Markup(MarkupContent {
                                        kind: MarkupKind::Markdown,
                                        value: contents,
                                    }),
                                    range: None,
                                });
                            }
                        }
                    }
                }
            }
        }
    } else if let Some(link) = crate::body_links::body_link_at(&text, line_idx, column) {
        // Body link hover — show target path, title, and types
        let rel_path = uri.to_file_path().ok()
            .and_then(|p| p.strip_prefix(&collection.root).ok().map(|r| r.to_string_lossy().to_string().replace('\\', "/")));
        if let Some(resolved) = crate::collection_utils::resolve_link_target(
            &collection,
            &link.target,
            rel_path.as_deref(),
        ) {
            if let Some(target_rel) = resolved
                .strip_prefix(&collection.root)
                .ok()
                .map(|r| r.to_string_lossy().to_string().replace('\\', "/"))
            {
                let mut contents = format!("**Target** `{}`", target_rel);
                // Try to read frontmatter from the target file for title/types
                if let Ok(target_text) = std::fs::read_to_string(&resolved) {
                    let parsed = text::parse_frontmatter(&target_text);
                    if !parsed.parse_error && !parsed.mapping_error {
                        if let Some(title) = parsed.json.get("title").and_then(|v| v.as_str()) {
                            if !title.is_empty() {
                                contents.push_str(&format!("\n\nTitle: {}", title));
                            }
                        }
                        let types = collection.determine_types_for_path(&parsed.json, Some(&target_rel));
                        if !types.is_empty() {
                            contents.push_str(&format!("\n\nTypes: {}", types.join(", ")));
                        }
                    }
                }
                return Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: contents,
                    }),
                    range: Some(Range {
                        start: Position::new(link.start_line as u32, link.start_col as u32),
                        end: Position::new(link.end_line as u32, link.end_col as u32),
                    }),
                });
            }
        }
    } else if let Some(type_name) = text::word_at(&line_text, column) {
        if let Some(type_def) = collection.types.get(&type_name.to_lowercase()) {
            let mut contents = String::new();
            contents.push_str(&format!("**Type** `{}`", type_def.name));
            if let Some(desc) = type_def.description.as_deref() {
                contents.push_str(&format!("\n\n{}", desc));
            }
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: contents,
                }),
                range: None,
            });
        }
    }

    None
}

fn field_def_for_types(
    collection: &mdbase::Collection,
    type_names: &[String],
    field_name: &str,
) -> Option<mdbase::types::schema::FieldDef> {
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

fn is_link_field(def: &mdbase::types::schema::FieldDef) -> bool {
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
