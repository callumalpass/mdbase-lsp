/// Resolve body link targets to file paths within the collection.
use std::path::PathBuf;

use tower_lsp::lsp_types::Url;
use tracing::debug;

use crate::body_links::BodyLink;
use crate::collection_utils;

/// Resolve a `BodyLink` target to a file `Url`.
///
/// Uses the existing `resolve_link_target` from `collection_utils`, which handles:
/// - Root-relative paths (containing `/`)
/// - Source-relative paths (`./`, `../`)
/// - Bare names (stem matching across collection)
/// - Extension inference (`.md` appended)
pub(crate) fn resolve_body_link(
    collection: &mdbase::Collection,
    source_uri: &Url,
    link: &BodyLink,
) -> Option<Url> {
    let source_rel_path = source_uri.to_file_path().ok().and_then(|p| {
        p.strip_prefix(&collection.root)
            .ok()
            .map(|r| r.to_string_lossy().to_string().replace('\\', "/"))
    });

    debug!(
        target = %link.target,
        source = ?source_rel_path,
        "link_resolve: resolving body link"
    );

    let resolved: PathBuf = collection_utils::resolve_link_target(
        collection,
        &link.target,
        source_rel_path.as_deref(),
    )?;

    Url::from_file_path(&resolved).ok()
}
