use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::body_links::{self, LinkFormat};
use crate::state::{BackendState, CollectionContext};
use crate::text;

pub(crate) fn provide(state: &BackendState, params: ReferenceParams) -> Option<Vec<Location>> {
    let uri = &params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let source_text = state.document_text(uri)?;
    let (ctx, source_rel) = state.context_and_rel_path_for_uri(uri)?;
    let symbol = symbol_at_position(&ctx, &source_text, &source_rel, position)?;

    let mut locations = Vec::new();
    for rel_path in files_for_search(&ctx, &source_rel) {
        let abs = ctx.collection.root.join(&rel_path);
        let Ok(file_uri) = Url::from_file_path(&abs) else {
            continue;
        };
        let text = state
            .document_text(&file_uri)
            .or_else(|| std::fs::read_to_string(&abs).ok())
            .unwrap_or_default();
        let refs = find_references_in_text(&ctx, &text, &rel_path, &symbol.target);
        locations.extend(refs.into_iter().map(|r| Location {
            uri: file_uri.clone(),
            range: r.range,
        }));
    }

    if !params.context.include_declaration {
        locations.retain(|loc| !(loc.uri == *uri && loc.range == symbol.range));
    }
    Some(locations)
}

pub(crate) fn prepare_rename(
    state: &BackendState,
    params: TextDocumentPositionParams,
) -> Option<PrepareRenameResponse> {
    let uri = &params.text_document.uri;
    let position = params.position;
    let source_text = state.document_text(uri)?;
    let (ctx, source_rel) = state.context_and_rel_path_for_uri(uri)?;
    let symbol = symbol_at_position(&ctx, &source_text, &source_rel, position)?;
    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: symbol.range,
        placeholder: symbol.target,
    })
}

