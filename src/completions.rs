use tower_lsp::lsp_types::*;
use tracing::{debug, warn};

use crate::state::BackendState;
use crate::text;

use mdbase::types::schema::FieldDef;

/// Provide completions at the given position.
pub fn provide(state: &BackendState, uri: &Url, position: Position) -> Option<CompletionResponse> {
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

    // Check for link completion context first — works in both body and frontmatter
    if let Some(ctx) = text::link_completion_context(&line_text, column) {
        let rel_path = uri.to_file_path().ok().and_then(|p| {
            p.strip_prefix(&collection.root)
                .ok()
                .map(|r| r.to_string_lossy().to_string().replace('\\', "/"))
        });
        return Some(CompletionResponse::Array(provide_link_completions(
            state,
            &ctx,
            line_idx,
            column,
            rel_path.as_deref(),
        )));
    }

    let in_frontmatter = text::is_in_frontmatter(&text, line_idx);
    if !in_frontmatter {
        debug!(uri = %uri, line = line_idx, "completion: not in frontmatter");
    }
    if in_frontmatter {
        let colon_idx = line_text.find(':');
        let is_field_name_pos = colon_idx.is_none() || column <= colon_idx.unwrap_or(0);

        let parsed = state
            .documents
            .get(uri)
            .map(|doc| doc.frontmatter())
            .unwrap_or_else(|| text::parse_frontmatter(&text));

        // When typing a new field name the incomplete line makes YAML invalid.
        // Remove the current line and re-parse so we can still offer field names.
        if (parsed.parse_error || parsed.mapping_error) && is_field_name_pos {
            debug!(uri = %uri, "completion: frontmatter invalid, trying with current line removed");
            let patched = text::parse_frontmatter(&remove_line(&text, line_idx));
            if patched.parse_error || patched.mapping_error {
                debug!(uri = %uri, "completion: still invalid after removing line");
                return None;
            }
            let rel_path = uri.to_file_path().ok().and_then(|p| {
                p.strip_prefix(&collection.root)
                    .ok()
                    .map(|r| r.to_string_lossy().to_string().replace('\\', "/"))
            });
            let type_names =
                collection.determine_types_for_path(&patched.json, rel_path.as_deref());
            debug!(uri = %uri, ?type_names, "completion: resolved types (patched)");
            let existing: std::collections::HashSet<String> = patched
                .json
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            let fields = fields_for_types(&collection, &type_names);
            let items: Vec<CompletionItem> = fields
                .into_iter()
                .filter(|(name, _)| !existing.contains(name))
                .map(|(name, def)| {
                    let mut item = CompletionItem::new_simple(name.clone(), field_detail(&def));
                    item.kind = Some(CompletionItemKind::FIELD);
                    item.documentation = field_documentation(&def);
                    item
                })
                .collect();
            return Some(CompletionResponse::Array(items));
        }

        if parsed.parse_error || parsed.mapping_error {
            debug!(uri = %uri, "completion: frontmatter parse/mapping error (value position)");
            return None;
        }

        let rel_path = uri.to_file_path().ok().and_then(|p| {
            p.strip_prefix(&collection.root)
                .ok()
                .map(|r| r.to_string_lossy().to_string().replace('\\', "/"))
        });
        let type_names = collection.determine_types_for_path(&parsed.json, rel_path.as_deref());
        debug!(uri = %uri, ?type_names, "completion: resolved types");

        if is_field_name_pos {
            let existing: std::collections::HashSet<String> = parsed
                .json
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            let fields = fields_for_types(&collection, &type_names);
            let items: Vec<CompletionItem> = fields
                .into_iter()
                .filter(|(name, _)| !existing.contains(name))
                .map(|(name, def)| {
                    let mut item = CompletionItem::new_simple(name.clone(), field_detail(&def));
                    item.kind = Some(CompletionItemKind::FIELD);
                    item.documentation = field_documentation(&def);
                    item
                })
                .collect();
            return Some(CompletionResponse::Array(items));
        }

        if let Some(field_name) = text::field_name_from_line(&line_text) {
            debug!(uri = %uri, field_name = %field_name, "completion: looking up field def for value completion");
            if let Some(field_def) = field_def_for_types(&collection, &type_names, &field_name) {
                debug!(uri = %uri, field_name = %field_name, field_type = %field_def.field_type, has_values = field_def.values.is_some(), "completion: found field def");
                if let Some(values) = &field_def.values {
                    let items = values
                        .iter()
                        .map(|v| CompletionItem {
                            label: v.clone(),
                            kind: Some(CompletionItemKind::ENUM_MEMBER),
                            detail: Some(format!("{} value", field_name)),
                            ..Default::default()
                        })
                        .collect();
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
                    let items = link_target_completions(state, target_type.as_deref());
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
            let items = tag_completions(state);
            return Some(CompletionResponse::Array(items));
        }
    }

    None
}

fn fields_for_types(
    collection: &mdbase::Collection,
    type_names: &[String],
) -> Vec<(String, FieldDef)> {
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

fn field_documentation(def: &FieldDef) -> Option<Documentation> {
    let mut lines = Vec::new();
    if let Some(desc) = &def.description {
        lines.push(desc.clone());
    }
    if let Some(values) = &def.values {
        lines.push(format!("Allowed: {}", values.join(", ")));
    }
    if let Some(default) = &def.default {
        lines.push(format!("Default: {}", default));
    }
    if let Some(deprecated) = &def.deprecated {
        lines.push(format!("Deprecated: {}", deprecated));
    }
    if lines.is_empty() {
        None
    } else {
        Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: lines.join("\n\n"),
        }))
    }
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

fn link_target_completions(state: &BackendState, target_type: Option<&str>) -> Vec<CompletionItem> {
    state
        .file_index
        .link_targets(target_type)
        .into_iter()
        .map(|rel_path| CompletionItem {
            label: rel_path,
            kind: Some(CompletionItemKind::FILE),
            ..Default::default()
        })
        .collect()
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

fn tag_completions(state: &BackendState) -> Vec<CompletionItem> {
    state
        .file_index
        .tag_counts()
        .into_iter()
        .map(|(tag, count)| CompletionItem {
            label: tag,
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some(format!("used {} times", count)),
            ..Default::default()
        })
        .collect()
}

fn provide_link_completions(
    state: &BackendState,
    ctx: &text::LinkCompletionContext,
    line_idx: usize,
    column: usize,
    source_rel_path: Option<&str>,
) -> Vec<CompletionItem> {
    let targets = state.file_index.link_targets_with_display(None);
    let edit_range = Range {
        start: Position::new(line_idx as u32, ctx.start_col as u32),
        end: Position::new(line_idx as u32, column as u32),
    };

    targets
        .into_iter()
        .map(|(rel_path, display_name, preview)| match ctx.kind {
            text::LinkCompletionKind::Wikilink => {
                let stem = rel_path.strip_suffix(".md").unwrap_or(&rel_path);
                let label = display_name.clone().unwrap_or_else(|| stem.to_string());
                let insert_text = match display_name.as_deref() {
                    Some(name) if !name.is_empty() && name != stem => {
                        format!("{}|{}", stem, name)
                    }
                    _ => stem.to_string(),
                };
                let filter_text = display_name
                    .as_ref()
                    .map(|d| format!("{} {} {}", d, stem, rel_path));
                let documentation = preview.map(|p| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::PlainText,
                        value: p,
                    })
                });
                CompletionItem {
                    label,
                    detail: Some(rel_path.clone()),
                    kind: Some(CompletionItemKind::FILE),
                    insert_text: Some(insert_text.clone()),
                    filter_text,
                    documentation,
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: edit_range,
                        new_text: insert_text,
                    })),
                    ..Default::default()
                }
            }
            text::LinkCompletionKind::Markdown => {
                let label = match source_rel_path {
                    Some(src) => relative_path_from(src, &rel_path),
                    None => rel_path.clone(),
                };
                CompletionItem {
                    label: label.clone(),
                    detail: Some(rel_path.clone()),
                    kind: Some(CompletionItemKind::FILE),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: edit_range,
                        new_text: label,
                    })),
                    ..Default::default()
                }
            }
        })
        .collect()
}

/// Compute a relative path from `source` to `target`, where both are
/// collection-relative paths (e.g. `notes/foo.md`, `other/bar.md`).
fn relative_path_from(source: &str, target: &str) -> String {
    let src_dir = match source.rfind('/') {
        Some(i) => &source[..i],
        None => "",
    };
    let tgt_parts: Vec<&str> = target.split('/').collect();
    let src_parts: Vec<&str> = if src_dir.is_empty() {
        Vec::new()
    } else {
        src_dir.split('/').collect()
    };

    // Find the common prefix length
    let common = src_parts
        .iter()
        .zip(tgt_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let ups = src_parts.len() - common;
    let mut parts: Vec<&str> = Vec::new();
    for _ in 0..ups {
        parts.push("..");
    }
    for segment in &tgt_parts[common..] {
        parts.push(segment);
    }

    if parts.is_empty() {
        tgt_parts.last().unwrap_or(&"").to_string()
    } else {
        parts.join("/")
    }
}
