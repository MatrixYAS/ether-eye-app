use thiserror::Error;

#[derive(Error, Debug)]
pub enum EngineError {
    #[error("RPC failure: {0}")]
    Rpc(String),

    #[error("Account decode failure: {0}")]
    Decode(String),

    #[error("Simulation failure: {0}")]
    Simulation(String),

    #[error("Database failure: {0}")]
    Database(String),

    #[error("Configuration failure: {0}")]
    Config(String),

    #[error("No markets available for route")]
    NoMarkets,
}
