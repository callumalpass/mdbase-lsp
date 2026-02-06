use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::body_links::{self, LinkFormat};
use crate::collection_utils;
use crate::state::BackendState;
use crate::text;

pub(crate) fn provide(state: &BackendState, params: ReferenceParams) -> Option<Vec<Location>> {
    let collection = state.get_collection()?;
    let uri = &params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let source_text = state.document_text(uri)?;
    let source_rel = collection_utils::rel_path_from_uri(&collection, uri)?;
    let symbol = symbol_at_position(&collection, &source_text, &source_rel, position)?;

    let mut locations = Vec::new();
    let files = collection_utils::scan_collection_files(&collection);
    for path in files {
        let Ok(file_uri) = Url::from_file_path(&path) else {
            continue;
        };
        let rel_path = match path.strip_prefix(&collection.root) {
            Ok(p) => p.to_string_lossy().to_string().replace('\\', "/"),
            Err(_) => continue,
        };
        let text = state
            .document_text(&file_uri)
            .or_else(|| std::fs::read_to_string(&path).ok())
            .unwrap_or_default();
        let refs = find_references_in_text(&collection, &text, &rel_path, &symbol.target);
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
    let collection = state.get_collection()?;
    let uri = &params.text_document.uri;
    let position = params.position;
    let source_text = state.document_text(uri)?;
    let source_rel = collection_utils::rel_path_from_uri(&collection, uri)?;
    let symbol = symbol_at_position(&collection, &source_text, &source_rel, position)?;
    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: symbol.range,
        placeholder: symbol.target,
    })
}

pub(crate) fn rename(state: &BackendState, params: RenameParams) -> Option<WorkspaceEdit> {
    let collection = state.get_collection()?;
    let uri = &params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let source_text = state.document_text(uri)?;
    let source_rel = collection_utils::rel_path_from_uri(&collection, uri)?;
    let symbol = symbol_at_position(&collection, &source_text, &source_rel, position)?;

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    let files = collection_utils::scan_collection_files(&collection);
    for path in files {
        let Ok(file_uri) = Url::from_file_path(&path) else {
            continue;
        };
        let rel_path = match path.strip_prefix(&collection.root) {
            Ok(p) => p.to_string_lossy().to_string().replace('\\', "/"),
            Err(_) => continue,
        };
        let text = state
            .document_text(&file_uri)
            .or_else(|| std::fs::read_to_string(&path).ok())
            .unwrap_or_default();

        let refs = find_references_in_text(&collection, &text, &rel_path, &symbol.target);
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
    alias: Option<String>,
    anchor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefFormat {
    Wikilink,
    Markdown,
    FrontmatterValue,
}

fn symbol_at_position(
    collection: &mdbase::Collection,
    text: &str,
    source_rel: &str,
    position: Position,
) -> Option<SymbolAtCursor> {
    let line = position.line as usize;
    let col = position.character as usize;

    if let Some(link) = body_links::body_link_at(text, line, col) {
        let resolved =
            collection_utils::resolve_link_target(collection, &link.target, Some(source_rel))?;
        let rel = resolved
            .strip_prefix(&collection.root)
            .ok()
            .map(|p| p.to_string_lossy().to_string().replace('\\', "/"))?;
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
    let value = text::value_from_frontmatter_line(line_text, col)?;
    let parsed = collection_utils::parse_link_value(&value)?;
    let resolved = collection_utils::resolve_link_target(collection, &parsed, Some(source_rel))?;
    let rel = resolved
        .strip_prefix(&collection.root)
        .ok()
        .map(|p| p.to_string_lossy().to_string().replace('\\', "/"))?;

    Some(SymbolAtCursor {
        target: rel,
        range: Range {
            start: Position::new(line as u32, 0),
            end: Position::new(line as u32, line_text.len() as u32),
        },
    })
}

fn find_references_in_text(
    collection: &mdbase::Collection,
    text: &str,
    source_rel: &str,
    target_rel: &str,
) -> Vec<FoundRef> {
    let mut refs = Vec::new();
    for link in body_links::find_body_links(text) {
        if let Some(resolved) =
            collection_utils::resolve_link_target(collection, &link.target, Some(source_rel))
        {
            let rel = resolved
                .strip_prefix(&collection.root)
                .ok()
                .map(|p| p.to_string_lossy().to_string().replace('\\', "/"));
            if rel.as_deref() == Some(target_rel) {
                refs.push(FoundRef {
                    range: Range {
                        start: Position::new(link.start_line as u32, link.start_col as u32),
                        end: Position::new(link.end_line as u32, link.end_col as u32),
                    },
                    format: match link.format {
                        LinkFormat::Wikilink => RefFormat::Wikilink,
                        LinkFormat::Markdown => RefFormat::Markdown,
                    },
                    alias: link.alias.clone(),
                    anchor: link.anchor.clone(),
                });
            }
        }
    }

    if let Some((start, end)) = text::frontmatter_bounds(text) {
        for (line_idx, line_text) in text.lines().enumerate() {
            if line_idx < start || line_idx > end {
                continue;
            }
            if let Some(value) = text::value_from_frontmatter_line(line_text, line_text.len()) {
                if let Some(parsed) = collection_utils::parse_link_value(&value) {
                    if let Some(resolved) =
                        collection_utils::resolve_link_target(collection, &parsed, Some(source_rel))
                    {
                        let rel = resolved
                            .strip_prefix(&collection.root)
                            .ok()
                            .map(|p| p.to_string_lossy().to_string().replace('\\', "/"));
                        if rel.as_deref() == Some(target_rel) {
                            refs.push(FoundRef {
                                range: Range {
                                    start: Position::new(line_idx as u32, 0),
                                    end: Position::new(line_idx as u32, line_text.len() as u32),
                                },
                                format: RefFormat::FrontmatterValue,
                                alias: None,
                                anchor: None,
                            });
                        }
                    }
                }
            }
        }
    }

    refs
}

fn replacement_for_ref(found: &FoundRef, new_target: &str) -> String {
    match found.format {
        RefFormat::Wikilink => {
            let mut s = new_target.to_string();
            if let Some(anchor) = &found.anchor {
                s.push('#');
                s.push_str(anchor);
            }
            if let Some(alias) = &found.alias {
                format!("[[{}|{}]]", s, alias)
            } else {
                format!("[[{}]]", s)
            }
        }
        RefFormat::Markdown => {
            let mut path = new_target.to_string();
            if let Some(anchor) = &found.anchor {
                path.push('#');
                path.push_str(anchor);
            }
            let label = found.alias.clone().unwrap_or_else(|| "link".to_string());
            format!("[{}]({})", label, path)
        }
        RefFormat::FrontmatterValue => new_target.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_wikilink_preserves_alias_and_anchor() {
        let found = FoundRef {
            range: Range::default(),
            format: RefFormat::Wikilink,
            alias: Some("Alias".to_string()),
            anchor: Some("section".to_string()),
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
            format: RefFormat::Markdown,
            alias: Some("Read".to_string()),
            anchor: None,
        };
        assert_eq!(
            replacement_for_ref(&found, "notes/new.md"),
            "[Read](notes/new.md)"
        );
    }
}
