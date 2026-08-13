// RouteSimulator — full-cost accounting for a candidate two-legged route.
//
// Cost model (every assumption is explicit in CostBreakdown):
//   net_profit = gross_output_value − input_value
//              − dex_fees − price_impact_cost
//              − network_base_fee − priority_fee − tip
//              − safety_buffer (SAFETY_BUFFER_BPS × input_value)
//
// USD conversion uses the oracle-free SOL_USD_PRICE config value (never an
// external price API). Priority fees come from getRecentPrioritizationFees
// when USE_LIVE_PRIORITY_FEES is set, otherwise from the fixed assumption.
// The optional Jito tip is included only when JITO_ENABLED.

use crate::config::EngineConfig;
use crate::dex::DexAdapter;
use crate::error::EngineError;
use crate::types::{
    CandidateRoute, CostBreakdown, LegQuote, RejectionReason, Venue,
};

/// Base Solana transaction fee (5000 lamports) + estimated compute budget fee.
const BASE_TX_FEE_LAMPORTS: u64 = 5_000;
const COMPUTE_BUDGET_FEE_LAMPORTS: u64 = 1_000;

pub struct RouteSimulator<'a> {
    config: &'a EngineConfig,
    registry: &'a crate::dex::AdapterRegistry,
    /// Live priority-fee allowance in lamports, refreshed per slot if available.
    pub live_priority_fee: Option<u64>,
}

impl<'a> RouteSimulator<'a> {
    pub fn new(config: &'a EngineConfig, registry: &'a crate::dex::AdapterRegistry) -> Self {
        Self {
            config,
            registry,
            live_priority_fee: None,
        }
    }

