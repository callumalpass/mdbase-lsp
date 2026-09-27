use std::path::{Path, PathBuf};

use mdbase::Collection;
use tower_lsp::lsp_types::Url;

pub(crate) fn scan_collection_files(collection: &Collection) -> Vec<PathBuf> {
    let mut files = Vec::new();
    scan_dir_recursive(collection, collection.root(), &mut files);
    files
}

pub(crate) fn find_type_definition_path(
    collection: &Collection,
    type_name: &str,
) -> Option<PathBuf> {
    let types_dir = collection.root().join(&collection.settings().types_folder);
    if !types_dir.exists() {
        return None;
    }
    let mut candidates = Vec::new();
    collect_type_files(&types_dir, &mut candidates);
    for path in candidates {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.eq_ignore_ascii_case(type_name) {
            return Some(path);
        }
    }
    None
}

pub(crate) fn find_data_contract_definition_path(
    collection: &Collection,
    contract_id: &str,
) -> Option<PathBuf> {
    collection
        .list_data_contracts()
        .into_iter()
        .filter(|contract| contract.id == contract_id)
        .flat_map(|contract| contract.source_paths)
        .map(|relative| collection.root().join(relative))
        .min()
}

fn scan_dir_recursive(collection: &Collection, dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(rel) = rel_path(collection, &path) else {
            continue;
        };
        // mdbase applies the discovery rules for the collection's spec version.
        if path.is_dir() {
            if !collection.is_excluded_path(&rel) {
                scan_dir_recursive(collection, &path, files);
            }
        } else if path.is_file() && collection.is_record_path(&rel) {
            files.push(path);
        }
    }
}

fn rel_path(collection: &Collection, path: &Path) -> Option<String> {
    path.strip_prefix(collection.root())
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

fn collect_type_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_type_files(&path, out);
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext = ext.to_ascii_lowercase();
                if ext == "md" || ext == "yaml" || ext == "yml" {
                    out.push(path);
                }
            }
        }
    }
}

/// Parse a frontmatter link value and extract the target string.
///
/// Handles: `[[target]]`, `[[target|alias]]`, `[text](path)`, bare paths.
pub(crate) fn parse_link_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let (value, _) = strip_wrapping_quotes(value);
    if value.is_empty() {
        return None;
    }

    // Wikilink: [[target]] or [[target|alias]]
    if value.starts_with("[[") && value.ends_with("]]") {
        let inner = &value[2..value.len() - 2];
        let target = inner.split('|').next().unwrap_or(inner);
        let target = target.split('#').next().unwrap_or(target).trim();
        if target.is_empty() {
            return None;
        }
        return Some(target.to_string());
    }

    // Markdown link: [text](path)
    if value.starts_with('[') {
        if let Some(bracket_end) = value.find("](") {
            if value.ends_with(')') {
                let path = &value[bracket_end + 2..value.len() - 1];
                let path = path.trim();
                if !path.is_empty() && !path.starts_with("http://") && !path.starts_with("https://")
                {
                    let target = path.split('#').next().unwrap_or(path).to_string();
                    if !target.is_empty() {
                        return Some(target);
                    }
                }
                return None;
            }
        }
    }

    // Bare path — skip external URLs
    if value.starts_with("http://") || value.starts_with("https://") {
        return None;
    }

    Some(value.split('#').next().unwrap_or(value).trim().to_string())
}

/// Return whether a collection-relative path should be included in indexing.
pub(crate) fn should_index_rel_path(collection: &Collection, rel_path: &str) -> bool {
    collection.is_record_path(rel_path)
}

pub(crate) fn uri_from_rel_path(collection: &Collection, rel_path: &str) -> Option<Url> {
    Url::from_file_path(collection.root().join(rel_path)).ok()
}

fn strip_wrapping_quotes(value: &str) -> (&str, Option<char>) {
    if value.len() >= 2 {
        let first = value.as_bytes()[0] as char;
        let last = value.as_bytes()[value.len() - 1] as char;
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            return (&value[1..value.len() - 1], Some(first));
        }
    }
    (value, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_link_value_strips_quotes_and_wikilink() {
        assert_eq!(
            parse_link_value("\"[[notes/alice#bio|Alice]]\""),
            Some("notes/alice".to_string())
        );
    }

    #[test]
    fn parse_link_value_strips_bare_anchor() {
        assert_eq!(
            parse_link_value("'notes/alice.md#bio'"),
            Some("notes/alice.md".to_string())
        );
    }
}
