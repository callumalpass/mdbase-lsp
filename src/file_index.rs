use std::sync::RwLock;

use mdbase::Collection;
use tracing::debug;

use crate::collection_utils;
use crate::text;

pub(crate) struct FileEntry {
    pub rel_path: String,
    pub types: Vec<String>,
    pub tags: Vec<String>,
    pub display_name: Option<String>,
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

            let types = collection.determine_types_for_path(&parsed.json, Some(&rel_path));
            let display_name = resolve_display_name(collection, &types, &parsed.json);
            let preview = build_preview(&content);

            let mut tags = Vec::new();
            if let Some(arr) = parsed.json.get("tags").and_then(|v| v.as_array()) {
                for tag_val in arr {
                    if let Some(tag) = tag_val.as_str() {
                        if !tags.contains(&tag.to_string()) {
                            tags.push(tag.to_string());
                        }
                    }
                }
            } else if let Some(tag) = parsed.json.get("tags").and_then(|v| v.as_str()) {
                if !tags.contains(&tag.to_string()) {
                    tags.push(tag.to_string());
                }
            }
            let parsed_doc = mdbase::frontmatter::parser::parse_document(&content);
            let body_tags = mdbase::expressions::evaluator::extract_tags_from_body(&parsed_doc.body);
            for tag in body_tags {
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }

            entries.push(FileEntry {
                rel_path,
                types,
                tags,
                display_name,
                preview,
            });
        }

        debug!(count = entries.len(), "file_index: rebuilt");
        *self.entries.write().unwrap() = entries;
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
            .map(|e| (e.rel_path.clone(), e.display_name.clone(), e.preview.clone()))
            .collect()
    }

    /// Return all unique tags across the collection, sorted.
    pub fn all_tags(&self) -> Vec<String> {
        let entries = self.entries.read().unwrap();
        let mut tags = Vec::new();
        for entry in entries.iter() {
            for tag in &entry.tags {
                if !tags.contains(tag) {
                    tags.push(tag.clone());
                }
            }
        }
        tags.sort();
        tags
    }
}

fn resolve_display_name(
    collection: &Collection,
    type_names: &[String],
    frontmatter: &serde_json::Value,
) -> Option<String> {
    let mut candidates = Vec::new();
    if type_names.is_empty() {
        for type_def in collection.types.values() {
            if let Some(name) = &type_def.display_name {
                candidates.push(name.clone());
            }
        }
    } else {
        for type_name in type_names {
            if let Some(type_def) = collection.types.get(type_name) {
                if let Some(name) = &type_def.display_name {
                    candidates.push(name.clone());
                }
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    for field in candidates {
        if !seen.insert(field.clone()) {
            continue;
        }
        if let Some(value) = frontmatter.get(&field).and_then(|v| v.as_str()) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    if let Some(value) = frontmatter.get("display-name").and_then(|v| v.as_str()) {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }

    if let Some(value) = frontmatter.get("title").and_then(|v| v.as_str()) {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }

    None
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
