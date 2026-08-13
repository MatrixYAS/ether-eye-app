// Core domain types for the deterministic arbitrage pipeline.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Base58-encoded public key / account address.
pub type Pubkey = String;
/// Lamport amount.
pub type Lamports = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Venue {
    RaydiumCpmm,
    RaydiumClmm,
    OrcaWhirlpool,
    MeteoraDlmm,
    Phoenix,
}

impl std::fmt::Display for Venue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Venue::RaydiumCpmm => write!(f, "Raydium CPMM"),
            Venue::RaydiumClmm => write!(f, "Raydium CLMM"),
            Venue::OrcaWhirlpool => write!(f, "Orca Whirlpool"),
            Venue::MeteoraDlmm => write!(f, "Meteora DLMM"),
            Venue::Phoenix => write!(f, "Phoenix OB"),
        }
    }
}

/// Raw account snapshot fetched from RPC, plus decoding metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawAccount {
    pub pubkey: Pubkey,
    pub lamports: Lamports,
    pub owner: Pubkey,
    pub data_b64: String,
    pub executable: bool,
    /// Slot at which this account state was observed.
    pub context_slot: u64,
}

/// Decoded market state, the single source of truth for quoting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketState {
    pub venue: Venue,
    pub market_address: Pubkey,
    /// Mint A and Mint B of the pool/book (canonical order depends on venue).
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub decimals_a: u8,
    pub decimals_b: u8,
    /// Mid / reference price in human units: units of B per unit of A.
    pub price_b_per_a: f64,
    /// Fee expressed as numerator/denominator of input amount.
    pub fee_num: u64,
    pub fee_den: u64,
    /// Available liquidity at the reference price (in quote token human units).
    pub liquidity_quote: f64,
    /// Venue-specific detail retained for auditing (tick arrays, book snapshot, ...).
    pub detail: MarketDetail,
    /// Slot + wall-clock at which this state was captured.
    pub slot: u64,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarketDetail {
    Cpmm { reserve_a: u64, reserve_b: u64 },
    Clmm {
        sqrt_price_x64: f64,
        liquidity: u128,
        /// tick-indexed liquidities and fees crossed by a quote.
        ticks_crossed: Vec<TickSegment>,
        tick_spacing: i32,
        current_tick: i32,
    },
    PhoenixBook {
        levels: Vec<BookLevel>,
        best_bid: f64,
        best_ask: f64,
    },
    Dlmm {
        bins_crossed: Vec<BinSegment>,
        active_bin: i64,
        bin_step_bps: u64,
    },
}

/// A price range of a CLMM quote traversal with the liquidity active in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickSegment {
    pub start_tick: i32,
    pub end_tick: i32,
    pub liquidity: u128,
    pub fee_growth_in: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinSegment {
    pub start_bin: i64,
    pub end_bin: i64,
    pub liquidity: u128,
}

/// One price level in a Phoenix order book snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: f64,
    pub quantity_base: f64,
    pub side: Side,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Bid,
    Ask,
}

/// Result of quoting one leg of a route against a specific venue/market.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegQuote {
    pub venue: Venue,
    pub market_address: Pubkey,
    pub token_in: Pubkey,
    pub token_out: Pubkey,
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee_lamports: u64,
    /// Decimals of the INPUT token (fee_lamports and amount_in are in these units).
    pub decimals_in: u8,
    /// Decimals of the OUTPUT token (amount_out is in these units).
    pub decimals_out: u8,
    /// Output-token price in USD per human unit (used to value the leg output).
    pub usd_per_output_unit: f64,
    pub price_impact_bps: f64,
    pub minimum_output: u64,
    pub available_liquidity: f64,
    pub quote_slot: u64,
    pub quote_ts: DateTime<Utc>,
    /// Human-readable description of how this number was computed.
    pub source: String,
    /// True if the full amount could be absorbed by available liquidity.
    pub liquidity_ok: bool,
}

