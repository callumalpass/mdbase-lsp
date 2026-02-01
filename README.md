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
the LSP.  The command calls `mdbase.typeInfo` first to discover which fields
need user input (required, no default, no generated strategy), then prompts
for each before creating the file.

```lua
-- Prompt for a list of fields sequentially, then call done(values).
local function prompt_fields(fields, idx, values, done)
  if idx > #fields then
    done(values)
    return
  end
  local field = fields[idx]
  local label = field.name
  if field.description and field.description ~= "" then
    label = label .. " (" .. field.description .. ")"
  end
  vim.ui.input({ prompt = label .. ": " }, function(value)
    if value == nil then return end
    if value ~= "" then
      values[field.name] = value
    end
    prompt_fields(fields, idx + 1, values, done)
  end)
end

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

  local function create_with_type(type_name)
    client:request("workspace/executeCommand", {
      command = "mdbase.typeInfo",
      arguments = { { type = type_name } },
    }, function(err, result)
      if err then
        vim.schedule(function()
          vim.notify("mdbase: " .. tostring(err), vim.log.levels.ERROR)
        end)
        return
      end
      local fields = (result and result.prompt_fields) or {}

      vim.schedule(function()
        vim.ui.input({ prompt = "File path (blank to auto-generate): " }, function(file_path)
          if file_path == nil then return end

          prompt_fields(fields, 1, {}, function(values)
            local fm = next(values) and values or vim.empty_dict()
            local args = { type = type_name, frontmatter = fm }
            if file_path ~= "" then
              args.path = file_path
            end
            client:request("workspace/executeCommand", {
              command = "mdbase.createFile",
              arguments = { args },
            })
          end)
        end)
      end)
    end)
  end

  if #type_names > 0 then
    vim.ui.select(type_names, { prompt = "Select a type:" }, function(type_name)
      if not type_name or type_name == "" then return end
      create_with_type(type_name)
    end)
  else
    vim.ui.input({ prompt = "Type name: " }, function(type_name)
      if not type_name or type_name == "" then return end
      create_with_type(type_name)
    end)
  end
end, {})
```

The flow is: select type → optional file path → prompted for any required
fields without defaults → file created and opened. Plugins like
`dressing.nvim` or `telescope` will enhance the prompts.

## Notes

- Diagnostics are mapped to frontmatter field lines when possible.
- Link hover/definition uses saved file state because `mdbase-rs` resolves
  links from the file system.
- Tag completion merges frontmatter `tags` with inline tags from body text.

## Development

This project is intentionally thin: all spec logic lives in `mdbase-rs`.
If you need new APIs, add them to `mdbase-rs` and call them here.
