use tower_lsp::lsp_types::*;

use crate::collection_utils;
use crate::state::{BackendState, CollectionContext};
use crate::text;

/// Provide hover information at the given position.
pub fn provide(state: &BackendState, uri: &Url, position: Position) -> Option<Hover> {
    let (ctx, source_rel_path) = state.context_and_rel_path_for_uri(uri)?;
    let collection = &ctx.collection;
    let text = state.document_text(uri)?;
    let line_idx = position.line as usize;
    let line_text = text.lines().nth(line_idx).unwrap_or("").to_string();
    let column = position.character as usize;

    if text::is_in_frontmatter(&text, line_idx)
        || text::is_in_frontmatter_edit_region(&text, line_idx)
    {
        let parsed = state
            .documents
            .get(uri)
            .map(|doc| doc.frontmatter())
            .unwrap_or_else(|| text::parse_frontmatter(&text));

        let type_names = if parsed.parse_error || parsed.mapping_error {
            Vec::new()
        } else {
            collection.determine_types_for_path(&parsed.json, Some(&source_rel_path))
        };

        // Field key hover (only when cursor is on the key side of `field: value`).
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
            }
        }

        // Value-side hover in frontmatter (supports list items too).
        if let Some(value) = text::value_from_frontmatter_line(&line_text, column) {
            if let Some(field_name) = text::field_name_for_position(&text, line_idx) {
                if field_name == "type" || field_name == "types" {
                    if let Some(type_name) = text::word_at(&line_text, column) {
                        if let Some(type_def) = collection.types().get(&type_name.to_lowercase()) {
                            return Some(type_hover(type_def));
                        }
                    }
                }

                if let Some(field_def) = field_def_for_types(collection, &type_names, &field_name) {
                    if is_link_field(&field_def) {
                        let (target, anchor) =
                            parse_link_target_and_anchor(&value).or_else(|| {
                                collection_utils::parse_link_value(&value).map(|t| (t, None))
                            })?;
                        return build_link_hover(
                            state,
                            &ctx,
                            &source_rel_path,
                            &target,
                            anchor.as_deref(),
                            None,
                        );
                    }
                }
            }
        }
    } else if let Some(link) = crate::body_links::body_link_at(&text, line_idx, column) {
        return build_link_hover(
            state,
            &ctx,
            &source_rel_path,
            &link.target,
            link.anchor.as_deref(),
            Some(Range {
                start: Position::new(link.start_line as u32, link.start_col as u32),
                end: Position::new(link.end_line as u32, link.end_col as u32),
            }),
        );
    } else if let Some(type_name) = text::word_at(&line_text, column) {
        if let Some(type_def) = collection.types().get(&type_name.to_lowercase()) {
            return Some(type_hover(type_def));
        }
    }

    None
}

fn type_hover(type_def: &mdbase::types::schema::TypeDef) -> Hover {
    let mut contents = String::new();
    contents.push_str(&format!("**Type** `{}`", type_def.name));
    if let Some(desc) = type_def.description.as_deref() {
        contents.push_str(&format!("\n\n{}", desc));
    }
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: contents,
        }),
        range: None,
    }
}

