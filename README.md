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

## Editor Setup

### VS Code

Install the extension from `editors/vscode/`. It registers the
`mdbase.createFile` and `mdbase.validateCollection` commands automatically.

### Neovim (0.11+)

```lua
vim.lsp.config("mdbase", {
  cmd = { "/path/to/mdbase-lsp" },
  filetypes = { "markdown" },
  root_markers = { ".mdbase", ".git" },
  capabilities = {
    workspace = {
      didChangeWatchedFiles = { dynamicRegistration = true },
    },
  },
})

vim.lsp.enable("mdbase")
```

#### Commands

Create a `:MdbaseCreateFile` user command to invoke `mdbase.createFile` via
the LSP:

```lua
vim.api.nvim_create_user_command("MdbaseCreateFile", function()
  local clients = vim.lsp.get_clients({ name = "mdbase" })
  if #clients == 0 then
    vim.notify("mdbase LSP not attached", vim.log.levels.ERROR)
    return
  end
  local client = clients[1]
  local root = client.root_dir or vim.fn.getcwd()
  local types_dir = root .. "/_types"

  local type_names = {}
  local ok, entries = pcall(vim.fn.readdir, types_dir)
  if ok then
    for _, f in ipairs(entries) do
      if f:match("%.md$") then
        table.insert(type_names, (f:gsub("%.md$", "")))
      end
    end
  end

  local function on_type(type_name)
    if not type_name or type_name == "" then return end
    vim.ui.input({ prompt = "File path (relative to collection root): " }, function(file_path)
      if not file_path or file_path == "" then return end
      client:request("workspace/executeCommand", {
        command = "mdbase.createFile",
        arguments = { { type = type_name, path = file_path, frontmatter = {} } },
      })
    end)
  end

  if #type_names > 0 then
    vim.ui.select(type_names, { prompt = "Select a type:" }, on_type)
  else
    vim.ui.input({ prompt = "Type name: " }, on_type)
  end
end, {})
```

This scans `_types/` for available types, prompts for a type and file path,
then sends the command to the LSP server. The server creates the file and
opens it via `showDocument`. Plugins like `dressing.nvim` or `telescope` will
enhance the `vim.ui.select` and `vim.ui.input` prompts.

## Notes

- Diagnostics are mapped to frontmatter field lines when possible.
- Link hover/definition uses saved file state because `mdbase-rs` resolves
  links from the file system.
- Tag completion merges frontmatter `tags` with inline tags from body text.

## Development

This project is intentionally thin: all spec logic lives in `mdbase-rs`.
If you need new APIs, add them to `mdbase-rs` and call them here.