impl LegQuote {
    /// USD value of one HUMAN unit of this leg's INPUT token.
    /// SOL (9 decimals) is priced via `sol_usd_price`; USDC (6 decimals) is
    /// assumed at $1.00 (oracle-free reference, consistent with the engine's
    /// USD accounting policy). Use after converting base units to human units
    /// (`base / 10^decimals`).
    pub fn usd_per_input_unit(&self, sol_usd_price: f64) -> f64 {
        if self.decimals_in == 9 {
            sol_usd_price
        } else {
            1.0
        }
    }

    /// USD value of one HUMAN unit of this leg's OUTPUT token.
    pub fn usd_per_output_unit_fn(&self, sol_usd_price: f64) -> f64 {
        if self.decimals_out == 9 {
            sol_usd_price
        } else {
            1.0
        }
    }
}

/// A candidate two-legged arbitrage route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateRoute {
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub leg_a: LegQuote,
    pub leg_b: LegQuote,
    pub quote_age_ms: u64,
    pub state_slot: u64,
}

/// Full per-opportunity cost accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostBreakdown {
    pub gross_profit_usd: f64,
    pub dex_fees_usd: f64,
    pub price_impact_usd: f64,
    pub network_base_fee_usd: f64,
    pub priority_fee_usd: f64,
    pub tip_allowance_usd: f64,
    pub safety_buffer_usd: f64,
    pub net_profit_usd: f64,
    pub profit_bps: f64,
    pub jito_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Profitable,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RejectionReason {
    QuoteStale { age_ms: u64, max_age_ms: u64 },
    InsufficientLiquidity { venue: String, needed: f64, available: f64 },
    HighPriceImpact { impact_bps: f64, threshold_bps: f64 },
    FeeErasesProfit { fees_usd: f64, gross_usd: f64 },
    SecondQuoteInvalidated { first_usd: f64, second_usd: f64 },
    BelowProfitFloor { net_usd: f64, floor_usd: f64 },
    RouteNotExecutable { detail: String },
}

impl std::fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RejectionReason::QuoteStale { age_ms, max_age_ms } =>
                write!(f, "Quote became stale (age {age_ms}ms > max {max_age_ms}ms)"),
            RejectionReason::InsufficientLiquidity { venue, needed, available } =>
                write!(f, "Insufficient liquidity on {venue} (needed {needed:.2}, available {available:.2} USD)"),
            RejectionReason::HighPriceImpact { impact_bps, threshold_bps } =>
                write!(f, "Price impact reduced profit below threshold ({impact_bps:.0}bps > {threshold_bps:.0}bps)"),
            RejectionReason::FeeErasesProfit { fees_usd, gross_usd } =>
                write!(f, "Fees erase profit (fees ${fees_usd:.2} >= gross ${gross_usd:.2})"),
            RejectionReason::SecondQuoteInvalidated { first_usd, second_usd } =>
                write!(f, "Second quote invalidated opportunity (${first_usd:.2} → ${second_usd:.2})"),
            RejectionReason::BelowProfitFloor { net_usd, floor_usd } =>
                write!(f, "Net profit ${net_usd:.2} below floor ${floor_usd:.2} after safety buffer"),
            RejectionReason::RouteNotExecutable { detail } =>
                write!(f, "Route not executable: {detail}"),
        }
    }
}

/// Final verdict for a candidate route with full audit data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityVerdict {
    pub decision: Decision,
    pub route: CandidateRoute,
    pub size_usd: f64,
    pub optimal_size_usd: Option<f64>,
    pub costs: CostBreakdown,
    pub first_check_usd: f64,
    pub second_check_usd: Option<f64>,
    pub verification_status: VerificationStatus,
    pub rejection: Option<RejectionReason>,
    pub confidence: u8,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationStatus {
    Pending,
    SingleVerified,
    DoubleVerified,
    Invalidated,
}
