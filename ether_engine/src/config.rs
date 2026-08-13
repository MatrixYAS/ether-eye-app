// Configuration for ether_engine.
//
// Required / recognized environment variables:
//   RPC_PROVIDER_URL      Solana RPC endpoint (public RPC is fine; swap for Helius/QuikNode later)
//   JITO_ENABLED          "1"/"true" to include the optional Jito tip in the cost model
//
// Thresholds (all configurable, with sane defaults):
//   MAX_QUOTE_AGE_MS      reject quotes older than this (default 500)
//   MIN_NET_PROFIT_USD    profit floor (default 1.00)
//   SAFETY_BUFFER_BPS     safety buffer in basis points applied to input amount (default 30)
//   PRIORITY_FEE_ASSUMPTION_LAMPORTS  fixed priority-fee allowance when not using live estimates (default 10_000)
//   JITO_TIP_ASSUMPTION_LAMPORTS      fixed Jito tip allowance (default 5_000_000)
//   ENGINE_PORT           localhost port for the engine HTTP API (default 8787)
//   DATA_DIR              directory holding the embedded SQLite database (default ./data)
//   SOL_USD_PRICE         oracle-free fallback price of SOL in USD for USD reporting (default 150.0)
//   USDC_DECIMALS         decimals of the tracked quote token (default 6)
//   INPUT_TOKEN           mint of the input token to scan (default USDC)
//   SCANNER_TICK_MS       loop cadence (default 2000)
//   DEMO_MODE             "1"/"true" runs the deterministic demo dataset when RPC is unreachable
use serde::{Deserialize, Serialize};
use std::env;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub rpc_provider_url: String,
    pub jito_enabled: bool,
    pub max_quote_age_ms: u64,
    pub min_net_profit_usd: f64,
    pub safety_buffer_bps: u64,
    pub priority_fee_assumption_lamports: u64,
    pub jito_tip_assumption_lamports: u64,
    pub engine_port: u16,
    pub data_dir: String,
    pub sol_usd_price: f64,
    pub scanner_tick_ms: u64,
    pub demo_mode: bool,
    pub use_live_priority_fees: bool,
}

fn env_bool(name: &str) -> bool {
    matches!(
        env::var(name).as_deref().unwrap_or(""),
        "1" | "true" | "yes" | "on"
    )
}

fn env_parse<T: std::str::FromStr>(name: &str, default: T) -> T {
    env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

impl EngineConfig {
    pub fn from_env() -> Self {
        Self {
            rpc_provider_url: env::var("RPC_PROVIDER_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
            jito_enabled: env_bool("JITO_ENABLED"),
            max_quote_age_ms: env_parse("MAX_QUOTE_AGE_MS", 500),
            min_net_profit_usd: env_parse("MIN_NET_PROFIT_USD", 1.0),
            safety_buffer_bps: env_parse("SAFETY_BUFFER_BPS", 30),
            priority_fee_assumption_lamports: env_parse(
                "PRIORITY_FEE_ASSUMPTION_LAMPORTS",
                10_000,
            ),
            jito_tip_assumption_lamports: env_parse("JITO_TIP_ASSUMPTION_LAMPORTS", 5_000_000),
            engine_port: env_parse("ENGINE_PORT", 8787),
            data_dir: env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_string()),
            sol_usd_price: env_parse("SOL_USD_PRICE", 150.0),
            scanner_tick_ms: env_parse("SCANNER_TICK_MS", 2000),
            demo_mode: env_bool("DEMO_MODE"),
            use_live_priority_fees: env_bool("USE_LIVE_PRIORITY_FEES"),
        }
    }

    /// Tip cost in lamports according to the current cost model.
    pub fn tip_allowance_lamports(&self) -> u64 {
        if self.jito_enabled {
            self.jito_tip_assumption_lamports
        } else {
            0
        }
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            rpc_provider_url: "https://api.mainnet-beta.solana.com".to_string(),
            jito_enabled: false,
            max_quote_age_ms: 500,
            min_net_profit_usd: 1.0,
            safety_buffer_bps: 30,
            priority_fee_assumption_lamports: 10_000,
            jito_tip_assumption_lamports: 5_000_000,
            engine_port: 8787,
            data_dir: "./data".to_string(),
            sol_usd_price: 150.0,
            scanner_tick_ms: 2000,
            demo_mode: false,
            use_live_priority_fees: false,
        }
    }
}
