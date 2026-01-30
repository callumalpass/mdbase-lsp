use dashmap::DashMap;
use ropey::Rope;
use tower_lsp::lsp_types::Url;

/// Shared backend state for the LSP server.
///
/// Holds the collection root, loaded type definitions, and in-memory
/// document contents for open files.
pub struct BackendState {
    /// Root path of the mdbase collection (where mdbase.yaml lives).
    pub collection_root: std::sync::RwLock<Option<std::path::PathBuf>>,

    /// In-memory content of open documents, keyed by URI.
    pub documents: DashMap<Url, Rope>,
}

impl BackendState {
    pub fn new() -> Self {
        Self {
            collection_root: std::sync::RwLock::new(None),
            documents: DashMap::new(),
        }
    }
}
