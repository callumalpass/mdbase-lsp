/// Body link parser — finds wikilinks and markdown links in document body text
/// with UTF-16 column offsets for LSP compatibility.

/// The syntactic format of a body link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkFormat {
    Wikilink,
    Markdown,
}

/// A link found in the document body.
#[derive(Debug, Clone)]
pub(crate) struct BodyLink {
    /// The link target (filename or path, without anchor).
    pub target: String,
    /// Display alias (`[[target|alias]]`) or link text (`[text](path)`).
    pub alias: Option<String>,
    /// Fragment/anchor portion after `#`, if any.
    pub anchor: Option<String>,
    /// Whether this was a wikilink or markdown link.
    pub format: LinkFormat,
    /// 0-based line number.
    pub start_line: usize,
    /// 0-based UTF-16 column offset of the link start.
    pub start_col: usize,
    /// 0-based line number (always same as start_line for inline links).
    pub end_line: usize,
    /// 0-based UTF-16 column offset one past the link end.
    pub end_col: usize,
}

/// Scan the full document text and return all body links.
///
/// Skips links inside fenced code blocks and inline code spans.
/// Skips external URLs (`http://`, `https://`).
/// Skips image embeds (`![[...]]`, `![...](...)`) .
pub(crate) fn find_body_links(text: &str) -> Vec<BodyLink> {
    let mut links = Vec::new();
    let mut in_fenced_block = false;

    for (line_idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fenced_block = !in_fenced_block;
            continue;
        }
        if in_fenced_block {
            continue;
        }

        parse_line_links(line, line_idx, &mut links);
    }

    links
}

/// Find the body link at the given cursor position, if any.
pub(crate) fn body_link_at(text: &str, line: usize, col: usize) -> Option<BodyLink> {
    let mut in_fenced_block = false;

    for (line_idx, line_text) in text.lines().enumerate() {
        let trimmed = line_text.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fenced_block = !in_fenced_block;
            continue;
        }
        if in_fenced_block {
            continue;
        }

        if line_idx == line {
            let mut links = Vec::new();
            parse_line_links(line_text, line_idx, &mut links);
            return links.into_iter().find(|l| col >= l.start_col && col < l.end_col);
        }
    }

    None
}

/// Parse a single line for wikilinks and markdown links, appending to `out`.
///
/// Skips content inside inline code spans (backticks).
/// Skips image embeds and external URLs.
fn parse_line_links(line: &str, line_idx: usize, out: &mut Vec<BodyLink>) {
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Skip inline code spans
        if chars[i] == '`' {
            i += 1;
            while i < len && chars[i] != '`' {
                i += 1;
            }
            if i < len {
                i += 1; // skip closing backtick
            }
            continue;
        }

        // Wikilink: [[target]] or [[target|alias]] or [[target#anchor]]
        // Also detect ![[...]] (image embed) and skip it
        if i + 1 < len && chars[i] == '[' && chars[i + 1] == '[' {
            let is_embed = i > 0 && chars[i - 1] == '!';
            let link_start_utf16 = utf16_col(&chars, if is_embed { i - 1 } else { i });
            i += 2; // skip [[
            let content_start = i;
            while i < len && !(chars[i] == ']' && i + 1 < len && chars[i + 1] == ']') {
                i += 1;
            }
            if i >= len {
                break; // unclosed wikilink
            }
            let content: String = chars[content_start..i].iter().collect();
            i += 2; // skip ]]
            let link_end_utf16 = utf16_col(&chars, i);

            if is_embed {
                continue; // skip image embeds
            }

            if content.is_empty() {
                continue;
            }

            let (target_and_anchor, alias) = if let Some(pipe_pos) = content.find('|') {
                let alias = content[pipe_pos + 1..].trim().to_string();
                let target_part = content[..pipe_pos].trim().to_string();
                (target_part, Some(alias))
            } else {
                (content.trim().to_string(), None)
            };

            let (target, anchor) = split_anchor(&target_and_anchor);

            if !target.is_empty() {
                out.push(BodyLink {
                    target,
                    alias,
                    anchor,
                    format: LinkFormat::Wikilink,
                    start_line: line_idx,
                    start_col: link_start_utf16,
                    end_line: line_idx,
                    end_col: link_end_utf16,
                });
            }
            continue;
        }

        // Markdown link: [text](path) or ![alt](path)
        if chars[i] == '[' {
            let is_image = i > 0 && chars[i - 1] == '!';
            let link_start_utf16 = utf16_col(&chars, if is_image { i - 1 } else { i });
            i += 1; // skip [
            // Find matching ]
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
            // text portion is between the brackets (for alias)
            // check for (path) immediately after
            if i < len && chars[i] == '(' {
                // Extract the text portion for alias: from original [+1 to ]-1
                let text_start = if is_image {
                    link_start_utf16 // already accounts for !
                } else {
                    link_start_utf16
                };
                let _ = text_start;

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
                let link_end_utf16 = utf16_col(&chars, i);

                if is_image {
                    continue; // skip image embeds
                }

                let path = path.trim();
                if path.is_empty()
                    || path.starts_with("http://")
                    || path.starts_with("https://")
                {
                    continue;
                }

                let (target, anchor) = split_anchor(path);

                // Extract the link text between [ and ] for alias
                // Walk back from paren_start-2 (which is ]) to find the [
                // Simpler: re-scan the bracket content
                let bracket_open = if is_image {
                    // chars layout: ... ! [ text ] ( path )
                    // link_start_utf16 points to !
                    // actual [ is at link_start_utf16_chars + 1
                    link_start_utf16 + 1 // UTF-16 offset of [
                } else {
                    link_start_utf16
                };
                let _ = bracket_open;
                // The text between [ and ] — we already consumed it, extract from chars
                // bracket started at the [ we skipped, ended before paren
                // Actually let's just use the raw chars to extract text
                let bracket_content_start = if is_image {
                    // find the [ after !
                    let mut j = 0;
                    let mut utf16_count = 0;
                    while j < chars.len() && utf16_count < link_start_utf16 {
                        utf16_count += chars[j].len_utf16();
                        j += 1;
                    }
                    j + 2 // skip ! and [
                } else {
                    let mut j = 0;
                    let mut utf16_count = 0;
                    while j < chars.len() && utf16_count < link_start_utf16 {
                        utf16_count += chars[j].len_utf16();
                        j += 1;
                    }
                    j + 1 // skip [
                };
                let bracket_content_end = paren_start - 2; // before ](
                let alias = if bracket_content_start < bracket_content_end
                    && bracket_content_end <= chars.len()
                {
                    let s: String = chars[bracket_content_start..bracket_content_end]
                        .iter()
                        .collect();
                    let s = s.trim().to_string();
                    if s.is_empty() { None } else { Some(s) }
                } else {
                    None
                };

                if !target.is_empty() {
                    out.push(BodyLink {
                        target,
                        alias,
                        anchor,
                        format: LinkFormat::Markdown,
                        start_line: line_idx,
                        start_col: link_start_utf16,
                        end_line: line_idx,
                        end_col: link_end_utf16,
                    });
                }
                continue;
            }
            // No ( after ] — not a markdown link, continue
            continue;
        }

        i += 1;
    }
}

