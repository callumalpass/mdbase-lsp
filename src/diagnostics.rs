use tower_lsp::lsp_types::*;
use tower_lsp::Client;

use crate::state::BackendState;

/// Validate the document and publish diagnostics.
pub async fn publish(client: &Client, state: &BackendState, uri: &Url) {
    // Only process markdown files
    if !uri.path().ends_with(".md") {
        return;
    }

    let diagnostics = compute(state, uri);
    client.publish_diagnostics(uri.clone(), diagnostics, None).await;
}

/// Compute diagnostics for a document.
///
/// TODO: Use mdbase library to parse frontmatter, resolve types, and validate.
/// Map mdbase::errors::Issue → LSP Diagnostic.
fn compute(_state: &BackendState, _uri: &Url) -> Vec<Diagnostic> {
    // Stub: no diagnostics yet.
    // Implementation will:
    // 1. Get document content from state.documents
    // 2. Parse frontmatter using mdbase::frontmatter::parser
    // 3. Determine types using mdbase::matching::engine
    // 4. Validate using mdbase::validation::validator
    // 5. Map each Issue to a Diagnostic
    Vec::new()
}
