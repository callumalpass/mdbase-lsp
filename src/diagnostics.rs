use tower_lsp::lsp_types::*;
use tower_lsp::Client;
use tracing::warn;

use std::collections::{HashMap, HashSet};

use crate::state::BackendState;
use crate::text;

/// Validate the document and publish diagnostics.
pub async fn publish(client: &Client, state: &BackendState, uri: &Url) {
    // Only process markdown files
    if !uri.path().ends_with(".md") {
        return;
    }

    let Some(text) = state.document_text(uri) else {
        warn!(uri = %uri, "diagnostics: no document text");
        return;
    };

    let cached = state.documents.get(uri).map(|doc| doc.frontmatter());
    let diagnostics = if let Some((ctx, rel_path)) = state.context_and_rel_path_for_uri(uri) {
        compute(&ctx.collection, &text, &rel_path, cached)
    } else if let Some(root) = state.root_for_uri(uri) {
        let path = uri.to_file_path().ok();
        let relative = path
            .as_deref()
            .and_then(|path| path.strip_prefix(&root).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        match relative.and_then(|relative| compute_v03_without_collection(&root, &text, &relative))
        {
            Some(diagnostics) => diagnostics,
            None => {
                warn!(uri = %uri, "diagnostics: no collection context available");
                return;
            }
        }
    } else {
        warn!(uri = %uri, "diagnostics: no collection root available");
        return;
    };
    client
        .publish_diagnostics(uri.clone(), diagnostics, None)
        .await;
}

/// Validate the whole collection and publish diagnostics for each affected file.
pub async fn publish_collection(
    client: &Client,
    state: &BackendState,
) -> Option<serde_json::Value> {
    let contexts = state.all_contexts();
    if contexts.is_empty() {
        return None;
    }

    let mut published_now = HashSet::new();
    let mut results = Vec::new();

    for ctx in contexts {
        let (result, issues_key) = if ctx.collection.spec_profile() == mdbase::SpecProfile::V03 {
            let result = ctx
                .collection
                .v03_operations()
                .expect("v0.3 collection provides v0.3 operations")
                .validate(&serde_json::json!({}));
            (
                serde_json::to_value(result).expect("serialize v0.3 operation result"),
                "diagnostics",
            )
        } else {
            (ctx.collection.validate_op(&serde_json::json!({})), "issues")
        };
        let issues = result
            .get(issues_key)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut by_path: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
        for issue in issues {
            let path = issue_path(&issue).unwrap_or_default();
            by_path.entry(path).or_default().push(issue);
        }

        for (rel_path, file_issues) in by_path {
            if rel_path.is_empty() {
                continue;
            }
            let abs = ctx.collection.root().join(&rel_path);
            let Ok(uri) = Url::from_file_path(&abs) else {
                continue;
            };
            let text = if let Some(in_mem) = state.document_text(&uri) {
                in_mem
            } else if let Ok(on_disk) = std::fs::read_to_string(&abs) {
                on_disk
            } else {
                continue;
            };
            let diagnostics = diagnostics_from_issues(&text, file_issues);
            client
                .publish_diagnostics(uri.clone(), diagnostics, None)
                .await;
            published_now.insert(uri);
        }

        results.push(serde_json::json!({
            "root": ctx.collection.root(),
            "result": result,
        }));
    }

    // Clear diagnostics that were published by previous collection validations
    // but no longer appear in current results.
    let previously_published: Vec<Url> = state
        .collection_diagnostics_published
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    for uri in previously_published {
        if !published_now.contains(&uri) {
            client
                .publish_diagnostics(uri.clone(), Vec::new(), None)
                .await;
            state.collection_diagnostics_published.remove(&uri);
        }
    }
    for uri in published_now {
        state.collection_diagnostics_published.insert(uri, ());
    }

    if results.len() == 1 {
        Some(results.remove(0).get("result").cloned().unwrap_or_default())
    } else {
        Some(serde_json::json!({ "collections": results }))
    }
}

/// Compute diagnostics for a document using the owning mdbase collection.
pub(crate) fn compute(
    collection: &mdbase::Collection,
    text: &str,
    rel_path: &str,
    cached: Option<text::ParsedFrontmatter>,
) -> Vec<Diagnostic> {
    let parsed = cached.unwrap_or_else(|| text::parse_frontmatter(text));
    if parsed.parse_error {
        return vec![Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            severity: Some(DiagnosticSeverity::ERROR),
            code: Some(NumberOrString::String("invalid_frontmatter".to_string())),
            source: Some("mdbase".to_string()),
            message: "Failed to parse YAML frontmatter".to_string(),
            ..Default::default()
        }];
    }

    if parsed.mapping_error {
        return vec![Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            severity: Some(DiagnosticSeverity::ERROR),
            code: Some(NumberOrString::String("invalid_frontmatter".to_string())),
            source: Some("mdbase".to_string()),
            message: "Frontmatter must be a YAML mapping".to_string(),
            ..Default::default()
        }];
    }

    if collection.spec_profile() == mdbase::SpecProfile::V03 {
        if is_type_file(collection, rel_path) {
            return diagnostics_from_v03_type_file(collection.root(), text, rel_path);
        }

        let mut diagnostics = collection
            .validate_v03_frontmatter(&parsed.json, rel_path)
            .unwrap_or_default()
            .into_iter()
            .map(|diagnostic| diagnostic_from_v03(text, diagnostic))
            .collect::<Vec<_>>();
        let collection_result = collection
            .v03_operations()
            .expect("v0.3 collection provides v0.3 operations")
            .validate(&serde_json::json!({
                "path": rel_path,
                "frontmatter": parsed.json,
            }));
        let collection_issues = collection_result
            .diagnostics
            .into_iter()
            .filter_map(|diagnostic| serde_json::to_value(diagnostic).ok())
            .filter(|issue| {
                !issue
                    .get("code")
                    .and_then(|value| value.as_str())
                    .is_some_and(|code| code.starts_with("schema_"))
            });
        for issue in collection_issues {
            let candidate = diagnostic_from_issue(
                text,
                text::frontmatter_bounds(text)
                    .map(|(start, _)| start)
                    .unwrap_or(0),
                issue,
            );
            let duplicate = diagnostics.iter().any(|diagnostic| {
                diagnostic.code == candidate.code && diagnostic.range == candidate.range
            });
            if !duplicate {
                diagnostics.push(candidate);
            }
        }
        return diagnostics;
    }

    let result = collection.validate_op(&serde_json::json!({
        "path": rel_path,
        "frontmatter": parsed.json,
    }));

    let issues = result
        .get("issues")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    diagnostics_from_issues(text, issues)
}

