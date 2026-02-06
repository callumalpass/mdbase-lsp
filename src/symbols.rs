use tower_lsp::lsp_types::*;

use crate::collection_utils;
use crate::state::BackendState;

pub(crate) fn workspace_symbols(
    state: &BackendState,
    query: &str,
) -> Option<Vec<SymbolInformation>> {
    let collection = state.get_collection()?;
    let normalized = query.trim().to_lowercase();
    let mut symbols = Vec::new();

    for entry in state.file_index.all_entries() {
        if !matches_query(&entry, &normalized) {
            continue;
        }
        let Some(uri) = collection_utils::uri_from_rel_path(&collection, &entry.rel_path) else {
            continue;
        };
        let name = entry
            .display_name
            .clone()
            .unwrap_or_else(|| entry.rel_path.clone());
        let detail = if entry.types.is_empty() {
            entry.rel_path.clone()
        } else {
            format!("{} ({})", entry.rel_path, entry.types.join(", "))
        };
        #[allow(deprecated)]
        symbols.push(SymbolInformation {
            name,
            kind: SymbolKind::FILE,
            tags: None,
            deprecated: None,
            location: Location {
                uri,
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            },
            container_name: Some(detail),
        });
    }

    Some(symbols)
}

pub(crate) fn query_collection(state: &BackendState, query: &str) -> serde_json::Value {
    let normalized = query.trim().to_lowercase();
    let matches = state
        .file_index
        .all_entries()
        .into_iter()
        .filter(|entry| matches_query(entry, &normalized))
        .map(|entry| {
            serde_json::json!({
                "path": entry.rel_path,
                "title": entry.title,
                "id": entry.id,
                "types": entry.types,
                "tags": entry.tags,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "query": query,
        "count": matches.len(),
        "matches": matches,
    })
}

fn matches_query(entry: &crate::file_index::FileEntry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    if let Some((k, v)) = query.split_once(':') {
        let value = v.trim();
        return match k.trim() {
            "type" => entry.types.iter().any(|t| t.eq_ignore_ascii_case(value)),
            "tag" => entry.tags.iter().any(|t| t.eq_ignore_ascii_case(value)),
            "id" => entry
                .id
                .as_deref()
                .map(|id| id.eq_ignore_ascii_case(value))
                .unwrap_or(false),
            "title" => entry
                .title
                .as_deref()
                .map(|title| title.to_lowercase().contains(value))
                .unwrap_or(false),
            _ => false,
        };
    }

    let haystack = format!(
        "{} {} {} {} {}",
        entry.rel_path,
        entry.display_name.clone().unwrap_or_default(),
        entry.title.clone().unwrap_or_default(),
        entry.types.join(" "),
        entry.tags.join(" ")
    )
    .to_lowercase();
    haystack.contains(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> crate::file_index::FileEntry {
        crate::file_index::FileEntry {
            rel_path: "notes/demo.md".to_string(),
            types: vec!["zettel".to_string()],
            tags: vec!["project".to_string(), "rust".to_string()],
            display_name: Some("Demo Note".to_string()),
            title: Some("Demo Note".to_string()),
            id: Some("abc-1".to_string()),
            preview: None,
        }
    }

    #[test]
    fn matches_field_queries() {
        let e = entry();
        assert!(matches_query(&e, "type:zettel"));
        assert!(matches_query(&e, "tag:project"));
        assert!(matches_query(&e, "id:abc-1"));
        assert!(!matches_query(&e, "type:person"));
    }

    #[test]
    fn matches_free_text_queries() {
        let e = entry();
        assert!(matches_query(&e, "demo"));
        assert!(matches_query(&e, "notes/demo"));
        assert!(!matches_query(&e, "nonexistent"));
    }
}
