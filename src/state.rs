use dashmap::DashMap;
use ropey::Rope;
use tower_lsp::lsp_types::Url;
use tracing::{info, warn};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mdbase::Collection;

use crate::file_index::FileIndex;
use crate::text::ParsedFrontmatter;

/// Per-document state: rope content + cached frontmatter.
pub struct DocumentState {
    pub rope: Rope,
    cached_frontmatter: Mutex<Option<ParsedFrontmatter>>,
}

impl DocumentState {
    pub fn new(rope: Rope) -> Self {
        Self {
            rope,
            cached_frontmatter: Mutex::new(None),
        }
    }

    /// Get cached frontmatter, parsing lazily if needed.
    pub fn frontmatter(&self) -> ParsedFrontmatter {
        let mut cache = self.cached_frontmatter.lock().unwrap();
        if let Some(ref cached) = *cache {
            return cached.clone();
        }
        let text = self.rope.to_string();
        let parsed = crate::text::parse_frontmatter(&text);
        *cache = Some(parsed.clone());
        parsed
    }

    /// Invalidate cached frontmatter (call after rope mutations).
    pub fn invalidate_frontmatter(&self) {
        *self.cached_frontmatter.lock().unwrap() = None;
    }
}

/// Shared backend state for the LSP server.
///
/// Holds the collection root, loaded type definitions, and in-memory
/// document contents for open files.
pub struct BackendState {
    /// Root path of the mdbase collection (where mdbase.yaml lives).
    pub collection_root: std::sync::RwLock<Option<std::path::PathBuf>>,

    /// Loaded mdbase collection (config + types).
    pub collection: std::sync::RwLock<Option<Arc<Collection>>>,

    /// In-memory content of open documents, keyed by URI.
    pub documents: DashMap<Url, DocumentState>,

    /// Generation counter per document for debouncing diagnostics.
    pub diagnostics_generation: DashMap<Url, Arc<AtomicU64>>,

    /// Cached file index for completions.
    pub file_index: FileIndex,
}

impl BackendState {
    pub fn new() -> Self {
        Self {
            collection_root: std::sync::RwLock::new(None),
            collection: std::sync::RwLock::new(None),
            documents: DashMap::new(),
            diagnostics_generation: DashMap::new(),
            file_index: FileIndex::new(),
        }
    }

    pub fn get_collection(&self) -> Option<Arc<Collection>> {
        if let Some(existing) = self.collection.read().unwrap().as_ref() {
            return Some(existing.clone());
        }
        let root = match self.collection_root.read().unwrap().clone() {
            Some(r) => r,
            None => {
                warn!("get_collection: collection_root is None");
                return None;
            }
        };
        info!(root = %root.display(), "get_collection: opening collection");
        match Collection::open(&root) {
            Ok(collection) => {
                info!(
                    types = collection.types.len(),
                    "get_collection: loaded collection"
                );
                let arc = Arc::new(collection);
                *self.collection.write().unwrap() = Some(arc.clone());
                Some(arc)
            }
            Err(e) => {
                warn!(root = %root.display(), error = %e, "get_collection: Collection::open failed");
                None
            }
        }
    }

    /// Drop the cached collection so the next `get_collection()` reloads from disk.
    pub fn invalidate_collection(&self) {
        *self.collection.write().unwrap() = None;
    }

    pub fn document_text(&self, uri: &Url) -> Option<String> {
        self.documents.get(uri).map(|r| r.rope.to_string())
    }

    /// Get the diagnostics generation counter for a URI, creating it if needed.
    pub fn generation_counter(&self, uri: &Url) -> Arc<AtomicU64> {
        self.diagnostics_generation
            .entry(uri.clone())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone()
    }

    /// Bump the generation counter and return the new value.
    pub fn bump_generation(&self, uri: &Url) -> u64 {
        self.generation_counter(uri).fetch_add(1, Ordering::SeqCst) + 1
    }
}
