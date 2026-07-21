use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

use mdbase::Collection;
use tracing::debug;

use crate::collection_utils;
use crate::text;

#[derive(Debug, Clone)]
pub(crate) struct FileEntry {
    pub rel_path: String,
    pub types: Vec<String>,
    pub tags: Vec<String>,
    pub display_name: Option<String>,
    pub title: Option<String>,
    pub id: Option<String>,
    pub preview: Option<String>,
}

pub(crate) struct FileIndex {
    data: RwLock<IndexData>,
}

impl FileIndex {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(IndexData::default()),
        }
    }

    /// Full scan of the collection — reads every file's frontmatter and body.
    /// Call from a blocking context (spawn_blocking).
    pub fn rebuild(&self, collection: &Collection) {
        let files = collection_utils::scan_collection_files(collection);
        let mut entries = Vec::with_capacity(files.len());

        for path in files {
            let rel_path = match path.strip_prefix(&collection.root) {
                Ok(p) => p.to_string_lossy().to_string().replace('\\', "/"),
                Err(_) => continue,
            };
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let parsed = text::parse_frontmatter(&content);
            if parsed.parse_error || parsed.mapping_error {
                continue;
            }

            if let Some(entry) = build_entry(collection, rel_path, &content, &parsed.json) {
                entries.push(entry);
            }
        }

        entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

        let mut data = IndexData {
            entries,
            ..Default::default()
        };
        data.reindex();
        debug!(count = data.entries.len(), "file_index: rebuilt");
        *self.data.write().unwrap() = data;
    }

    /// Upsert a single file entry from in-memory text.
    pub fn upsert_from_text(&self, collection: &Collection, rel_path: String, text: &str) {
        let parsed = text::parse_frontmatter(text);
        if parsed.parse_error || parsed.mapping_error {
            self.remove_path(&rel_path);
            return;
        }
        let Some(entry) = build_entry(collection, rel_path.clone(), text, &parsed.json) else {
            self.remove_path(&rel_path);
            return;
        };
        let mut data = self.data.write().unwrap();
        if let Some(existing) = data.entries.iter_mut().find(|e| e.rel_path == rel_path) {
            *existing = entry;
        } else {
            data.entries.push(entry);
        }
        data.entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        data.reindex();
    }

    /// Remove a file entry by its collection-relative path.
    pub fn remove_path(&self, rel_path: &str) {
        let mut data = self.data.write().unwrap();
        data.entries.retain(|e| e.rel_path != rel_path);
        data.reindex();
    }

    /// Return rel_paths that match `target_type` (or all files if None).
    pub fn link_targets(&self, target_type: Option<&str>) -> Vec<String> {
        let data = self.data.read().unwrap();
        data.entries
            .iter()
            .filter(|e| match target_type {
                Some(tt) => e.types.iter().any(|t| t.eq_ignore_ascii_case(tt)),
                None => true,
            })
            .map(|e| e.rel_path.clone())
            .collect()
    }

    /// Return rel_paths with optional display names that match `target_type`.
    pub fn link_targets_with_display(
        &self,
        target_type: Option<&str>,
    ) -> Vec<(String, Option<String>, Option<String>)> {
        let data = self.data.read().unwrap();
        data.entries
            .iter()
            .filter(|e| match target_type {
                Some(tt) => e.types.iter().any(|t| t.eq_ignore_ascii_case(tt)),
                None => true,
            })
            .map(|e| {
                (
                    e.rel_path.clone(),
                    e.display_name.clone(),
                    e.preview.clone(),
                )
            })
            .collect()
    }

    pub fn all_entries(&self) -> Vec<FileEntry> {
        self.data.read().unwrap().entries.clone()
    }

    pub fn all_rel_paths(&self) -> Vec<String> {
        self.data
            .read()
            .unwrap()
            .entries
            .iter()
            .map(|e| e.rel_path.clone())
            .collect()
    }

    pub fn tag_counts(&self) -> Vec<(String, usize)> {
        let data = self.data.read().unwrap();
        let mut counts = std::collections::HashMap::<String, usize>::new();
        for entry in data.entries.iter() {
            for tag in &entry.tags {
                *counts.entry(tag.clone()).or_default() += 1;
            }
        }
        let mut result: Vec<(String, usize)> = counts.into_iter().collect();
        result.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        result
    }

    /// Resolve a user-entered link target to a collection-relative file path.
    pub fn resolve_target_rel_path(
        &self,
        collection: &Collection,
        target: &str,
        source_rel_path: Option<&str>,
    ) -> Option<String> {
        let target = normalize_target(target)?;
        let resolved = resolve_relative_target(&target, source_rel_path);

        let data = self.data.read().unwrap();
        if let Some(found) = lookup_exact(&data, &resolved) {
            return Some(found);
        }

        // Extension inference.
        if !resolved.contains('.')
            || (!resolved.ends_with(".md") && !has_known_extension(collection, &resolved))
        {
            let with_md = format!("{}.md", resolved);
            if let Some(found) = lookup_exact(&data, &with_md) {
                return Some(found);
            }
            for ext in &collection.settings.extensions {
                let with_ext = format!("{}.{}", resolved, ext);
                if let Some(found) = lookup_exact(&data, &with_ext) {
                    return Some(found);
                }
            }
        }

        // Stem fallback for simple names.
        if !resolved.contains('/') {
            let key = resolved.to_lowercase();
            if let Some(indices) = data.by_stem_lower.get(&key) {
                if indices.is_empty() {
                    return None;
                }
                // Prefer exact-case stem match, then first indexed candidate.
                if let Some(idx) = indices.iter().copied().find(|idx| {
                    let stem = Path::new(&data.entries[*idx].rel_path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    stem == resolved
                }) {
                    return Some(data.entries[idx].rel_path.clone());
                }
                let first = indices[0];
                return Some(data.entries[first].rel_path.clone());
            }
        }

        None
    }

    pub fn resolve_target_abs_path(
        &self,
        collection: &Collection,
        target: &str,
        source_rel_path: Option<&str>,
    ) -> Option<std::path::PathBuf> {
        self.resolve_target_rel_path(collection, target, source_rel_path)
            .map(|rel| collection.root.join(rel))
    }
}

