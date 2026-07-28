use std::path::PathBuf;

use tower_lsp::lsp_types::*;
use tracing::debug;

use crate::state::{BackendState, CollectionContext};
use crate::text;

/// Provide go-to-definition for the given position.
///
/// Handles:
/// - Body links: `[[wikilinks]]`, `[text](path)`, `![[embeds]]`, `![img](path)`
/// - Frontmatter type/types fields → `_types/` definition file
/// - Data contract IDs in a type's `implements` list → `_contracts/` definition file
/// - Frontmatter link-type fields → resolved target file
/// - Frontmatter list items under link-type fields
pub fn definition(
    state: &BackendState,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let (ctx, rel_path) = state.context_and_rel_path_for_uri(uri)?;
    let text = state.document_text(uri)?;
    let line_idx = position.line as usize;
    let column = position.character as usize;

    if text::is_in_frontmatter(&text, line_idx) {
        debug!(line = line_idx, col = column, "goto: cursor in frontmatter");
        definition_in_frontmatter(state, uri, &ctx, &text, line_idx, column, &rel_path)
    } else {
        debug!(line = line_idx, col = column, "goto: cursor in body");
        definition_in_body(&ctx, &text, line_idx, column, &rel_path)
    }
}

/// Handle go-to-definition for a cursor position in the document body.
fn definition_in_body(
    ctx: &CollectionContext,
    text: &str,
    line_idx: usize,
    column: usize,
    rel_path: &str,
) -> Option<GotoDefinitionResponse> {
    // Use body_links parser (respects fenced code blocks and inline code spans)
    if let Some(link) = crate::body_links::body_link_at(text, line_idx, column) {
        debug!(target = %link.target, "goto body: found body link at cursor");
        let resolved = ctx.file_index.resolve_target_abs_path(
            &ctx.collection,
            &link.target,
            Some(rel_path),
        )?;
        return make_location_response(&resolved);
    }
    None
}

/// Handle go-to-definition for a cursor position in the frontmatter.
fn definition_in_frontmatter(
    state: &BackendState,
    uri: &Url,
    ctx: &CollectionContext,
    text: &str,
    line_idx: usize,
    column: usize,
    rel_path: &str,
) -> Option<GotoDefinitionResponse> {
    // 1. Check if the cursor is on an inline link (wikilink/markdown link in a FM value)
    if let Some(link) = text::link_at_position(text, line_idx, column) {
        debug!(target = %link.target, "goto fm: inline link at cursor");
        if let Some(resolved) =
            ctx.file_index
                .resolve_target_abs_path(&ctx.collection, &link.target, Some(rel_path))
        {
            return make_location_response(&resolved);
        }
    }

    // 2. Determine the field name (handles both `field: value` and list items)
    let field_name = text::field_name_for_position(text, line_idx)?;
    debug!(field = %field_name, "goto fm: resolved field name");

    if field_name == "contract"
        && rel_path.starts_with(&format!("{}/", ctx.collection.settings().types_folder))
    {
        let line_text = text.lines().nth(line_idx).unwrap_or("");
        if let Some(contract_id) = text::value_from_frontmatter_line(line_text, column) {
            if let Some(contract_path) = crate::collection_utils::find_data_contract_definition_path(
                &ctx.collection,
                contract_id.trim_matches(['\'', '"']),
            ) {
                return make_location_response(&contract_path);
            }
        }
        return None;
    }

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
    let type_names = ctx
        .collection
        .determine_types_for_path(&parsed.json, Some(rel_path));

    // 4. Type/types field → jump to type definition
    if field_name == "type" || field_name == "types" {
        if let Some(word) = text::word_at(text.lines().nth(line_idx).unwrap_or(""), column) {
            debug!(type_name = %word, "goto fm: looking up type definition");
            if let Some(type_path) =
                crate::collection_utils::find_type_definition_path(&ctx.collection, &word)
            {
                return make_location_response(&type_path);
            }
        }
        return None;
    }

    // 5. Link-type field → resolve the value as a link target
    if is_link_field(&ctx.collection, &type_names, &field_name) {
        let line_text = text.lines().nth(line_idx).unwrap_or("");
        if let Some(value) = text::value_from_frontmatter_line(line_text, column) {
            debug!(value = %value, "goto fm: link field value");
            let target = crate::collection_utils::parse_link_value(&value).unwrap_or(value);
            debug!(target = %target, "goto fm: parsed link target");
            if let Some(resolved) =
                ctx.file_index
                    .resolve_target_abs_path(&ctx.collection, &target, Some(rel_path))
            {
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
fn is_link_field(collection: &mdbase::Collection, type_names: &[String], field_name: &str) -> bool {
    let types_to_check: Vec<&mdbase::types::schema::TypeDef> = if type_names.is_empty() {
        collection.types().values().collect()
    } else {
        type_names
            .iter()
            .filter_map(|n| collection.types().get(n))
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

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::state::DocumentState;

    use super::*;

    #[test]
    fn contract_implementation_navigates_to_its_definition() {
        let directory = tempfile::tempdir().expect("temp collection");
        fs::write(
            directory.path().join("mdbase.yaml"),
            "spec_version: \"0.3.0\"\n",
        )
        .unwrap();
        fs::create_dir_all(directory.path().join("_contracts")).unwrap();
        fs::create_dir_all(directory.path().join("_types")).unwrap();
        fs::write(
            directory.path().join("_contracts/example.note.md"),
            r#"---
kind: mdbase.contract
contract_type: record
id: example.note
version: 1.0.0
record_schema:
  dialect: json-schema-2020-12
  value:
    type: object
    required: [title]
    properties:
      title: { type: string }
---
"#,
        )
        .unwrap();
        let type_text = r#"---
kind: mdbase.type
name: note
schema:
  dialect: json-schema-2020-12
  value:
    type: object
    required: [title]
    properties:
      title: { type: string }
implements:
  - contract: example.note
    version: 1.0.0
    fields:
      title: title
---
"#;
        let type_path = directory.path().join("_types/note.md");
        fs::write(&type_path, type_text).unwrap();

        let state = BackendState::new();
        state.set_workspace_roots(vec![directory.path().to_path_buf()]);
        let uri = Url::from_file_path(type_path).unwrap();
        state.documents.insert(
            uri.clone(),
            DocumentState::new(ropey::Rope::from_str(type_text)),
        );
        let line = type_text
            .lines()
            .position(|line| line.contains("contract:"))
            .unwrap() as u32;

        let response = definition(&state, &uri, Position::new(line, 20)).unwrap();
        let GotoDefinitionResponse::Scalar(location) = response else {
            panic!("expected scalar definition");
        };
        assert_eq!(
            location.uri,
            Url::from_file_path(directory.path().join("_contracts/example.note.md")).unwrap()
        );
    }
}
