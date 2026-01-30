use tower_lsp::lsp_types::*;

use crate::state::BackendState;

/// Provide completions at the given position.
///
/// TODO: Implement:
/// - Field name completions (when cursor is at start of a frontmatter line)
/// - Enum value completions (when cursor is after a known enum field's colon)
/// - Link target completions (when cursor is inside [[ ]] or []())
/// - Tag completions (when cursor is after #)
pub fn provide(
    _state: &BackendState,
    _uri: &Url,
    _position: Position,
) -> Option<CompletionResponse> {
    // Stub: no completions yet.
    None
}
