use tower_lsp::lsp_types::*;

use mdbase::types::schema::FieldDef;

use crate::collection_utils;
use crate::state::BackendState;
use crate::text;

pub(crate) fn provide(
    state: &BackendState,
    params: CodeActionParams,
) -> Option<CodeActionResponse> {
    let collection = state.get_collection()?;
    let uri = &params.text_document.uri;
    let doc_text = state.document_text(uri)?;
    let rel_path = collection_utils::rel_path_from_uri(&collection, uri)?;

    let parsed = state
        .documents
        .get(uri)
        .map(|d| d.frontmatter())
        .unwrap_or_else(|| text::parse_frontmatter(&doc_text));
    if parsed.parse_error || parsed.mapping_error {
        return None;
    }
    let type_names = collection.determine_types_for_path(&parsed.json, Some(&rel_path));

    let mut actions = Vec::new();
    for diagnostic in &params.context.diagnostics {
        if diagnostic.source.as_deref() != Some("mdbase") {
            continue;
        }
        let field = diagnostic
            .data
            .as_ref()
            .and_then(|d| d.get("field"))
            .and_then(|v| v.as_str());
        let Some(field_name) = field else {
            continue;
        };

        // Always offer to insert missing field.
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("Add field '{}'", field_name),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            edit: Some(workspace_edit_for(
                uri.clone(),
                field_edit(&doc_text, field_name, ""),
            )),
            is_preferred: Some(false),
            ..Default::default()
        }));

        if let Some(def) = field_def_for_types(&collection, &type_names, field_name) {
            if let Some(values) = &def.values {
                for value in values {
                    let new_text = yaml_value_text(field_name, value);
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: format!("Set '{}' to '{}'", field_name, value),
                        kind: Some(CodeActionKind::QUICKFIX),
                        diagnostics: Some(vec![diagnostic.clone()]),
                        edit: Some(workspace_edit_for(
                            uri.clone(),
                            field_edit(&doc_text, field_name, &new_text),
                        )),
                        is_preferred: Some(true),
                        ..Default::default()
                    }));
                }
            } else if def.field_type == "boolean" {
                for value in ["true", "false"] {
                    let new_text = yaml_value_text(field_name, value);
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: format!("Set '{}' to {}", field_name, value),
                        kind: Some(CodeActionKind::QUICKFIX),
                        diagnostics: Some(vec![diagnostic.clone()]),
                        edit: Some(workspace_edit_for(
                            uri.clone(),
                            field_edit(&doc_text, field_name, &new_text),
                        )),
                        is_preferred: Some(false),
                        ..Default::default()
                    }));
                }
            }
        }
    }

    if actions.is_empty() {
        None
    } else {
        Some(actions)
    }
}

fn workspace_edit_for(uri: Url, edit: TextEdit) -> WorkspaceEdit {
    WorkspaceEdit {
        changes: Some(std::collections::HashMap::from([(uri, vec![edit])])),
        ..Default::default()
    }
}

fn field_edit(text: &str, field_name: &str, replacement_line: &str) -> TextEdit {
    let fallback_line = text::frontmatter_bounds(text)
        .map(|(start, _)| start)
        .unwrap_or(0);
    let (start, end) = text::find_field_range(text, field_name, fallback_line);
    if start.line == end.line && start.character == end.character {
        // No existing field: insert before closing frontmatter, or at top.
        if let Some((_, fm_end)) = text::frontmatter_bounds(text) {
            return TextEdit {
                range: Range::new(
                    Position::new((fm_end + 1) as u32, 0),
                    Position::new((fm_end + 1) as u32, 0),
                ),
                new_text: if replacement_line.is_empty() {
                    format!("{}: \n", field_name)
                } else {
                    format!("{}\n", replacement_line)
                },
            };
        }
        return TextEdit {
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            new_text: if replacement_line.is_empty() {
                format!("---\n{}: \n---\n", field_name)
            } else {
                format!("---\n{}\n---\n", replacement_line)
            },
        };
    }

    let line_idx = start.line as usize;
    let line_text = text.lines().nth(line_idx).unwrap_or("");
    TextEdit {
        range: Range::new(
            Position::new(line_idx as u32, 0),
            Position::new(line_idx as u32, line_text.len() as u32),
        ),
        new_text: if replacement_line.is_empty() {
            format!("{}: ", field_name)
        } else {
            replacement_line.to_string()
        },
    }
}

fn yaml_value_text(field: &str, value: &str) -> String {
    format!("{}: {}", field, value)
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
        return None;
    }
    for type_name in type_names {
        if let Some(type_def) = collection.types.get(type_name) {
            if let Some(def) = type_def.fields.get(field_name) {
                return Some(def.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_field_into_existing_frontmatter() {
        let text = "---\ntitle: Demo\n---\nBody\n";
        let edit = field_edit(text, "status", "");
        assert_eq!(edit.range.start.line, 2);
        assert_eq!(edit.new_text, "status: \n");
    }

    #[test]
    fn replace_existing_field_line() {
        let text = "---\nstatus: old\n---\n";
        let edit = field_edit(text, "status", "status: new");
        assert_eq!(edit.range.start.line, 1);
        assert_eq!(edit.new_text, "status: new");
    }
}