fn build_link_hover(
    state: &BackendState,
    ctx: &CollectionContext,
    source_rel_path: &str,
    target: &str,
    anchor: Option<&str>,
    range: Option<Range>,
) -> Option<Hover> {
    let mut contents = String::new();
    let resolved_rel =
        ctx.file_index
            .resolve_target_rel_path(&ctx.collection, target, Some(source_rel_path));

    if let Some(target_rel) = resolved_rel {
        contents.push_str(&format!("**Target** `{}`", target_rel));

        let resolved = ctx.collection.root().join(&target_rel);
        if let Some(target_text) = read_target_text(state, &resolved) {
            let (effective_frontmatter, types) = effective_frontmatter_and_types(ctx, &target_rel)
                .unwrap_or_else(|| {
                    let parsed = text::parse_frontmatter(&target_text);
                    if parsed.parse_error || parsed.mapping_error {
                        (serde_json::json!({}), Vec::new())
                    } else {
                        let types = ctx
                            .collection
                            .determine_types_for_path(&parsed.json, Some(&target_rel));
                        (parsed.json, types)
                    }
                });

            if let Some((key, value)) =
                display_name_for_types(&ctx.collection, &types, &effective_frontmatter)
            {
                contents.push_str(&format!("\n\nDisplay name (`{}`): {}", key, value));
            }
            if !types.is_empty() {
                contents.push_str(&format!("\n\nTypes: {}", types.join(", ")));
            }

            if let Some(anchor) = anchor {
                contents.push_str(&format!("\n\nAnchor: `#{}`", anchor));
                if let Some((line, heading)) = find_heading_for_anchor(&target_text, anchor) {
                    contents.push_str(&format!("\n\nHeading: `{}` (line {})", heading, line + 1));
                } else {
                    contents.push_str("\n\nHeading: not found");
                }
            }

            if let Some(summary) = extract_body_summary_line(&target_text) {
                contents.push_str(&format!("\n\nPreview: {}", summary));
            }
        }
    } else {
        contents.push_str(&format!("**Target** `{}`", target));
        if let Some(anchor) = anchor {
            contents.push_str(&format!("\n\nAnchor: `#{}`", anchor));
        }
        contents.push_str("\n\nUnresolved link target");
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: contents,
        }),
        range,
    })
}

fn effective_frontmatter_and_types(
    ctx: &CollectionContext,
    target_rel: &str,
) -> Option<(serde_json::Value, Vec<String>)> {
    let result = ctx
        .collection
        .read(&serde_json::json!({ "path": target_rel }));
    if result.get("error").is_some() {
        return None;
    }

    let frontmatter = result.get("frontmatter")?.clone();
    let types = result
        .get("types")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some((frontmatter, types))
}

fn display_name_for_types(
    collection: &mdbase::Collection,
    type_names: &[String],
    frontmatter: &serde_json::Value,
) -> Option<(String, String)> {
    for type_name in type_names {
        let Some(type_def) = collection
            .types()
            .get(type_name)
            .or_else(|| collection.types().get(&type_name.to_lowercase()))
        else {
            continue;
        };
        let Some(key) = type_def.display_name_key.as_deref() else {
            continue;
        };
        let Some(value) = frontmatter.get(key).and_then(frontmatter_display_value) else {
            continue;
        };
        return Some((key.to_string(), value));
    }
    None
}

fn frontmatter_display_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

fn parse_link_target_and_anchor(value: &str) -> Option<(String, Option<String>)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let (value, _) = strip_wrapping_quotes(value);
    if value.is_empty() {
        return None;
    }

    // Wikilink: [[target#anchor|alias]]
    if value.starts_with("[[") && value.ends_with("]]") {
        let inner = &value[2..value.len() - 2];
        let target = inner.split('|').next().unwrap_or(inner).trim();
        return split_anchor(target);
    }

    // Markdown link: [text](path#anchor)
    if value.starts_with('[') && value.ends_with(')') {
        if let Some(bracket_end) = value.find("](") {
            let path = value[bracket_end + 2..value.len() - 1].trim();
            if path.starts_with("http://") || path.starts_with("https://") {
                return None;
            }
            return split_anchor(path);
        }
    }

    // Bare path
    if value.starts_with("http://") || value.starts_with("https://") {
        return None;
    }

    split_anchor(value)
}

fn strip_wrapping_quotes(value: &str) -> (&str, Option<char>) {
    if value.len() >= 2 {
        let first = value.as_bytes()[0] as char;
        let last = value.as_bytes()[value.len() - 1] as char;
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            return (&value[1..value.len() - 1], Some(first));
        }
    }
    (value, None)
}

