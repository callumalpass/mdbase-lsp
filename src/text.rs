use mdbase::frontmatter::parser::{is_parse_error, parse_document, yaml_mapping_to_json};

pub(crate) struct ParsedFrontmatter {
    pub json: serde_json::Value,
    pub has_frontmatter: bool,
    pub parse_error: bool,
    pub mapping_error: bool,
}

pub(crate) fn parse_frontmatter(text: &str) -> ParsedFrontmatter {
    let doc = parse_document(text);
    if let Some(ref fm) = doc.frontmatter {
        if is_parse_error(fm) {
            return ParsedFrontmatter {
                json: serde_json::json!({}),
                has_frontmatter: doc.has_frontmatter,
                parse_error: true,
                mapping_error: false,
            };
        }
    }

    match &doc.frontmatter {
        Some(serde_yaml::Value::Mapping(m)) => ParsedFrontmatter {
            json: yaml_mapping_to_json(m),
            has_frontmatter: doc.has_frontmatter,
            parse_error: false,
            mapping_error: false,
        },
        Some(serde_yaml::Value::Null) | None => ParsedFrontmatter {
            json: serde_json::json!({}),
            has_frontmatter: doc.has_frontmatter,
            parse_error: false,
            mapping_error: false,
        },
        Some(_) => ParsedFrontmatter {
            json: serde_json::json!({}),
            has_frontmatter: doc.has_frontmatter,
            parse_error: false,
            mapping_error: true,
        },
    }
}

pub(crate) fn frontmatter_bounds(text: &str) -> Option<(usize, usize)> {
    let mut lines = text.lines().enumerate();
    let (first_idx, first_line) = lines.next()?;
    if first_line.trim_end() != "---" {
        return None;
    }
    let mut close_idx: Option<usize> = None;
    for (idx, line) in lines {
        if line.trim_end() == "---" {
            close_idx = Some(idx);
            break;
        }
    }
    let close_idx = close_idx?;
    if close_idx <= first_idx + 1 {
        return None;
    }
    Some((first_idx + 1, close_idx - 1))
}

pub(crate) fn is_in_frontmatter(text: &str, line: usize) -> bool {
    match frontmatter_bounds(text) {
        Some((start, end)) => line >= start && line <= end,
        None => false,
    }
}

pub(crate) fn field_name_from_line(line: &str) -> Option<String> {
    let mut trimmed = line.trim_start();
    if trimmed.starts_with('-') {
        trimmed = trimmed.trim_start_matches('-').trim_start();
    }
    let colon_idx = trimmed.find(':')?;
    let name = trimmed[..colon_idx].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

pub(crate) fn find_field_range(
    text: &str,
    field: &str,
    fallback_line: usize,
) -> (tower_lsp::lsp_types::Position, tower_lsp::lsp_types::Position) {
    if let Some((start, end)) = frontmatter_bounds(text) {
        for (line_idx, line) in text.lines().enumerate() {
            if line_idx < start || line_idx > end {
                continue;
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with(field) {
                let after = trimmed[field.len()..].trim_start();
                if after.starts_with(':') {
                    let prefix_len = line.len() - trimmed.len();
                    let start_col = prefix_len;
                    let end_col = start_col + field.len();
                    let start_pos = tower_lsp::lsp_types::Position::new(line_idx as u32, start_col as u32);
                    let end_pos = tower_lsp::lsp_types::Position::new(line_idx as u32, end_col as u32);
                    return (start_pos, end_pos);
                }
            }
        }
    }

    let start_pos = tower_lsp::lsp_types::Position::new(fallback_line as u32, 0);
    let end_pos = tower_lsp::lsp_types::Position::new(fallback_line as u32, 0);
    (start_pos, end_pos)
}

pub(crate) fn word_at(line: &str, column: usize) -> Option<String> {
    if column >= line.len() {
        return None;
    }
    let bytes = line.as_bytes();
    let mut start = column;
    while start > 0 && is_word_char(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = column;
    while end < bytes.len() && is_word_char(bytes[end]) {
        end += 1;
    }
    if start == end {
        None
    } else {
        Some(line[start..end].to_string())
    }
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}
