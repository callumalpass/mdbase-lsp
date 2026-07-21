# v0.3 diagnostic adapter evidence

`mdbase-lsp` does not make an independent mdbase core profile claim. It uses
the `mdbase-rs` crate and inherits only behavior covered by that crate's
validated `conformance/v0.3.0-rc.1.yml` claim. Canonical view records are
ordinary schema-validated Markdown records; this adapter does not claim the
optional `view_records` execution feature.

The LSP-specific evidence is:

- `cargo test`: canonical v0.3 codes, severities, paths, fields, type names,
  schema locations, and source ranges are preserved in LSP diagnostics.
- `cargo clippy --all-targets --all-features -- -D warnings`: the adapter and
  server pass the strict lint gate.
- `cargo package --allow-dirty --list`: package contents exclude local
  collections and development state. `cargo package` currently stops at
  dependency resolution because the exact `mdbase = 0.3.0-rc.1` crate is not
  published; full package verification therefore runs after the Rust core
  prerelease is available from the release registry.

Evidence last refreshed: 2026-07-16T22:27:44+10:00 on Linux x86_64 with Rust
1.94.0.