fn split_anchor(value: &str) -> Option<(String, Option<String>)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(idx) = value.find('#') {
        let target = value[..idx].trim().to_string();
        let anchor = value[idx + 1..].trim();
        if target.is_empty() {
            return None;
        }
        return Some((
            target,
            if anchor.is_empty() {
                None
            } else {
                Some(anchor.to_string())
            },
        ));
    }
    Some((value.to_string(), None))
}

fn find_heading_for_anchor(text: &str, anchor: &str) -> Option<(usize, String)> {
    let target_anchor = slugify_anchor(anchor);
    if target_anchor.is_empty() {
        return None;
    }

    let skip_until_line = text::frontmatter_bounds(text).map(|(_, end)| end + 1);
    let mut in_fenced_block = false;

    for (line_idx, line) in text.lines().enumerate() {
        if let Some(skip_until) = skip_until_line {
            if line_idx <= skip_until {
                continue;
            }
        }

        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fenced_block = !in_fenced_block;
            continue;
        }
        if in_fenced_block {
            continue;
        }

        let Some((heading, _level)) = parse_atx_heading(trimmed) else {
            continue;
        };
        if slugify_anchor(&heading) == target_anchor {
            return Some((line_idx, heading));
        }
    }

    None
}

fn parse_atx_heading(line: &str) -> Option<(String, usize)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 {
        return None;
    }

    let mut chars = line.chars();
    for _ in 0..hashes {
        chars.next()?;
    }
    if !chars.next().is_some_and(|c| c.is_whitespace()) {
        return None;
    }

    let raw = line[hashes..].trim();
    let heading = raw.trim_end_matches('#').trim();
    if heading.is_empty() {
        return None;
    }

    Some((heading.to_string(), hashes))
}

fn slugify_anchor(input: &str) -> String {
    let input = input.trim().trim_start_matches('#').trim();
    let mut out = String::new();
    let mut pending_dash = false;

    for ch in input.chars() {
        if ch.is_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            for lowered in ch.to_lowercase() {
                out.push(lowered);
            }
            continue;
        }

        if ch == '-' || ch.is_whitespace() {
            pending_dash = true;
        }
    }

    out
}

fn extract_body_summary_line(text: &str) -> Option<String> {
    let body = mdbase::frontmatter::parser::parse_document(text).body;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return Some(trim_preview_text(trimmed, 120));
    }
    None
}

fn trim_preview_text(s: &str, max_chars: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }

    let mut out: String = collapsed.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

fn field_def_for_types(
    collection: &mdbase::Collection,
    type_names: &[String],
    field_name: &str,
) -> Option<mdbase::types::schema::FieldDef> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_link_target_and_anchor_wikilink() {
        assert_eq!(
            parse_link_target_and_anchor("[[notes/idea#next-step|Idea]]"),
            Some(("notes/idea".to_string(), Some("next-step".to_string())))
        );
    }

    #[test]
    fn parse_link_target_and_anchor_markdown_quoted() {
        assert_eq!(
            parse_link_target_and_anchor("'[Read](notes/idea.md#Overview)'"),
            Some(("notes/idea.md".to_string(), Some("Overview".to_string())))
        );
    }

    #[test]
    fn parse_link_target_and_anchor_bare_path() {
        assert_eq!(
            parse_link_target_and_anchor("notes/idea.md#details"),
            Some(("notes/idea.md".to_string(), Some("details".to_string())))
        );
    }

    #[test]
    fn find_heading_for_anchor_matches_heading_slug() {
        let text = "---\ntitle: Demo\n---\n\n## Next Steps\nBody";
        let found = find_heading_for_anchor(text, "next-steps").unwrap();
        assert_eq!(found.0, 4);
        assert_eq!(found.1, "Next Steps");
    }

    #[test]
    fn slugify_anchor_drops_punctuation() {
        assert_eq!(slugify_anchor("Project: Plan!"), "project-plan");
    }
}
