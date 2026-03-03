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

    let Some((ctx, rel_path)) = state.context_and_rel_path_for_uri(uri) else {
        warn!(uri = %uri, "diagnostics: no collection context available");
        return;
    };
    let Some(text) = state.document_text(uri) else {
        warn!(uri = %uri, "diagnostics: no document text");
        return;
    };

    let cached = state.documents.get(uri).map(|doc| doc.frontmatter());
    let diagnostics = compute(&ctx.collection, &text, &rel_path, cached);
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
        let result = ctx.collection.validate_op(&serde_json::json!({}));
        let issues = result
            .get("issues")
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
            let abs = ctx.collection.root.join(&rel_path);
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
            "root": ctx.collection.root,
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

/// Compute diagnostics for a document.
///
/// TODO: Use mdbase library to parse frontmatter, resolve types, and validate.
/// Map mdbase::errors::Issue → LSP Diagnostic.
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
