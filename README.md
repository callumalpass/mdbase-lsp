# mdbase-lsp

Language Server Protocol (LSP) server for the mdbase specification. It uses
`mdbase-rs` for all spec logic (types, matching, validation, links).

## Features

- Diagnostics: frontmatter parse errors, validation issues, unknown fields
- Completions: field names, enum values, booleans, link targets, tags
- Hover: field/type info and link target preview
- Go to definition: link targets and type definitions in `_types/`
- Commands: `mdbase.createFile`, `mdbase.validateCollection`

## Requirements

- Rust toolchain (stable)
- A valid mdbase collection (folder with `mdbase.yaml`)

## Build

```bash
cargo build
```

## Run (stdio)

```bash
cargo run
```

Your editor should launch the server via stdio. Use the resulting binary in
your editor's LSP config.

## Commands

### `mdbase.createFile`

Creates a new file using mdbase create semantics.

Example arguments:

```json
{
  "type": "note",
  "frontmatter": { "title": "Example" },
  "body": "Hello",
  "path": "notes/example.md"
}
```

### `mdbase.validateCollection`

Validates the entire collection and returns the JSON report from
`mdbase-rs`.

## Notes

- Diagnostics are mapped to frontmatter field lines when possible.
- Link hover/definition uses saved file state because `mdbase-rs` resolves
  links from the file system.
- Tag completion merges frontmatter `tags` with inline tags from body text.

## Development

This project is intentionally thin: all spec logic lives in `mdbase-rs`.
If you need new APIs, add them to `mdbase-rs` and call them here.
