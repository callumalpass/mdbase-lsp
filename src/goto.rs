use tower_lsp::lsp_types::*;

use crate::state::BackendState;

/// Provide go-to-definition for the given position.
///
/// TODO: Implement:
/// - Link → target file (wikilink, markdown link, bare path)
/// - Type name in frontmatter → _types/ definition file
pub fn definition(
    _state: &BackendState,
    _uri: &Url,
    _position: Position,
) -> Option<GotoDefinitionResponse> {
    // Stub: no go-to-definition yet.
    None
}
