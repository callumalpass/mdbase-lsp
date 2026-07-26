use tower_lsp::lsp_types::*;
use tracing::{debug, warn};

use crate::file_index::FileIndex;
use crate::state::BackendState;
use crate::text;

use mdbase::types::schema::FieldDef;

/// Provide completions at the given position.
pub fn provide(state: &BackendState, uri: &Url, position: Position) -> Option<CompletionResponse> {
    let Some((ctx, source_rel_path)) = state.context_and_rel_path_for_uri(uri) else {
        warn!(uri = %uri, "completion: no collection context available");
        return None;
    };
    let collection = &ctx.collection;
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
    let in_frontmatter = text::is_in_frontmatter(&text, line_idx)
        || text::is_in_frontmatter_edit_region(&text, line_idx);

    // Check for link completion context first — works in both body and frontmatter
    if let Some(link_ctx) = text::link_completion_context(&line_text, column) {
        let target_type = if in_frontmatter {
            link_target_type_for_position(state, uri, &text, line_idx, collection, &source_rel_path)
        } else {
            None
        };
        return Some(CompletionResponse::Array(provide_link_completions(
            &ctx.file_index,
            &link_ctx,
            line_idx,
            column,
            Some(&source_rel_path),
            target_type.as_deref(),
        )));
    }

    if !in_frontmatter {
        debug!(uri = %uri, line = line_idx, "completion: not in frontmatter");
    }
    if in_frontmatter {
        let is_field_name_pos = is_field_name_position(&line_text, column);

        let parsed = parsed_frontmatter_with_recovery(state, uri, &text, line_idx)?;

        let type_names = collection.determine_types_for_path(&parsed.json, Some(&source_rel_path));
        debug!(uri = %uri, ?type_names, "completion: resolved types");

        if is_field_name_pos {
            let existing: std::collections::HashSet<String> = parsed
                .json
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            let fields = fields_for_types(collection, &type_names);
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

        if let Some(field_name) = text::field_name_for_position(&text, line_idx) {
            debug!(uri = %uri, field_name = %field_name, "completion: looking up field def for value completion");
            if let Some(field_def) = field_def_for_types(collection, &type_names, &field_name) {
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
                    let items = link_target_completions(&ctx.file_index, target_type.as_deref());
                    return Some(CompletionResponse::Array(items));
                }
            } else {
                debug!(uri = %uri, field_name = %field_name, "completion: no field def found");
            }
        } else {
            debug!(
                uri = %uri,
                line = line_idx,
                line_text = %line_text,
                "completion: could not extract field name for value position"
            );
        }
    } else if column > 0 {
        let prefix = line_text.chars().take(column).collect::<String>();
        if prefix.ends_with('#') {
            let items = tag_completions(&ctx.file_index);
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
        for type_def in collection.types().values() {
            for (name, def) in &type_def.fields {
                fields.entry(name.clone()).or_insert_with(|| def.clone());
            }
        }
    } else {
        for type_name in type_names {
            if let Some(type_def) = collection.types().get(type_name) {
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
        for type_def in collection.types().values() {
            if let Some(def) = type_def.fields.get(field_name) {
                return Some(def.clone());
            }
        }
        None
    } else {
        for type_name in type_names {
            if let Some(type_def) = collection.types().get(type_name) {
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

fn link_target_completions(
    file_index: &FileIndex,
    target_type: Option<&str>,
) -> Vec<CompletionItem> {
    file_index
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

fn parse_frontmatter_for_completion(text: &str) -> text::ParsedFrontmatter {
    if !text::has_unclosed_frontmatter(text) {
        return text::parse_frontmatter(text);
    }

    // mdbase parser treats unclosed frontmatter as "no frontmatter". For editor
    // completion, parse against a synthetic close delimiter.
    let mut synthetic = text.to_string();
    if !synthetic.ends_with('\n') {
        synthetic.push('\n');
    }
    synthetic.push_str("---\n");
    text::parse_frontmatter(&synthetic)
}

fn parsed_frontmatter_with_recovery(
    state: &BackendState,
    uri: &Url,
    text: &str,
    line_idx: usize,
) -> Option<text::ParsedFrontmatter> {
    let mut parsed = if text::has_unclosed_frontmatter(text) {
        parse_frontmatter_for_completion(text)
    } else {
        state
            .documents
            .get(uri)
            .map(|doc| doc.frontmatter())
            .unwrap_or_else(|| text::parse_frontmatter(text))
    };

    if parsed.parse_error || parsed.mapping_error {
        debug!(
            uri = %uri,
            "completion: frontmatter invalid, trying with current line removed"
        );
        let patched = parse_frontmatter_for_completion(&remove_line(text, line_idx));
        if patched.parse_error || patched.mapping_error {
            debug!(uri = %uri, "completion: still invalid after removing line");
            return None;
        }
        parsed = patched;
    }

    Some(parsed)
}

fn link_target_type_for_position(
    state: &BackendState,
    uri: &Url,
    text: &str,
    line_idx: usize,
    collection: &mdbase::Collection,
    source_rel_path: &str,
) -> Option<String> {
    let field_name = text::field_name_for_position(text, line_idx)?;
    let parsed = parsed_frontmatter_with_recovery(state, uri, text, line_idx)?;
    let type_names = collection.determine_types_for_path(&parsed.json, Some(source_rel_path));
    let field_def = field_def_for_types(collection, &type_names, &field_name)?;
    link_target_type(&field_def)
}

fn is_field_name_position(line: &str, column: usize) -> bool {
    if is_yaml_list_item_line(line) {
        return false;
    }
    let colon_idx = line.find(':');
    colon_idx.is_none() || column <= colon_idx.unwrap_or(0)
}

fn is_yaml_list_item_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix('-') else {
        return false;
    };
    rest.is_empty() || rest.chars().next().is_some_and(|c| c.is_whitespace())
}

fn tag_completions(file_index: &FileIndex) -> Vec<CompletionItem> {
    file_index
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
    file_index: &FileIndex,
    ctx: &text::LinkCompletionContext,
    line_idx: usize,
    column: usize,
    source_rel_path: Option<&str>,
    target_type: Option<&str>,
) -> Vec<CompletionItem> {
    let targets = file_index.link_targets_with_display(target_type);
    let edit_range = Range {
        start: Position::new(line_idx as u32, ctx.start_col as u32),
        end: Position::new(line_idx as u32, column as u32),
    };
    let prefix = ctx.prefix.trim().to_lowercase();

    let mut candidates: Vec<(usize, usize, String, CompletionItem)> = targets
        .into_iter()
        .filter_map(|(rel_path, display_name, preview)| match ctx.kind {
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
                let searchable = format!(
                    "{} {} {}",
                    label.to_lowercase(),
                    stem.to_lowercase(),
                    rel_path.to_lowercase()
                );
                if !prefix.is_empty() && !searchable.contains(&prefix) {
                    return None;
                }
                let rank = rank_match(
                    &prefix,
                    &label.to_lowercase(),
                    &stem.to_lowercase(),
                    &rel_path.to_lowercase(),
                );
                let quality = completion_quality(&rel_path, display_name.is_some());
                Some((
                    rank,
                    quality,
                    label.clone(),
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
                    },
                ))
            }
            text::LinkCompletionKind::Markdown => {
                let label = match source_rel_path {
                    Some(src) => relative_path_from(src, &rel_path),
                    None => rel_path.clone(),
                };
                let searchable = format!("{} {}", label.to_lowercase(), rel_path.to_lowercase());
                if !prefix.is_empty() && !searchable.contains(&prefix) {
                    return None;
                }
                let rank = rank_match(
                    &prefix,
                    &label.to_lowercase(),
                    &label.to_lowercase(),
                    &rel_path.to_lowercase(),
                );
                let quality = completion_quality(&rel_path, false);
                Some((
                    rank,
                    quality,
                    label.clone(),
                    CompletionItem {
                        label: label.clone(),
                        detail: Some(rel_path.clone()),
                        kind: Some(CompletionItemKind::FILE),
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: edit_range,
                            new_text: label,
                        })),
                        ..Default::default()
                    },
                ))
            }
        })
        .collect();

    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    candidates.into_iter().map(|(_, _, _, item)| item).collect()
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
    let mut parts: Vec<&str> = vec![".."; ups];
    for segment in &tgt_parts[common..] {
        parts.push(segment);
    }

    if parts.is_empty() {
        tgt_parts.last().unwrap_or(&"").to_string()
    } else {
        parts.join("/")
    }
}

fn rank_match(prefix: &str, label: &str, stem: &str, rel_path: &str) -> usize {
    if prefix.is_empty() {
        return 3;
    }
    if label == prefix || stem == prefix || rel_path == prefix {
        return 0;
    }
    if label.starts_with(prefix) || stem.starts_with(prefix) || rel_path.starts_with(prefix) {
        return 1;
    }
    if label.contains(prefix) || stem.contains(prefix) || rel_path.contains(prefix) {
        return 2;
    }
    4
}

fn completion_quality(rel_path: &str, has_display_name: bool) -> usize {
    let mut penalty = 0usize;
    if !has_display_name {
        penalty += 15;
    }

    let depth = rel_path.split('/').count().saturating_sub(1);
    penalty += depth.min(15);

    for segment in rel_path.split('/') {
        if segment.starts_with('.') {
            penalty += 40;
        }
        if segment.eq_ignore_ascii_case("node_modules") {
            penalty += 80;
        }
    }

    penalty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_for_completion_parses_unclosed_frontmatter() {
        let text = "---\ntype: task\ntitle: Demo";
        let parsed = parse_frontmatter_for_completion(text);
        assert!(!parsed.parse_error);
        assert!(!parsed.mapping_error);
        assert_eq!(
            parsed.json.get("type").and_then(|v| v.as_str()),
            Some("task")
        );
        assert_eq!(
            parsed.json.get("title").and_then(|v| v.as_str()),
            Some("Demo")
        );
    }

    #[test]
    fn field_name_position_detects_value_for_list_items() {
        assert!(!is_field_name_position("  - note-a", 8));
        assert!(!is_field_name_position("  - ", 4));
    }

    #[test]
    fn field_name_position_detects_key_side_of_mapping_line() {
        assert!(is_field_name_position("status: open", 3));
        assert!(is_field_name_position("status", 6));
        assert!(!is_field_name_position("status: open", 10));
    }

    #[test]
    fn completion_quality_prefers_display_names() {
        assert!(
            completion_quality("people/alice.md", true)
                < completion_quality("people/alice.md", false)
        );
    }

    #[test]
    fn completion_quality_penalizes_hidden_vendor_paths() {
        assert!(
            completion_quality("241029jvb.md", true)
                < completion_quality(".obsidian/plugins/example/node_modules/README.md", false)
        );
    }
}
