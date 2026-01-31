use tower_lsp::lsp_types::*;

use crate::collection_utils;
use crate::state::BackendState;
use crate::text;

/// Provide go-to-definition for the given position.
///
/// TODO: Implement:
/// - Link → target file (wikilink, markdown link, bare path)
/// - Type name in frontmatter → _types/ definition file
pub fn definition(
    state: &BackendState,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let collection = state.get_collection()?;
    let text = state.document_text(uri)?;
    let line_idx = position.line as usize;
    let line_text = text.lines().nth(line_idx).unwrap_or("").to_string();
    let column = position.character as usize;

    if text::is_in_frontmatter(&text, line_idx) {
        let parsed = text::parse_frontmatter(&text);
        if parsed.parse_error || parsed.mapping_error {
            return None;
        }
        let rel_path = uri.to_file_path().ok()
            .and_then(|p| p.strip_prefix(&collection.root).ok().map(|r| r.to_string_lossy().to_string().replace('\\', "/")));
        let type_names = collection.determine_types_for_path(&parsed.json, rel_path.as_deref());

        if let Some(field_name) = text::field_name_from_line(&line_text) {
            let colon_idx = line_text.find(':').unwrap_or(0);
            if column > colon_idx {
                if is_link_field(&collection, &type_names, &field_name) {
                    if let Some(rel_path) = rel_path {
                        let resolved = collection.resolve_link(&serde_json::json!({
                            "path": rel_path,
                            "field": field_name,
                        }));
                        if let Some(target) = resolved.get("resolved_path").and_then(|v| v.as_str()) {
                            let target_path = collection.root.join(target);
                            if let Ok(target_uri) = Url::from_file_path(&target_path) {
                                let location = Location::new(target_uri, Range::new(Position::new(0, 0), Position::new(0, 0)));
                                return Some(GotoDefinitionResponse::Scalar(location));
                            }
                        }
                    }
                }

                if (field_name == "type" || field_name == "types")
                    && text::word_at(&line_text, column).is_some()
                {
                    if let Some(type_name) = text::word_at(&line_text, column) {
                        if let Some(type_path) = collection_utils::find_type_definition_path(&collection, &type_name) {
                            if let Ok(type_uri) = Url::from_file_path(&type_path) {
                                let location = Location::new(type_uri, Range::new(Position::new(0, 0), Position::new(0, 0)));
                                return Some(GotoDefinitionResponse::Scalar(location));
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

fn is_link_field(
    collection: &mdbase::Collection,
    type_names: &[String],
    field_name: &str,
) -> bool {
    if type_names.is_empty() {
        for type_def in collection.types.values() {
            if let Some(def) = type_def.fields.get(field_name) {
                if def.field_type == "link" {
                    return true;
                }
                if def.field_type == "list" {
                    if let Some(item) = &def.items {
                        if item.field_type == "link" {
                            return true;
                        }
                    }
                }
            }
        }
        false
    } else {
        for type_name in type_names {
            if let Some(type_def) = collection.types.get(type_name) {
                if let Some(def) = type_def.fields.get(field_name) {
                    if def.field_type == "link" {
                        return true;
                    }
                    if def.field_type == "list" {
                        if let Some(item) = &def.items {
                            if item.field_type == "link" {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }
}
