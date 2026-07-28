# Diagnostic adapter evidence

`mdbase-lsp` does not make an independent mdbase core profile claim. It uses
the `mdbase-rs` crate and inherits only behavior covered by that crate's
validated `conformance/v0.4.0-rc.2.yml` claim. Canonical view records are
ordinary schema-validated Markdown records; this adapter does not claim the
optional `view_records` execution feature.

The LSP-specific evidence is:

- `cargo test`: canonical v0.3 codes, severities, paths, fields, type names,
  schema locations, and source ranges are preserved in LSP diagnostics.
- `cargo clippy --all-targets --all-features -- -D warnings`: the adapter and
  server pass the strict lint gate.
- `cargo package --allow-dirty --list`: package contents exclude local
  collections and development state. Full package verification requires the
  exact `mdbase = 0.4.0-rc.2` crate to be available from the release registry.

Evidence last refreshed: 2026-07-26 on Linux x86_64 with Rust 1.94.0.
