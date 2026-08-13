// ether_engine library root — all modules are public so the integration test
// harness and future consumers can build deterministic pipelines by hand.

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
