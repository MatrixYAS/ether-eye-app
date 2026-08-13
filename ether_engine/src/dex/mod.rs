// DEX adapter architecture.
//
// Every venue implements the DexAdapter trait so adding a new venue later does
// not require touching the engine. All quoting math is pure and deterministic
// given the same market state and inputs — no RNG anywhere in this module.

pub mod clmm_math;
pub mod meteora_dlmm;
pub mod orca_whirlpool;
pub mod phoenix;
pub mod raydium_clmm;
pub mod raydium_cpmm;

use crate::error::EngineError;
use crate::rpc::accounts::DecodedPool;
use crate::types::{LegQuote, MarketState, Pubkey};
use chrono::{DateTime, Utc};

/// Trait every venue adapter must implement.
pub trait DexAdapter: Send + Sync {
    fn venue(&self) -> crate::types::Venue;

    /// Build human-readable market state from a decoded pool account.
    fn market_state_from_pool(&self, pool: &DecodedPool) -> Result<MarketState, EngineError>;

    /// Deterministic quote for one leg: swap `amount_in` of token_in for
    /// token_out given the market state at capture time.
    fn quote(
        &self,
        market: &MarketState,
        token_in: &Pubkey,
        token_out: &Pubkey,
        amount_in: u64,
    ) -> Result<LegQuote, EngineError>;

    /// Fee charged by the venue for this swap, in input-token lamports.
    fn calculate_fees(&self, market: &MarketState, amount_in: u64) -> Result<u64, EngineError>;

    /// Price impact in basis points for swapping amount_in through the venue.
    fn calculate_price_impact(
        &self,
        market: &MarketState,
        token_in: &Pubkey,
        amount_in: u64,
    ) -> Result<f64, EngineError>;

    /// Executable liquidity in USD for token_in at the current state.
    fn get_liquidity(&self, market: &MarketState, sol_usd: f64) -> Result<f64, EngineError>;
}

/// Registry of available adapters.
pub struct AdapterRegistry {
    adapters: Vec<Box<dyn DexAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self { adapters: Vec::new() }
    }

    pub fn register(&mut self, adapter: Box<dyn DexAdapter>) {
        self.adapters.push(adapter);
    }

    pub fn for_venue(&self, venue: crate::types::Venue) -> Option<&dyn DexAdapter> {
        self.adapters.iter().find(|a| a.venue() == venue).map(|a| a.as_ref())
    }

    pub fn adapters(&self) -> &[Box<dyn DexAdapter>] {
        &self.adapters
    }
}

/// Current wall clock as quoted timestamps are minted.
pub fn now_utc() -> DateTime<Utc> {
    Utc::now()
}
