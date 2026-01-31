use dashmap::DashMap;
use ropey::Rope;
use tower_lsp::lsp_types::Url;

use std::sync::Arc;

use mdbase::Collection;

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
    pub documents: DashMap<Url, Rope>,
}

impl BackendState {
    pub fn new() -> Self {
        Self {
            collection_root: std::sync::RwLock::new(None),
            collection: std::sync::RwLock::new(None),
            documents: DashMap::new(),
        }
    }

    pub fn get_collection(&self) -> Option<Arc<Collection>> {
        if let Some(existing) = self.collection.read().unwrap().as_ref() {
            return Some(existing.clone());
        }
        let root = self.collection_root.read().unwrap().clone()?;
        match Collection::open(&root) {
            Ok(collection) => {
                let arc = Arc::new(collection);
                *self.collection.write().unwrap() = Some(arc.clone());
                Some(arc)
            }
            Err(_) => None,
        }
    }

    pub fn document_text(&self, uri: &Url) -> Option<String> {
        self.documents.get(uri).map(|r| r.to_string())
    }
}
