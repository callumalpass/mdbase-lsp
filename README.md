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

#### lazy.nvim (auto-download latest release)

```lua
local function mdbase_lsp_paths()
  local data = vim.fn.stdpath("data")
  local dir = data .. "/mdbase-lsp"
  local bin = dir .. "/mdbase-lsp"
  if vim.fn.has("win32") == 1 then
    bin = bin .. ".exe"
  end
  return dir, bin
end

local function detect_target()
  if vim.fn.has("win32") == 1 then
    return "win32-x64"
  end
  if vim.fn.has("mac") == 1 then
    local arch = vim.fn.system({ "uname", "-m" }):gsub("%s+", "")
    if arch == "arm64" or arch == "aarch64" then
      return "darwin-arm64"
    end
    return "darwin-x64"
  end
  return "linux-x64"
end

local function latest_release_asset()
  local json = vim.fn.system({
    "curl",
    "-sL",
    "https://api.github.com/repos/callumalpass/mdbase-lsp/releases/latest",
  })
  local ok, data = pcall(vim.fn.json_decode, json)
  if not ok or type(data) ~= "table" then
    return nil, nil
  end

  local target = detect_target()
  local name = ({
    ["linux-x64"] = "mdbase-lsp-linux-x64.tar.gz",
    ["darwin-x64"] = "mdbase-lsp-darwin-x64.tar.gz",
    ["darwin-arm64"] = "mdbase-lsp-darwin-arm64.tar.gz",
    ["win32-x64"] = "mdbase-lsp-win32-x64.zip",
  })[target]

  for _, asset in ipairs(data.assets or {}) do
    if asset.name == name then
      return asset.browser_download_url, name
    end
  end

  return nil, nil
end

local function install_mdbase_lsp()
  local dir, bin = mdbase_lsp_paths()
  vim.fn.mkdir(dir, "p")

  local url, name = latest_release_asset()
  if not url or not name then
    vim.notify("mdbase-lsp: failed to find release asset", vim.log.levels.ERROR)
    return
  end

  local archive = dir .. "/" .. name
  vim.fn.system({ "curl", "-fL", "-o", archive, url })

  if vim.fn.has("win32") == 1 then
    local cmd = ("Expand-Archive -Force -Path '%s' -DestinationPath '%s'"):format(archive, dir)
    vim.fn.system({ "powershell", "-NoProfile", "-Command", cmd })
  else
    vim.fn.system({ "tar", "xzf", archive, "-C", dir })
    vim.fn.system({ "chmod", "+x", bin })
  end
end

require("lazy").setup({
  {
    "callumalpass/mdbase-lsp",
    build = install_mdbase_lsp,
  },
})
```

Use `:Lazy build mdbase-lsp` to force a refresh of the binary.

```lua
local data_dir = vim.fn.stdpath("data")
vim.lsp.config("mdbase", {
  cmd = { data_dir .. "/mdbase-lsp/mdbase-lsp" },
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