fn compute_v03_without_collection(
    root: &std::path::Path,
    text: &str,
    rel_path: &str,
) -> Option<Vec<Diagnostic>> {
    let config = mdbase::config::load_config(root);
    if config
        .pointer("/config/spec_profile")
        .and_then(serde_json::Value::as_str)
        != Some("v0.3")
    {
        return None;
    }
    let types_folder = config
        .pointer("/config/settings/types_folder")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("_types");
    if !rel_path.starts_with(&format!("{types_folder}/")) {
        return None;
    }
    Some(diagnostics_from_v03_type_file(root, text, rel_path))
}

fn is_type_file(collection: &mdbase::Collection, rel_path: &str) -> bool {
    rel_path.starts_with(&format!("{}/", collection.settings().types_folder))
}

fn diagnostics_from_v03_type_file(
    root: &std::path::Path,
    text: &str,
    rel_path: &str,
) -> Vec<Diagnostic> {
    let absolute_path = root.join(rel_path);
    match mdbase::v03::parse_type_file(text, &absolute_path, root, rel_path) {
        Ok(_) => Vec::new(),
        Err(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic_from_v03(text, diagnostic))
            .collect(),
    }
}

fn diagnostic_from_v03(text_value: &str, diagnostic: mdbase::v03::Diagnostic) -> Diagnostic {
    let fallback_line = text::frontmatter_bounds(text_value)
        .map(|(start, _)| start)
        .unwrap_or(0);
    let range = diagnostic
        .field
        .as_deref()
        .map(|field| {
            let field = field.split('.').next().unwrap_or(field);
            let (start, end) = text::find_field_range(text_value, field, fallback_line);
            Range::new(start, end)
        })
        .unwrap_or_else(|| {
            Range::new(
                Position::new(fallback_line as u32, 0),
                Position::new(fallback_line as u32, 0),
            )
        });
    let severity = match diagnostic.severity.as_str() {
        "warning" => DiagnosticSeverity::WARNING,
        "info" => DiagnosticSeverity::INFORMATION,
        _ => DiagnosticSeverity::ERROR,
    };
    let data = serde_json::to_value(&diagnostic).ok();
    Diagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(diagnostic.code)),
        source: Some("mdbase".to_string()),
        message: diagnostic.message,
        data,
        ..Default::default()
    }
}

fn diagnostics_from_issues(text: &str, issues: Vec<serde_json::Value>) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let fallback_line = text::frontmatter_bounds(text).map(|(s, _)| s).unwrap_or(0);

    for issue in issues {
        diagnostics.push(diagnostic_from_issue(text, fallback_line, issue));
    }
    diagnostics
}