#[derive(Default)]
struct IndexData {
    entries: Vec<FileEntry>,
    by_rel_path: HashMap<String, usize>,
    by_rel_path_lower: HashMap<String, usize>,
    by_stem_lower: HashMap<String, Vec<usize>>,
}

impl IndexData {
    fn reindex(&mut self) {
        self.by_rel_path.clear();
        self.by_rel_path_lower.clear();
        self.by_stem_lower.clear();

        for (idx, entry) in self.entries.iter().enumerate() {
            self.by_rel_path.insert(entry.rel_path.clone(), idx);
            self.by_rel_path_lower
                .entry(entry.rel_path.to_lowercase())
                .or_insert(idx);

            let stem = Path::new(&entry.rel_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            self.by_stem_lower.entry(stem).or_default().push(idx);
        }
    }
}

fn build_entry(
    collection: &Collection,
    rel_path: String,
    content: &str,
    frontmatter: &serde_json::Value,
) -> Option<FileEntry> {
    let types = collection.determine_types_for_path(frontmatter, Some(&rel_path));
    let title = json_string(frontmatter, "title");
    let id = json_string(frontmatter, "id");
    let mut display_name = display_name_from_type_defs(&collection.types, &types, frontmatter);

    if display_name.is_none() {
        if let Some((effective_frontmatter, effective_types)) =
            effective_frontmatter_and_types(collection, &rel_path)
        {
            display_name = display_name_from_type_defs(
                &collection.types,
                &effective_types,
                &effective_frontmatter,
            );
        }
    }

    if display_name.is_none() {
        display_name = title
            .clone()
            .or_else(|| json_string(frontmatter, "name"))
            .or_else(|| id.clone());
    }

    let preview = build_preview(content);
    let tags = collect_tags(content, frontmatter);
    Some(FileEntry {
        rel_path,
        types,
        tags,
        display_name,
        title,
        id,
        preview,
    })
}

fn build_preview(content: &str) -> Option<String> {
    let max_chars = 2000usize;
    if content.is_empty() {
        return None;
    }
    let mut preview: String = content.chars().take(max_chars).collect();
    if content.chars().count() > max_chars {
        preview.push_str("...");
    }
    Some(preview)
}

fn collect_tags(content: &str, frontmatter: &serde_json::Value) -> Vec<String> {
    let mut tags = Vec::new();
    if let Some(arr) = frontmatter.get("tags").and_then(|v| v.as_array()) {
        for tag_val in arr {
            if let Some(tag) = tag_val.as_str() {
                if !tags.contains(&tag.to_string()) {
                    tags.push(tag.to_string());
                }
            }
        }
    } else if let Some(tag) = frontmatter.get("tags").and_then(|v| v.as_str()) {
        if !tags.contains(&tag.to_string()) {
            tags.push(tag.to_string());
        }
    }
    let parsed_doc = mdbase::frontmatter::parser::parse_document(content);
    let body_tags = mdbase::expressions::evaluator::extract_tags_from_body(&parsed_doc.body);
    for tag in body_tags {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

fn json_string(frontmatter: &serde_json::Value, key: &str) -> Option<String> {
    let value = frontmatter.get(key)?.as_str()?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn effective_frontmatter_and_types(
    collection: &Collection,
    rel_path: &str,
) -> Option<(serde_json::Value, Vec<String>)> {
    let result = collection.read(&serde_json::json!({ "path": rel_path }));
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

fn display_name_from_type_defs(
    types_map: &HashMap<String, mdbase::types::schema::TypeDef>,
    type_names: &[String],
    frontmatter: &serde_json::Value,
) -> Option<String> {
    for type_name in type_names {
        let Some(type_def) = types_map
            .get(type_name)
            .or_else(|| types_map.get(&type_name.to_lowercase()))
        else {
            continue;
        };
        let Some(key) = type_def.display_name_key.as_deref() else {
            continue;
        };
        let Some(value) = frontmatter.get(key).and_then(frontmatter_display_value) else {
            continue;
        };
        return Some(value);
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

fn lookup_exact(data: &IndexData, rel_path: &str) -> Option<String> {
    if let Some(idx) = data.by_rel_path.get(rel_path).copied() {
        return Some(data.entries[idx].rel_path.clone());
    }
    data.by_rel_path_lower
        .get(&rel_path.to_lowercase())
        .copied()
        .map(|idx| data.entries[idx].rel_path.clone())
}

fn has_known_extension(collection: &Collection, path: &str) -> bool {
    if path.ends_with(".md") {
        return true;
    }
    for ext in &collection.settings.extensions {
        if path.ends_with(&format!(".{}", ext)) {
            return true;
        }
    }
    false
}

fn resolve_relative_target(target: &str, source_rel_path: Option<&str>) -> String {
    if target.starts_with("./") || target.starts_with("../") {
        let source_dir = source_rel_path
            .and_then(|s| Path::new(s).parent())
            .unwrap_or(Path::new(""));
        let joined = source_dir.join(target);
        return normalize_path_segments(&joined.to_string_lossy().replace('\\', "/"));
    }
    if let Some(stripped) = target.strip_prefix('/') {
        return stripped.to_string();
    }
    target.to_string()
}

fn normalize_target(target: &str) -> Option<String> {
    let target = if target.starts_with("[[") && target.ends_with("]]") {
        let inner = &target[2..target.len() - 2];
        inner
            .split('|')
            .next()
            .unwrap_or(inner)
            .split('#')
            .next()
            .unwrap_or(inner)
            .trim()
            .to_string()
    } else {
        target
            .split('#')
            .next()
            .unwrap_or(target)
            .trim()
            .to_string()
    };

    if target.is_empty() {
        None
    } else {
        Some(target)
    }
}

fn normalize_path_segments(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.is_empty() || parts.last() == Some(&"..") {
                    parts.push("..");
                } else {
                    parts.pop();
                }
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdbase::types::schema::TypeDef;

    #[test]
    fn display_name_from_type_defs_uses_display_name_key() {
        let mut types_map = HashMap::new();
        types_map.insert(
            "task".to_string(),
            TypeDef {
                name: "task".to_string(),
                kind: None,
                version: None,
                description: None,
                extends: None,
                strict: None,
                filename_pattern: None,
                path_pattern: None,
                display_name_key: Some("title".to_string()),
                fields: HashMap::new(),
                match_rules: None,
                json_schema: None,
                read_defaults: HashMap::new(),
                lifecycle: None,
                source_path: None,
            },
        );

        let frontmatter = serde_json::json!({
            "title": "Ship alias support",
            "type": "task"
        });
        let got = display_name_from_type_defs(&types_map, &["task".to_string()], &frontmatter);
        assert_eq!(got.as_deref(), Some("Ship alias support"));
    }

    #[test]
    fn display_name_from_type_defs_accepts_numeric_value() {
        let mut types_map = HashMap::new();
        types_map.insert(
            "invoice".to_string(),
            TypeDef {
                name: "invoice".to_string(),
                kind: None,
                version: None,
                description: None,
                extends: None,
                strict: None,
                filename_pattern: None,
                path_pattern: None,
                display_name_key: Some("number".to_string()),
                fields: HashMap::new(),
                match_rules: None,
                json_schema: None,
                read_defaults: HashMap::new(),
                lifecycle: None,
                source_path: None,
            },
        );

        let frontmatter = serde_json::json!({
            "number": 42,
            "type": "invoice"
        });
        let got = display_name_from_type_defs(&types_map, &["invoice".to_string()], &frontmatter);
        assert_eq!(got.as_deref(), Some("42"));
    }
}
