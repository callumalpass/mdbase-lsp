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
    entries: RwLock<Vec<FileEntry>>,
}

impl FileIndex {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
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

        debug!(count = entries.len(), "file_index: rebuilt");
        *self.entries.write().unwrap() = entries;
    }

    /// Upsert a single file entry from in-memory text.
    pub fn upsert_from_text(&self, collection: &Collection, rel_path: String, text: &str) {
        let parsed = text::parse_frontmatter(text);
        if parsed.parse_error || parsed.mapping_error {
            return;
        }
        let Some(entry) = build_entry(collection, rel_path.clone(), text, &parsed.json) else {
            return;
        };
        let mut entries = self.entries.write().unwrap();
        if let Some(existing) = entries.iter_mut().find(|e| e.rel_path == rel_path) {
            *existing = entry;
        } else {
            entries.push(entry);
        }
    }

    /// Remove a file entry by its collection-relative path.
    pub fn remove_path(&self, rel_path: &str) {
        let mut entries = self.entries.write().unwrap();
        entries.retain(|e| e.rel_path != rel_path);
    }

    /// Return rel_paths that match `target_type` (or all files if None).
    pub fn link_targets(&self, target_type: Option<&str>) -> Vec<String> {
        let entries = self.entries.read().unwrap();
        entries
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
        let entries = self.entries.read().unwrap();
        entries
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
        self.entries.read().unwrap().clone()
    }

    pub fn tag_counts(&self) -> Vec<(String, usize)> {
        let entries = self.entries.read().unwrap();
        let mut counts = std::collections::HashMap::<String, usize>::new();
        for entry in entries.iter() {
            for tag in &entry.tags {
                *counts.entry(tag.clone()).or_default() += 1;
            }
        }
        let mut result: Vec<(String, usize)> = counts.into_iter().collect();
        result.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        result
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
    let display_name = title
        .clone()
        .or_else(|| json_string(frontmatter, "name"))
        .or_else(|| id.clone());
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
