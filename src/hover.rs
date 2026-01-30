use tower_lsp::lsp_types::*;

use crate::state::BackendState;

/// Provide hover information at the given position.
///
/// TODO: Implement:
/// - Field name hover: show type, constraints, description from type schema
/// - Link hover: show target file's frontmatter preview
/// - Type name hover: show type definition summary
pub fn provide(
    _state: &BackendState,
    _uri: &Url,
    _position: Position,
) -> Option<Hover> {
    // Stub: no hover info yet.
    None
}
