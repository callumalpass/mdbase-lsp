use dashmap::DashMap;
use ropey::Rope;
use tower_lsp::lsp_types::Url;
use tracing::{info, warn};

use std::path::{Path, PathBuf};
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
    /// Workspace roots from LSP initialization / folder changes.
    pub workspace_roots: std::sync::RwLock<Vec<PathBuf>>,

    /// Loaded collection contexts (one per root).
    pub collection_contexts: DashMap<PathBuf, Arc<CollectionContext>>,

    /// In-memory content of open documents, keyed by URI.
    pub documents: DashMap<Url, DocumentState>,

    /// Generation counter per document for debouncing diagnostics.
    pub diagnostics_generation: DashMap<Url, Arc<AtomicU64>>,

    /// URIs that last had diagnostics published via collection-wide validation.
    pub collection_diagnostics_published: DashMap<Url, ()>,
}

impl BackendState {
    pub fn new() -> Self {
        Self {
            workspace_roots: std::sync::RwLock::new(Vec::new()),
            collection_contexts: DashMap::new(),
            documents: DashMap::new(),
            diagnostics_generation: DashMap::new(),
            collection_diagnostics_published: DashMap::new(),
        }
    }

    pub fn set_workspace_roots(&self, mut roots: Vec<PathBuf>) {
        roots.sort();
        roots.dedup();
        *self.workspace_roots.write().unwrap() = roots.clone();

        // Drop cached contexts that are no longer in the workspace root set.
        // Nested discovered roots are kept only if still under some workspace root.
        let stale: Vec<PathBuf> = self
            .collection_contexts
            .iter()
            .filter_map(|entry| {
                let root = entry.key();
                let keep = roots.iter().any(|workspace| root.starts_with(workspace));
                if keep {
                    None
                } else {
                    Some(root.clone())
                }
            })
            .collect();
        for root in stale {
            self.collection_contexts.remove(&root);
        }
    }

    pub fn workspace_roots_snapshot(&self) -> Vec<PathBuf> {
        self.workspace_roots.read().unwrap().clone()
    }

    pub fn root_for_uri(&self, uri: &Url) -> Option<PathBuf> {
        let path = uri.to_file_path().ok()?;
        self.root_for_path(&path)
    }

    pub fn root_for_path(&self, path: &Path) -> Option<PathBuf> {
        if let Some(discovered) = discover_root_from_path(path) {
            return Some(discovered);
        }

        let roots = self.workspace_roots_snapshot();
        let mut best: Option<PathBuf> = None;
        for root in roots {
            if path.starts_with(&root) {
                match &best {
                    Some(current) if current.components().count() >= root.components().count() => {}
                    _ => best = Some(root),
                }
            }
        }
        if best.is_some() {
            return best;
        }

        self.workspace_roots_snapshot().into_iter().next()
    }

    pub fn context_for_uri(&self, uri: &Url) -> Option<Arc<CollectionContext>> {
        let root = self.root_for_uri(uri)?;
        self.context_for_root(&root)
    }

    pub fn context_and_rel_path_for_uri(
        &self,
        uri: &Url,
    ) -> Option<(Arc<CollectionContext>, String)> {
        let path = uri.to_file_path().ok()?;
        let ctx = self.context_for_uri(uri)?;
        let rel_path = path
            .strip_prefix(ctx.collection.root())
            .ok()
            .map(|r| r.to_string_lossy().to_string().replace('\\', "/"))?;
        Some((ctx, rel_path))
    }

    pub fn context_for_root(&self, root: &Path) -> Option<Arc<CollectionContext>> {
        if let Some(existing) = self.collection_contexts.get(root) {
            return Some(existing.clone());
        }

        info!(root = %root.display(), "context_for_root: opening collection");
        match Collection::open(root) {
            Ok(collection) => {
                info!(
                    root = %root.display(),
                    types = collection.types().len(),
                    "context_for_root: loaded collection"
                );
                let collection = Arc::new(collection);
                let file_index = Arc::new(FileIndex::new());
                file_index.rebuild(&collection);
                let ctx = Arc::new(CollectionContext {
                    root: root.to_path_buf(),
                    collection,
                    file_index,
                });
                self.collection_contexts
                    .insert(root.to_path_buf(), Arc::clone(&ctx));
                Some(ctx)
            }
            Err(e) => {
                warn!(
                    root = %root.display(),
                    error = %e,
                    "context_for_root: Collection::open failed"
                );
                None
            }
        }
    }

    pub fn all_contexts(&self) -> Vec<Arc<CollectionContext>> {
        let mut contexts = Vec::new();

        for root in self.workspace_roots_snapshot() {
            if let Some(ctx) = self.context_for_root(&root) {
                contexts.push(ctx);
            }
        }

        // Include any discovered roots that may not be listed as workspace folders.
        for entry in self.collection_contexts.iter() {
            if !contexts.iter().any(|ctx| ctx.root == *entry.key()) {
                contexts.push(entry.value().clone());
            }
        }

        contexts
    }

    pub fn invalidate_root(&self, root: &Path) {
        self.collection_contexts.remove(root);
    }

    pub fn invalidate_for_uri(&self, uri: &Url) {
        if let Some(root) = self.root_for_uri(uri) {
            self.invalidate_root(&root);
        }
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

pub struct CollectionContext {
    /// Root path of the mdbase collection.
    pub root: PathBuf,
    /// Loaded mdbase collection (config + types).
    pub collection: Arc<Collection>,
    /// File index for link targets, tags, and symbols.
    pub file_index: Arc<FileIndex>,
}

fn discover_root_from_path(path: &Path) -> Option<PathBuf> {
    let mut current = if path.is_dir() {
        Some(path)
    } else {
        path.parent()
    };

    while let Some(dir) = current {
        let config = dir.join("mdbase.yaml");
        if config.exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }

    None
}