/// Split a target string at `#` into (target, anchor).
fn split_anchor(s: &str) -> (String, Option<String>) {
    if let Some(hash_pos) = s.find('#') {
        let target = s[..hash_pos].trim().to_string();
        let anchor = s[hash_pos + 1..].trim().to_string();
        (target, if anchor.is_empty() { None } else { Some(anchor) })
    } else {
        (s.trim().to_string(), None)
    }
}

/// Compute the UTF-16 column offset for char index `char_idx` in `chars`.
fn utf16_col(chars: &[char], char_idx: usize) -> usize {
    chars[..char_idx].iter().map(|c| c.len_utf16()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikilink_simple() {
        let links = find_body_links("See [[target]] here.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "target");
        assert_eq!(links[0].alias, None);
        assert_eq!(links[0].anchor, None);
        assert_eq!(links[0].format, LinkFormat::Wikilink);
        assert_eq!(links[0].start_col, 4);
        assert_eq!(links[0].end_col, 14);
    }

    #[test]
    fn wikilink_with_alias() {
        let links = find_body_links("See [[target|display name]] here.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "target");
        assert_eq!(links[0].alias, Some("display name".to_string()));
    }

    #[test]
    fn wikilink_with_anchor() {
        let links = find_body_links("See [[target#section]] here.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "target");
        assert_eq!(links[0].anchor, Some("section".to_string()));
    }

    #[test]
    fn markdown_link() {
        let links = find_body_links("See [click here](path.md) for details.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "path.md");
        assert_eq!(links[0].format, LinkFormat::Markdown);
        assert_eq!(links[0].start_col, 4);
        assert_eq!(links[0].end_col, 25);
    }

    #[test]
    fn markdown_link_with_anchor() {
        let links = find_body_links("See [text](path.md#heading) here.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "path.md");
        assert_eq!(links[0].anchor, Some("heading".to_string()));
    }

    #[test]
    fn skips_image_embed_wikilink() {
        let links = find_body_links("![[image.png]]");
        assert_eq!(links.len(), 0);
    }

    #[test]
    fn skips_image_embed_markdown() {
        let links = find_body_links("![alt text](image.png)");
        assert_eq!(links.len(), 0);
    }

    #[test]
    fn skips_external_urls() {
        let links = find_body_links("[Google](https://google.com)");
        assert_eq!(links.len(), 0);
    }

    #[test]
    fn skips_fenced_code_block() {
        let text = "before\n```\n[[inside]]\n```\nafter [[outside]]";
        let links = find_body_links(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "outside");
    }

    #[test]
    fn skips_inline_code() {
        let links = find_body_links("See `[[not a link]]` and [[real]].");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "real");
    }

    #[test]
    fn body_link_at_cursor() {
        let text = "See [[target]] here.";
        assert!(body_link_at(text, 0, 6).is_some());
        assert!(body_link_at(text, 0, 0).is_none());
        assert!(body_link_at(text, 0, 14).is_none()); // one past end
    }

    #[test]
    fn multiple_links_on_line() {
        let text = "[[a]] and [[b]] and [c](d.md)";
        let links = find_body_links(text);
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].target, "a");
        assert_eq!(links[1].target, "b");
        assert_eq!(links[2].target, "d.md");
    }
}