pub(crate) fn rename(state: &BackendState, params: RenameParams) -> Option<WorkspaceEdit> {
    let uri = &params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let source_text = state.document_text(uri)?;
    let (ctx, source_rel) = state.context_and_rel_path_for_uri(uri)?;
    let symbol = symbol_at_position(&ctx, &source_text, &source_rel, position)?;

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for rel_path in files_for_search(&ctx, &source_rel) {
        let abs = ctx.collection.root.join(&rel_path);
        let Ok(file_uri) = Url::from_file_path(&abs) else {
            continue;
        };
        let text = state
            .document_text(&file_uri)
            .or_else(|| std::fs::read_to_string(&abs).ok())
            .unwrap_or_default();

        let refs = find_references_in_text(&ctx, &text, &rel_path, &symbol.target);
        if refs.is_empty() {
            continue;
        }
        let edits = refs
            .into_iter()
            .map(|r| TextEdit {
                range: r.range,
                new_text: replacement_for_ref(&r, &params.new_name),
            })
            .collect::<Vec<_>>();
        changes.insert(file_uri, edits);
    }

    Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

#[derive(Debug, Clone)]
struct SymbolAtCursor {
    target: String,
    range: Range,
}

#[derive(Debug, Clone)]
struct FoundRef {
    range: Range,
    format: RefFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RefFormat {
    Wikilink {
        alias: Option<String>,
        anchor: Option<String>,
    },
    Markdown {
        label: Option<String>,
        anchor: Option<String>,
    },
    Frontmatter(FrontmatterStyle),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FrontmatterStyle {
    Wikilink {
        alias: Option<String>,
        anchor: Option<String>,
        quote: Option<char>,
    },
    Markdown {
        label: String,
        anchor: Option<String>,
        quote: Option<char>,
    },
    Bare {
        anchor: Option<String>,
        quote: Option<char>,
    },
}

fn symbol_at_position(
    ctx: &CollectionContext,
    text: &str,
    source_rel: &str,
    position: Position,
) -> Option<SymbolAtCursor> {
    let line = position.line as usize;
    let col = position.character as usize;

    if let Some(link) = body_links::body_link_at(text, line, col) {
        let rel = ctx.file_index.resolve_target_rel_path(
            &ctx.collection,
            &link.target,
            Some(source_rel),
        )?;
        return Some(SymbolAtCursor {
            target: rel,
            range: Range {
                start: Position::new(link.start_line as u32, link.start_col as u32),
                end: Position::new(link.end_line as u32, link.end_col as u32),
            },
        });
    }

    if !text::is_in_frontmatter(text, line) {
        return None;
    }
    let line_text = text.lines().nth(line)?;
    let value = text::frontmatter_value_at_column(line_text, col)?;
    let (parsed_target, _) = parse_frontmatter_link_syntax(&value.value)?;
    let rel = ctx.file_index.resolve_target_rel_path(
        &ctx.collection,
        &parsed_target,
        Some(source_rel),
    )?;

    Some(SymbolAtCursor {
        target: rel,
        range: Range {
            start: Position::new(line as u32, value.start_col as u32),
            end: Position::new(line as u32, value.end_col as u32),
        },
    })
}

fn find_references_in_text(
    ctx: &CollectionContext,
    text: &str,
    source_rel: &str,
    target_rel: &str,
) -> Vec<FoundRef> {
    let mut refs = Vec::new();
    for link in body_links::find_body_links(text) {
        let rel =
            ctx.file_index
                .resolve_target_rel_path(&ctx.collection, &link.target, Some(source_rel));
        if rel.as_deref() == Some(target_rel) {
            refs.push(FoundRef {
                range: Range {
                    start: Position::new(link.start_line as u32, link.start_col as u32),
                    end: Position::new(link.end_line as u32, link.end_col as u32),
                },
                format: match link.format {
                    LinkFormat::Wikilink => RefFormat::Wikilink {
                        alias: link.alias.clone(),
                        anchor: link.anchor.clone(),
                    },
                    LinkFormat::Markdown => RefFormat::Markdown {
                        label: link.alias.clone(),
                        anchor: link.anchor.clone(),
                    },
                },
            });
        }
    }

    if let Some((start, end)) = text::frontmatter_bounds(text) {
        for (line_idx, line_text) in text.lines().enumerate() {
            if line_idx < start || line_idx > end {
                continue;
            }
            if let Some(value) = text::frontmatter_value_at_column(line_text, line_text.len()) {
                if let Some((parsed_target, style)) = parse_frontmatter_link_syntax(&value.value) {
                    let rel = ctx.file_index.resolve_target_rel_path(
                        &ctx.collection,
                        &parsed_target,
                        Some(source_rel),
                    );
                    if rel.as_deref() == Some(target_rel) {
                        refs.push(FoundRef {
                            range: Range {
                                start: Position::new(line_idx as u32, value.start_col as u32),
                                end: Position::new(line_idx as u32, value.end_col as u32),
                            },
                            format: RefFormat::Frontmatter(style),
                        });
                    }
                }
            }
        }
    }

    refs
}

fn replacement_for_ref(found: &FoundRef, new_target: &str) -> String {
    match &found.format {
        RefFormat::Wikilink { alias, anchor } => {
            let mut s = new_target.to_string();
            if let Some(anchor) = anchor {
                s.push('#');
                s.push_str(anchor);
            }
            if let Some(alias) = alias {
                format!("[[{}|{}]]", s, alias)
            } else {
                format!("[[{}]]", s)
            }
        }
        RefFormat::Markdown { label, anchor } => {
            let mut path = new_target.to_string();
            if let Some(anchor) = anchor {
                path.push('#');
                path.push_str(anchor);
            }
            let label = label.clone().unwrap_or_default();
            format!("[{}]({})", label, path)
        }
        RefFormat::Frontmatter(style) => frontmatter_replacement(style.clone(), new_target),
    }
}

fn frontmatter_replacement(style: FrontmatterStyle, new_target: &str) -> String {
    match style {
        FrontmatterStyle::Wikilink {
            alias,
            anchor,
            quote,
        } => {
            let mut target = new_target.to_string();
            if let Some(anchor) = anchor {
                target.push('#');
                target.push_str(&anchor);
            }
            let value = if let Some(alias) = alias {
                format!("[[{}|{}]]", target, alias)
            } else {
                format!("[[{}]]", target)
            };
            wrap_quote(value, quote)
        }
        FrontmatterStyle::Markdown {
            label,
            anchor,
            quote,
        } => {
            let mut path = new_target.to_string();
            if let Some(anchor) = anchor {
                path.push('#');
                path.push_str(&anchor);
            }
            wrap_quote(format!("[{}]({})", label, path), quote)
        }
        FrontmatterStyle::Bare { anchor, quote } => {
            let mut value = new_target.to_string();
            if let Some(anchor) = anchor {
                value.push('#');
                value.push_str(&anchor);
            }
            wrap_quote(value, quote)
        }
    }
}

fn parse_frontmatter_link_syntax(value: &str) -> Option<(String, FrontmatterStyle)> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (unquoted, quote) = strip_wrapping_quotes(trimmed);

    // Wikilink value
    if unquoted.starts_with("[[") && unquoted.ends_with("]]") {
        let inner = &unquoted[2..unquoted.len() - 2];
        let (target_part, alias) = if let Some(pipe_idx) = inner.find('|') {
            (
                inner[..pipe_idx].trim(),
                Some(inner[pipe_idx + 1..].trim().to_string()).filter(|s| !s.is_empty()),
            )
        } else {
            (inner.trim(), None)
        };
        let (target, anchor) = split_anchor(target_part);
        if target.is_empty() {
            return None;
        }
        return Some((
            target.clone(),
            FrontmatterStyle::Wikilink {
                alias,
                anchor,
                quote,
            },
        ));
    }

    // Markdown link value
    if unquoted.starts_with('[') {
        if let Some(close_idx) = unquoted.find("](") {
            if unquoted.ends_with(')') {
                let label = unquoted[1..close_idx].to_string();
                let path = unquoted[close_idx + 2..unquoted.len() - 1].trim();
                if path.starts_with("http://") || path.starts_with("https://") {
                    return None;
                }
                let (target, anchor) = split_anchor(path);
                if target.is_empty() {
                    return None;
                }
                return Some((
                    target.clone(),
                    FrontmatterStyle::Markdown {
                        label,
                        anchor,
                        quote,
                    },
                ));
            }
        }
    }

    if unquoted.starts_with("http://") || unquoted.starts_with("https://") {
        return None;
    }

    let (target, anchor) = split_anchor(unquoted);
    if target.is_empty() {
        return None;
    }
    Some((target.clone(), FrontmatterStyle::Bare { anchor, quote }))
}

fn split_anchor(s: &str) -> (String, Option<String>) {
    if let Some(idx) = s.find('#') {
        let target = s[..idx].trim().to_string();
        let anchor = s[idx + 1..].trim().to_string();
        return (
            target,
            if anchor.is_empty() {
                None
            } else {
                Some(anchor)
            },
        );
    }
    (s.trim().to_string(), None)
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

fn wrap_quote(value: String, quote: Option<char>) -> String {
    if let Some(q) = quote {
        format!("{}{}{}", q, value, q)
    } else {
        value
    }
}

fn files_for_search(ctx: &CollectionContext, source_rel: &str) -> Vec<String> {
    let mut files = ctx.file_index.all_rel_paths();
    if !files.iter().any(|p| p == source_rel) {
        files.push(source_rel.to_string());
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_wikilink_preserves_alias_and_anchor() {
        let found = FoundRef {
            range: Range::default(),
            format: RefFormat::Wikilink {
                alias: Some("Alias".to_string()),
                anchor: Some("section".to_string()),
            },
        };
        assert_eq!(
            replacement_for_ref(&found, "notes/new"),
            "[[notes/new#section|Alias]]"
        );
    }

    #[test]
    fn replacement_markdown_preserves_label() {
        let found = FoundRef {
            range: Range::default(),
            format: RefFormat::Markdown {
                label: Some("Read".to_string()),
                anchor: None,
            },
        };
        assert_eq!(
            replacement_for_ref(&found, "notes/new.md"),
            "[Read](notes/new.md)"
        );
    }

    #[test]
    fn replacement_frontmatter_preserves_wikilink_shape() {
        let found = FoundRef {
            range: Range::default(),
            format: RefFormat::Frontmatter(FrontmatterStyle::Wikilink {
                alias: Some("Alias".to_string()),
                anchor: Some("section".to_string()),
                quote: Some('"'),
            }),
        };
        assert_eq!(
            replacement_for_ref(&found, "notes/new"),
            "\"[[notes/new#section|Alias]]\""
        );
    }

    #[test]
    fn parse_frontmatter_markdown_style() {
        let parsed = parse_frontmatter_link_syntax("'[Read](notes/old.md#h)'").unwrap();
        assert_eq!(parsed.0, "notes/old.md");
        assert_eq!(
            frontmatter_replacement(parsed.1, "notes/new.md"),
            "'[Read](notes/new.md#h)'"
        );
    }
}
