/// DocumentLink provider — returns clickable link ranges with resolved target URIs.

use tower_lsp::lsp_types::*;

use crate::body_links;
use crate::link_resolve;
use crate::state::BackendState;

/// Build the `textDocument/documentLink` response for a document.
pub(crate) fn provide(state: &BackendState, uri: &Url) -> Option<Vec<DocumentLink>> {
    let collection = state.get_collection()?;
    let text = state.document_text(uri)?;

    let body_links = body_links::find_body_links(&text);
    if body_links.is_empty() {
        return Some(Vec::new());
    }

    let mut result = Vec::new();
    for link in &body_links {
        if let Some(target_url) = link_resolve::resolve_body_link(&collection, uri, link) {
            result.push(DocumentLink {
                range: Range {
                    start: Position::new(link.start_line as u32, link.start_col as u32),
                    end: Position::new(link.end_line as u32, link.end_col as u32),
                },
                target: Some(target_url),
                tooltip: Some(link.target.clone()),
                data: None,
            });
        }
    }

    Some(result)
}
