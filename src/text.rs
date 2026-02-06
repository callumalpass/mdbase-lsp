use mdbase::frontmatter::parser::{is_parse_error, parse_document, yaml_mapping_to_json};

#[derive(Clone)]
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
) -> (
    tower_lsp::lsp_types::Position,
    tower_lsp::lsp_types::Position,
) {
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
                    let start_pos =
                        tower_lsp::lsp_types::Position::new(line_idx as u32, start_col as u32);
                    let end_pos =
                        tower_lsp::lsp_types::Position::new(line_idx as u32, end_col as u32);
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
    if line.is_empty() {
        return None;
    }
    // Clamp column to last valid index so cursor at end-of-line still finds the word
    let col = column.min(line.len().saturating_sub(1));
    let bytes = line.as_bytes();
    let mut start = col;
    while start > 0 && is_word_char(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = col;
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

// ---------------------------------------------------------------------------
// Link detection at cursor position
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct LinkAtCursor {
    pub target: String,
    pub start_col: usize,
    pub end_col: usize,
}

/// Scan `line_idx` of `text` for a link that spans `column`.
///
/// Detects `[[target]]`, `![[target]]`, `[text](path)`, `![alt](path)`.
/// Skips external URLs (`http://`, `https://`).
pub(crate) fn link_at_position(text: &str, line_idx: usize, column: usize) -> Option<LinkAtCursor> {
    let line = text.lines().nth(line_idx)?;
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Wikilink / embed: [[target]] or ![[target]]
        if i + 1 < len && chars[i] == '[' && chars[i + 1] == '[' {
            let link_start = if i > 0 && chars[i - 1] == '!' {
                i - 1
            } else {
                i
            };
            i += 2; // skip [[
            let content_start = i;
            while i < len && !(chars[i] == ']' && i + 1 < len && chars[i + 1] == ']') {
                i += 1;
            }
            if i < len {
                let content: String = chars[content_start..i].iter().collect();
                let link_end = i + 2; // past ]]
                i = link_end;

                if column >= link_start && column < link_end {
                    let target = content.split('|').next().unwrap_or(&content);
                    let target = target.split('#').next().unwrap_or(target).trim();
                    if !target.is_empty() {
                        return Some(LinkAtCursor {
                            target: target.to_string(),
                            start_col: link_start,
                            end_col: link_end,
                        });
                    }
                }
            } else {
                break;
            }
            continue;
        }

        // Markdown link / image: [text](path) or ![alt](path)
        if chars[i] == '[' {
            let link_start = if i > 0 && chars[i - 1] == '!' {
                i - 1
            } else {
                i
            };
            i += 1; // skip [
            let mut bracket_depth = 1;
            while i < len && bracket_depth > 0 {
                if chars[i] == '[' {
                    bracket_depth += 1;
                }
                if chars[i] == ']' {
                    bracket_depth -= 1;
                }
                i += 1;
            }
            // Expect (path) immediately after ]
            if i < len && chars[i] == '(' {
                i += 1; // skip (
                let paren_start = i;
                let mut paren_depth = 1;
                while i < len && paren_depth > 0 {
                    if chars[i] == '(' {
                        paren_depth += 1;
                    }
                    if chars[i] == ')' {
                        paren_depth -= 1;
                    }
                    i += 1;
                }
                let path: String = chars[paren_start..i - 1].iter().collect();
                let link_end = i;

                if column >= link_start && column < link_end {
                    let path = path.trim();
                    if !path.is_empty()
                        && !path.starts_with("http://")
                        && !path.starts_with("https://")
                    {
                        let target = path.split('#').next().unwrap_or(path).to_string();
                        if !target.is_empty() {
                            return Some(LinkAtCursor {
                                target,
                                start_col: link_start,
                                end_col: link_end,
                            });
                        }
                    }
                }
                continue;
            }
            continue;
        }

        i += 1;
    }

    None
}

// ---------------------------------------------------------------------------
// Frontmatter value extraction
// ---------------------------------------------------------------------------

/// Extract the value portion of a frontmatter line if the cursor is past the
/// field delimiter.
///
/// - `field: value` → returns `"value"` (trimmed) when cursor is past `:`
/// - `  - value`    → returns `"value"` (trimmed) when cursor is past `-`
pub(crate) fn value_from_frontmatter_line(line: &str, column: usize) -> Option<String> {
    let trimmed = line.trim_start();
    let leading = line.len() - trimmed.len();

    // `field: value` form
    if let Some(colon_idx) = trimmed.find(':') {
        let abs_colon = leading + colon_idx;
        if column > abs_colon {
            let value = trimmed[colon_idx + 1..].trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
        return None;
    }

    // `  - value` form (list item)
    if trimmed.starts_with('-') {
        let after_dash = &trimmed[1..];
        let dash_abs = leading; // position of the dash
        let value_start = dash_abs + 1 + (after_dash.len() - after_dash.trim_start().len());
        if column > dash_abs {
            let value = after_dash.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
        let _ = value_start; // suppress unused warning
        return None;
    }

    None
}

// ---------------------------------------------------------------------------
// Link completion context detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LinkCompletionKind {
    Wikilink,
    Markdown,
}

pub(crate) struct LinkCompletionContext {
    pub kind: LinkCompletionKind,
    pub prefix: String,
    pub start_col: usize,
}

/// Detect whether the cursor is inside an incomplete link and return context
/// for providing file completions.
///
/// Scans backwards from `column` on `line` for `[[` (wikilink/embed) or `](`
/// (markdown link). Returns `None` if the link is already closed, the prefix
/// contains `#` or `|` (anchor/alias), or the target is an external URL.
pub(crate) fn link_completion_context(line: &str, column: usize) -> Option<LinkCompletionContext> {
    let before: String = line.chars().take(column).collect();

    // Look for wikilink opener `[[` (or `![[`)
    if let Some(pos) = before.rfind("[[") {
        let after_open = &before[pos + 2..];
        // Already closed?
        if after_open.contains("]]") {
            // fall through to check markdown link
        } else if after_open.contains('#') || after_open.contains('|') {
            return None;
        } else {
            return Some(LinkCompletionContext {
                kind: LinkCompletionKind::Wikilink,
                prefix: after_open.to_string(),
                start_col: pos + 2,
            });
        }
    }

    // Look for markdown link opener `](`
    if let Some(pos) = before.rfind("](") {
        let after_open = &before[pos + 2..];
        // Already closed?
        if after_open.contains(')') {
            return None;
        }
        if after_open.contains('#') {
            return None;
        }
        // Skip external URLs
        let trimmed = after_open.trim_start();
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            return None;
        }
        return Some(LinkCompletionContext {
            kind: LinkCompletionKind::Markdown,
            prefix: after_open.to_string(),
            start_col: pos + 2,
        });
    }

    None
}

/// Find the field name that owns the value on `line_idx`.
///
/// For `field: value` lines, returns the field name directly.
/// For list items (`  - value`), walks backwards to find the parent `field:` line.
pub(crate) fn field_name_for_position(text: &str, line_idx: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let line = lines.get(line_idx)?;
    let trimmed = line.trim_start();

    // Direct `field: value` line
    if let Some(colon_idx) = trimmed.find(':') {
        let name = trimmed[..colon_idx].trim();
        if !name.is_empty() && !name.starts_with('-') {
            return Some(name.to_string());
        }
    }

    // List item — walk backwards to find the parent field
    if trimmed.starts_with('-') {
        let item_indent = line.len() - trimmed.len();
        for prev_idx in (0..line_idx).rev() {
            let prev = lines[prev_idx];
            let prev_trimmed = prev.trim_start();
            let prev_indent = prev.len() - prev_trimmed.len();

            // Must be less indented and have a colon
            if prev_indent < item_indent {
                if let Some(colon_idx) = prev_trimmed.find(':') {
                    let name = prev_trimmed[..colon_idx].trim();
                    if !name.is_empty() && !name.starts_with('-') {
                        return Some(name.to_string());
                    }
                }
                // If less indented but no colon, stop searching
                break;
            }
        }
    }

    None
}