    pub fn registry(&self) -> &'a crate::dex::AdapterRegistry {
        self.registry
    }

    pub fn refresh_priority_fees(&mut self, fees: Option<u64>) {
        self.live_priority_fee = fees;
    }

    fn priority_fee(&self) -> u64 {
        if self.config.use_live_priority_fees {
            self.live_priority_fee
                .unwrap_or(self.config.priority_fee_assumption_lamports)
        } else {
            self.config.priority_fee_assumption_lamports
        }
    }

    pub fn network_cost_usd(&self) -> f64 {
        let total_lamports =
            BASE_TX_FEE_LAMPORTS + COMPUTE_BUDGET_FEE_LAMPORTS + self.priority_fee();
        self.lamports_to_usd(total_lamports)
    }

    pub fn tip_cost_usd(&self) -> f64 {
        self.lamports_to_usd(self.config.tip_allowance_lamports())
    }

    fn lamports_to_usd(&self, lamports: u64) -> f64 {
        lamports as f64 / 1e9 * self.config.sol_usd_price
    }

    /// Quote a single leg through the venue adapter.
    pub fn quote_leg(
        &self,
        venue: Venue,
        market: &crate::types::MarketState,
        token_in: &str,
        token_out: &str,
        amount_in: u64,
    ) -> Result<LegQuote, EngineError> {
        let adapter = self
            .registry
            .for_venue(venue)
            .ok_or_else(|| EngineError::NoMarkets)?;
        adapter.quote(market, &token_in.to_string(), &token_out.to_string(), amount_in)
    }

    /// Full accounting for a candidate route at a fixed input size.
    /// Returns the net USD and the full breakdown.
    pub fn simulate(&self, route: &CandidateRoute, input_value_usd: f64) -> CostBreakdown {
        let leg_a = &route.leg_a;
        let leg_b = &route.leg_b;

        // USD-basis accounting. Leg A input/output are both the quote token
        // (USDC, 6 decimals) in this canonical pair, so its multiplier is a
        // direct USD ratio. Leg B converts the intermediate token back to the
        // quote token; its output is converted to USD via the
        // oracle-free SOL_USD_PRICE value for SOL (9 decimals).
        let multiplier_a = if leg_a.amount_in > 0 {
            leg_a.amount_out as f64 / leg_a.amount_in as f64
        } else {
            0.0
        };
        // Leg B output is the route's starting token (SOL) after the round
        // trip — convert from its base units using the leg's output decimals
        // and USD valuation instead of a hardcoded SOL assumption.
        let leg_b_out_usd = leg_b.amount_out as f64 / 10f64.powi(leg_b.decimals_out as i32)
            * leg_b.usd_per_output_unit_fn(self.config.sol_usd_price);
        let gross = leg_b_out_usd - input_value_usd;

        // Adapter fees are quoted in each leg's INPUT-token base units; the
        // LegQuote decimals carry the unit context so conversion stays correct
        // regardless of which token the leg buys or sells.
        let leg_a_fee_usd =
            leg_a.fee_lamports as f64 / 10f64.powi(leg_a.decimals_in as i32) * leg_a.usd_per_input_unit(self.config.sol_usd_price);
        let leg_b_fee_usd =
            leg_b.fee_lamports as f64 / 10f64.powi(leg_b.decimals_in as i32) * leg_b.usd_per_input_unit(self.config.sol_usd_price);
        let dex_fees = leg_a_fee_usd + leg_b_fee_usd;
        let impact_a = leg_a.price_impact_bps / 10_000.0 * input_value_usd;
        // Leg B impact applies to the USD value crossing leg A into the
        // mid-token, valued via the leg's own output decimals.
        let leg_a_out_usd_for_impact = input_value_usd * multiplier_a * leg_a.usd_per_output_unit_fn(self.config.sol_usd_price);
        let impact_b = leg_b.price_impact_bps / 10_000.0 * leg_a_out_usd_for_impact;
        let impact = impact_a + impact_b;

        // network_cost_usd already bundles base fee + compute budget + the
        // priority fee, so do not subtract `priority` twice.
        let network = self.network_cost_usd();
        let tip = self.tip_cost_usd();
        let safety = input_value_usd * self.config.safety_buffer_bps as f64 / 10_000.0;

        let net = gross - dex_fees - impact - network - tip - safety;
        let profit_bps = if input_value_usd > 0.0 {
            net / input_value_usd * 10_000.0
        } else {
            0.0
        };

        CostBreakdown {
            gross_profit_usd: gross,
            dex_fees_usd: dex_fees,
            price_impact_usd: impact,
            network_base_fee_usd: self.lamports_to_usd(BASE_TX_FEE_LAMPORTS + COMPUTE_BUDGET_FEE_LAMPORTS),
            priority_fee_usd: self.lamports_to_usd(self.priority_fee()),
            tip_allowance_usd: tip,
            safety_buffer_usd: safety,
            net_profit_usd: net,
            profit_bps,
            jito_enabled: self.config.jito_enabled,
        }
    }

    /// First freshness/liquidity gate applied before simulation.
    pub fn pre_check(route: &CandidateRoute, cfg: &EngineConfig) -> Option<RejectionReason> {
        if route.quote_age_ms > cfg.max_quote_age_ms {
            return Some(RejectionReason::QuoteStale {
                age_ms: route.quote_age_ms,
                max_age_ms: cfg.max_quote_age_ms,
            });
        }
        // Liquidity gate in USD: `available_liquidity` is quoted by each venue
        // adapter in USD, so convert the leg's input size to USD before the
        // comparison. Leg A input is the route's USD stake; leg B input is the
        // intermediate token, whose USD value ≈ stake × leg_a multiplier.
        let mult_a = if route.leg_a.amount_in > 0 {
            route.leg_a.amount_out as f64 / route.leg_a.amount_in as f64
        } else {
            0.0
        };
        if !route.leg_a.liquidity_ok {
            return Some(RejectionReason::InsufficientLiquidity {
                venue: route.leg_a.venue.to_string(),
                needed: route.leg_a.amount_in as f64,
                available: route.leg_a.available_liquidity,
            });
        }
        if !route.leg_b.liquidity_ok {
            return Some(RejectionReason::InsufficientLiquidity {
                venue: route.leg_b.venue.to_string(),
                needed: (route.leg_a.amount_in as f64 * mult_a).min(route.leg_b.amount_in as f64),
                available: route.leg_b.available_liquidity,
            });
        }
        if route.leg_a.price_impact_bps > 200.0 {
            return Some(RejectionReason::HighPriceImpact {
                impact_bps: route.leg_a.price_impact_bps,
                threshold_bps: 200.0,
            });
        }
        if route.leg_b.price_impact_bps > 200.0 {
            return Some(RejectionReason::HighPriceImpact {
                impact_bps: route.leg_b.price_impact_bps,
                threshold_bps: 200.0,
            });
        }
        None
    }
}
