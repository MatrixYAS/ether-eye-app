// ether_engine — deterministic Solana paper-arbitrage scanner.
//
// Entry point: loads EngineConfig from environment, builds the adapter
// registry, initialises the embedded SQLite store, and starts the scanner
// loop alongside the Axum HTTP API.

pub mod api;
pub mod config;
pub mod dex;
pub mod engine;
pub mod error;
pub mod optimizer;
pub mod rpc;
pub mod simulator;
pub mod store;
pub mod types;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let cfg = config::EngineConfig::from_env();
    tracing::info!(
        rpc = %cfg.rpc_provider_url,
        jito = cfg.jito_enabled,
        port = cfg.engine_port,
        "ether_engine starting"
    );
    engine::run(cfg).await?;
    Ok(())
}
