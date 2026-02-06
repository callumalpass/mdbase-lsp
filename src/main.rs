use tower_lsp::{LspService, Server};
use tracing_subscriber::EnvFilter;

mod body_links;
mod code_actions;
mod collection_utils;
mod commands;
mod completions;
mod diagnostics;
mod document_links;
mod file_index;
mod goto;
mod hover;
mod link_resolve;
mod references;
mod server;
mod state;
mod symbols;
mod text;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(server::MdbaseLanguageServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
