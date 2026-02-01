use std::sync::RwLock;

use mdbase::Collection;
use tracing::debug;

use crate::collection_utils;
use crate::text;

pub(crate) struct FileEntry {
    pub rel_path: String,
    pub types: Vec<String>,
    pub tags: Vec<String>,
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
