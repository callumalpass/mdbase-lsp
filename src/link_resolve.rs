/// Resolve body link targets to file paths within the collection.
use tower_lsp::lsp_types::Url;

use crate::body_links::BodyLink;
use crate::state::CollectionContext;

/// Resolve a `BodyLink` target to a file `Url`.
///
/// Uses the existing `resolve_link_target` from `collection_utils`, which handles:
/// - Root-relative paths (containing `/`)
/// - Source-relative paths (`./`, `../`)
/// - Bare names (stem matching across collection)
/// - Extension inference (`.md` appended)
pub(crate) fn resolve_body_link(
    ctx: &CollectionContext,
    source_rel_path: &str,
    link: &BodyLink,
) -> Option<Url> {
    let resolved = ctx.file_index.resolve_target_abs_path(
        &ctx.collection,
        &link.target,
        Some(source_rel_path),
    )?;

    Url::from_file_path(&resolved).ok()
}