fn diagnostic_from_issue(text: &str, fallback_line: usize, issue: serde_json::Value) -> Diagnostic {
    let code = issue
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let message = issue
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("Validation issue");
    let severity_str = issue
        .get("severity")
        .and_then(|v| v.as_str())
        .unwrap_or("error");
    let severity = match severity_str {
        "warning" => DiagnosticSeverity::WARNING,
        "info" => DiagnosticSeverity::INFORMATION,
        _ => DiagnosticSeverity::ERROR,
    };

    let range = if let Some(field) = issue.get("field").and_then(|v| v.as_str()) {
        let (start, end) = text::find_field_range(text, field, fallback_line);
        Range::new(start, end)
    } else {
        Range::new(
            Position::new(fallback_line as u32, 0),
            Position::new(fallback_line as u32, 0),
        )
    };

    Diagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(code.to_string())),
        source: Some("mdbase".to_string()),
        message: message.to_string(),
        data: Some(issue),
        ..Default::default()
    }
}

fn issue_path(issue: &serde_json::Value) -> Option<String> {
    for key in ["path", "file", "rel_path"] {
        if let Some(value) = issue.get(key).and_then(|v| v.as_str()) {
            if !value.trim().is_empty() {
                return Some(value.replace('\\', "/"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("test file parent"))
            .expect("create test directory");
        fs::write(path, content).expect("write test file");
    }

    fn collection() -> (tempfile::TempDir, mdbase::Collection) {
        let directory = tempfile::tempdir().expect("temp collection");
        write(
            directory.path(),
            "mdbase.yaml",
            "spec_version: \"0.3.0\"\nsettings:\n  validation: error\n",
        );
        write(
            directory.path(),
            "_types/task.md",
            r#"---
kind: mdbase.type
name: task
schema:
  dialect: json-schema-2020-12
  value:
    type: object
    required: [type, title]
    additionalProperties: false
    properties:
      type: { const: task }
      title: { type: string }
      parent: { type: string }
collection:
  links:
    parent:
      target_type: task
      validate_exists: true
---
"#,
        );
        let collection = mdbase::Collection::open(directory.path()).expect("open v0.3 collection");
        (directory, collection)
    }

    #[test]
    fn v03_record_diagnostics_preserve_canonical_schema_data() {
        let (_directory, collection) = collection();
        let text = "---\ntype: task\n---\n";
        let diagnostics = compute(&collection, text, "tasks/missing.md", None);
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.code == Some(NumberOrString::String("schema_required".to_string()))
            })
            .expect("required diagnostic");
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
        assert!(diagnostic
            .data
            .as_ref()
            .and_then(|value| value.get("schema_location"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|location| location.starts_with("embedded://type/schema#")));
    }

    #[test]
    fn invalid_type_wrapper_is_diagnosed_without_reopening_collection() {
        let (directory, _collection) = collection();
        let text = r#"---
kind: mdbase.type
name: task
schema:
  dialect: json-schema-2020-12
  value: { type: object }
collecton: {}
---
"#;
        let diagnostics = diagnostics_from_v03_type_file(directory.path(), text, "_types/task.md");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code
                == Some(NumberOrString::String(
                    "schema_unevaluated_properties".to_string(),
                ))
        }));
    }

    #[test]
    fn v03_schema_diagnostics_preserve_path_field_type_and_source_range() {
        let (_directory, collection) = collection();
        let text = "---\ntype: task\ntitle: 42\nextra: value\n---\n";
        let diagnostics = compute(&collection, text, "tasks/invalid.md", None);

        let type_error = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.code == Some(NumberOrString::String("schema_type".to_string()))
            })
            .expect("schema type diagnostic");
        assert_eq!(type_error.range.start.line, 2);
        assert_eq!(type_error.range.start.character, 0);
        let data = type_error.data.as_ref().expect("canonical diagnostic data");
        assert_eq!(
            data.get("path"),
            Some(&serde_json::json!("tasks/invalid.md"))
        );
        assert_eq!(data.get("field"), Some(&serde_json::json!("title")));
        assert_eq!(data.get("type"), Some(&serde_json::json!("task")));
        assert!(data
            .get("schema_location")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|location| location.starts_with("embedded://type/schema#")));

        let additional = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.code
                    == Some(NumberOrString::String(
                        "schema_additional_properties".to_string(),
                    ))
            })
            .expect("additional property diagnostic");
        assert_eq!(additional.range.start.line, 3);
    }

    #[test]
    fn v03_collection_diagnostics_keep_link_field_and_path_data() {
        let (_directory, collection) = collection();
        let text = "---\ntype: task\ntitle: Linked\nparent: '[[missing]]'\n---\n";
        let diagnostics = compute(&collection, text, "tasks/linked.md", None);
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.code == Some(NumberOrString::String("link_not_found".to_string()))
            })
            .expect("missing link diagnostic");
        assert_eq!(diagnostic.range.start.line, 3);
        let data = diagnostic.data.as_ref().expect("canonical diagnostic data");
        assert_eq!(
            data.get("path"),
            Some(&serde_json::json!("tasks/linked.md"))
        );
        assert_eq!(data.get("field"), Some(&serde_json::json!("parent")));
    }
}
