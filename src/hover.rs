use tower_lsp::lsp_types::*;

use crate::collection_utils;
use crate::state::BackendState;
use crate::text;

/// Provide hover information at the given position.
///
/// TODO: Implement:
/// - Field name hover: show type, constraints, description from type schema
/// - Link hover: show target file's frontmatter preview
/// - Type name hover: show type definition summary
pub fn provide(state: &BackendState, uri: &Url, position: Position) -> Option<Hover> {
    let (ctx, source_rel_path) = state.context_and_rel_path_for_uri(uri)?;
    let collection = &ctx.collection;
    let text = state.document_text(uri)?;
    let line_idx = position.line as usize;
    let line_text = text.lines().nth(line_idx).unwrap_or("").to_string();
    let column = position.character as usize;

    if text::is_in_frontmatter(&text, line_idx) {
        let parsed = state
            .documents
            .get(uri)
            .map(|doc| doc.frontmatter())
            .unwrap_or_else(|| text::parse_frontmatter(&text));
        if parsed.parse_error || parsed.mapping_error {
            return None;
        }
        let type_names = collection.determine_types_for_path(&parsed.json, Some(&source_rel_path));

        if let Some(field_name) = text::field_name_from_line(&line_text) {
            let colon_idx = line_text.find(':').unwrap_or(0);
            if column <= colon_idx {
                if let Some(field_def) = field_def_for_types(collection, &type_names, &field_name) {
                    let mut contents = String::new();
                    contents.push_str(&format!("**{}**: `{}`", field_name, field_def.field_type));
                    if field_def.required {
                        contents.push_str("\n\nRequired");
                    }
                    if let Some(values) = &field_def.values {
                        if !values.is_empty() {
                            contents.push_str(&format!("\n\nAllowed: {}", values.join(", ")));
                        }
                    }
                    if let Some(default) = &field_def.default {
                        contents.push_str(&format!("\n\nDefault: {}", default));
                    }
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

                if let Some(field_def) = field_def_for_types(collection, &type_names, &field_name) {
                    if is_link_field(&field_def) {
                        if let Some(value) = text::value_from_frontmatter_line(&line_text, column) {
                            let target =
                                collection_utils::parse_link_value(&value).unwrap_or(value);
                            if let Some(resolved) = ctx.file_index.resolve_target_abs_path(
                                &ctx.collection,
                                &target,
                                Some(&source_rel_path),
                            ) {
                                if let Some(target_rel) = resolved
                                    .strip_prefix(&collection.root)
                                    .ok()
                                    .map(|r| r.to_string_lossy().to_string().replace('\\', "/"))
                                {
                                    let mut contents = format!("**Target** `{}`", target_rel);
                                    if let Some(target_text) = read_target_text(state, &resolved) {
                                        let parsed = text::parse_frontmatter(&target_text);
                                        if !parsed.parse_error && !parsed.mapping_error {
                                            if let Some(title) =
                                                parsed.json.get("title").and_then(|v| v.as_str())
                                            {
                                                if !title.is_empty() {
                                                    contents
                                                        .push_str(&format!("\n\nTitle: {}", title));
                                                }
                                            }
                                            let types = collection.determine_types_for_path(
                                                &parsed.json,
                                                Some(&target_rel),
                                            );
                                            if !types.is_empty() {
                                                contents.push_str(&format!(
                                                    "\n\nTypes: {}",
                                                    types.join(", ")
                                                ));
                                            }
                                        }
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
        }
    } else if let Some(link) = crate::body_links::body_link_at(&text, line_idx, column) {
        // Body link hover — show target path, title, and types
        if let Some(resolved) = ctx.file_index.resolve_target_abs_path(
            &ctx.collection,
            &link.target,
            Some(&source_rel_path),
        ) {
            if let Some(target_rel) = resolved
                .strip_prefix(&collection.root)
                .ok()
                .map(|r| r.to_string_lossy().to_string().replace('\\', "/"))
            {
                let mut contents = format!("**Target** `{}`", target_rel);
                if let Some(target_text) = read_target_text(state, &resolved) {
                    let parsed = text::parse_frontmatter(&target_text);
                    if !parsed.parse_error && !parsed.mapping_error {
                        if let Some(title) = parsed.json.get("title").and_then(|v| v.as_str()) {
                            if !title.is_empty() {
                                contents.push_str(&format!("\n\nTitle: {}", title));
                            }
                        }
                        let types =
                            collection.determine_types_for_path(&parsed.json, Some(&target_rel));
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

fn read_target_text(state: &BackendState, resolved: &std::path::Path) -> Option<String> {
    if let Ok(uri) = Url::from_file_path(resolved) {
        if let Some(text) = state.document_text(&uri) {
            return Some(text);
        }
    }
    std::fs::read_to_string(resolved).ok()
}
