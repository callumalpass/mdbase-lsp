use std::path::PathBuf;
use std::sync::Arc;

use tower_lsp::lsp_types::*;
use tracing::debug;

use crate::collection_utils;
use crate::state::BackendState;
use crate::text;

/// Provide go-to-definition for the given position.
///
/// Handles:
/// - Body links: `[[wikilinks]]`, `[text](path)`, `![[embeds]]`, `![img](path)`
/// - Frontmatter type/types fields → `_types/` definition file
/// - Frontmatter link-type fields → resolved target file
/// - Frontmatter list items under link-type fields
pub fn definition(
    state: &BackendState,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let collection = state.get_collection()?;
    let text = state.document_text(uri)?;
    let line_idx = position.line as usize;
    let column = position.character as usize;

    let rel_path = uri
        .to_file_path()
        .ok()
        .and_then(|p| {
            p.strip_prefix(&collection.root)
                .ok()
                .map(|r| r.to_string_lossy().to_string().replace('\\', "/"))
        });

    if text::is_in_frontmatter(&text, line_idx) {
        debug!(line = line_idx, col = column, "goto: cursor in frontmatter");
        definition_in_frontmatter(state, uri, &collection, &text, line_idx, column, rel_path.as_deref())
    } else {
        debug!(line = line_idx, col = column, "goto: cursor in body");
        definition_in_body(&collection, &text, line_idx, column, rel_path.as_deref())
    }
}

/// Handle go-to-definition for a cursor position in the document body.
fn definition_in_body(
    collection: &Arc<mdbase::Collection>,
    text: &str,
    line_idx: usize,
    column: usize,
    rel_path: Option<&str>,
) -> Option<GotoDefinitionResponse> {
    let link = text::link_at_position(text, line_idx, column)?;
    debug!(target = %link.target, "goto body: found link at cursor");
    let resolved = collection_utils::resolve_link_target(collection, &link.target, rel_path)?;
    make_location_response(&resolved)
}

/// Handle go-to-definition for a cursor position in the frontmatter.
fn definition_in_frontmatter(
    state: &BackendState,
    uri: &Url,
    collection: &Arc<mdbase::Collection>,
    text: &str,
    line_idx: usize,
    column: usize,
    rel_path: Option<&str>,
) -> Option<GotoDefinitionResponse> {
    // 1. Check if the cursor is on an inline link (wikilink/markdown link in a FM value)
    if let Some(link) = text::link_at_position(text, line_idx, column) {
        debug!(target = %link.target, "goto fm: inline link at cursor");
        if let Some(resolved) = collection_utils::resolve_link_target(collection, &link.target, rel_path) {
            return make_location_response(&resolved);
        }
    }

    // 2. Determine the field name (handles both `field: value` and list items)
    let field_name = text::field_name_for_position(text, line_idx)?;
    debug!(field = %field_name, "goto fm: resolved field name");

    // 3. Determine types for this document
    let parsed = state
        .documents
        .get(uri)
        .map(|doc| doc.frontmatter())
        .unwrap_or_else(|| text::parse_frontmatter(text));
    if parsed.parse_error || parsed.mapping_error {
        debug!("goto fm: frontmatter parse error, bailing");
        return None;
    }
    let type_names = collection.determine_types_for_path(&parsed.json, rel_path);

    // 4. Type/types field → jump to type definition
    if field_name == "type" || field_name == "types" {
        if let Some(word) = text::word_at(text.lines().nth(line_idx).unwrap_or(""), column) {
            debug!(type_name = %word, "goto fm: looking up type definition");
            if let Some(type_path) = collection_utils::find_type_definition_path(collection, &word) {
                return make_location_response(&type_path);
            }
        }
        return None;
    }

    // 5. Link-type field → resolve the value as a link target
    if is_link_field(collection, &type_names, &field_name) {
        let line_text = text.lines().nth(line_idx).unwrap_or("");
        if let Some(value) = text::value_from_frontmatter_line(line_text, column) {
            debug!(value = %value, "goto fm: link field value");
            let target = collection_utils::parse_link_value(&value).unwrap_or(value);
            debug!(target = %target, "goto fm: parsed link target");
            if let Some(resolved) = collection_utils::resolve_link_target(collection, &target, rel_path) {
                return make_location_response(&resolved);
            }
        }
    }

    None
}

/// Build a `GotoDefinitionResponse::Scalar` pointing to line 0 of the given path.
fn make_location_response(path: &PathBuf) -> Option<GotoDefinitionResponse> {
    let target_uri = Url::from_file_path(path).ok()?;
    let location = Location::new(
        target_uri,
        Range::new(Position::new(0, 0), Position::new(0, 0)),
    );
    Some(GotoDefinitionResponse::Scalar(location))
}

/// Check whether `field_name` is a link-type field for any of the given types
/// (or any type at all, when `type_names` is empty).
fn is_link_field(
    collection: &mdbase::Collection,
    type_names: &[String],
    field_name: &str,
) -> bool {
    let types_to_check: Vec<&mdbase::types::schema::TypeDef> = if type_names.is_empty() {
        collection.types.values().collect()
    } else {
        type_names
            .iter()
            .filter_map(|n| collection.types.get(n))
            .collect()
    };

    for type_def in types_to_check {
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
}
