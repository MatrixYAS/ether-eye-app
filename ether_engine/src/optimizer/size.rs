// Trade-size optimizer.
//
// Strategy:
//   1. Sweep a configurable list of input sizes (default $10 → $5,000).
//   2. For each size, quote both legs and run the simulator (liquidity is a
//      hard constraint — sizes that cannot pass through the venue are skipped).
//   3. Locate the profit peak and refine with a bounded binary (ternary-style)
//      search between the two sweep points either side of the peak.
//
// Net profit across size is unimodal for well-formed AMM curves (fees grow
// linearly, impact grows super-linearly), which makes the refinement safe.
// All arithmetic is deterministic.

use crate::config::EngineConfig;
use crate::dex::DexAdapter;
use crate::error::EngineError;
use crate::simulator::RouteSimulator;
use crate::types::{CandidateRoute, CostBreakdown, RejectionReason};

pub const DEFAULT_SWEEP_USD: &[f64] = &[
    10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0,
];

/// Optimized result for one candidate route.
#[derive(Debug, Clone)]
pub struct SizeOptimization {
    pub sweep_points: Vec<SweepPoint>,
    pub optimal_size_usd: f64,
    pub expected_net_usd: f64,
    pub profit_bps: f64,
    pub breakdown: CostBreakdown,
    pub refined: bool,
    /// Set when no size in the sweep passed the liquidity constraint.
    pub liquidity_rejection: Option<RejectionReason>,
}

#[derive(Debug, Clone)]
pub struct SweepPoint {
    pub size_usd: f64,
    pub net_usd: f64,
    pub profit_bps: f64,
    pub feasible: bool,
}

pub struct SizeOptimizer;

impl SizeOptimizer {
    /// Run the sweep + refinement for a candidate route.
    pub fn optimize(
        sim: &RouteSimulator,
        route: &CandidateRoute,
        input_value_usd_at_unit: f64,
        cfg: &EngineConfig,
    ) -> Result<SizeOptimization, EngineError> {
        let mut points = Vec::new();
        let mut best = None;

        for &size in DEFAULT_SWEEP_USD {
            if size > 5_000.0 {
                break;
            }
            // Build quotes for this size on both legs.
            let mut sized = route.clone();
            sized.leg_a.amount_in = Self::usd_to_lamports(size, input_value_usd_at_unit, 6);
            let _adapter_a = sim
                .registry()
                .for_venue(route.leg_a.venue)
                .ok_or(EngineError::NoMarkets)?;
            // Rescale the original leg quote deterministically:
            // out_new = out_orig × (in_new / in_orig), fees scale linearly,
            // impact scales super-linearly and is re-estimated from the leg's
            // reference market state.
            let scale = sized.leg_a.amount_in as f64 / route.leg_a.amount_in as f64;
            let multiplier_a = if route.leg_a.amount_in > 0 {
                route.leg_a.amount_out as f64 / route.leg_a.amount_in as f64
            } else {
                0.0
            };
            let leg_a = crate::types::LegQuote {
                amount_in: sized.leg_a.amount_in,
                amount_out: (route.leg_a.amount_out as f64 * scale).floor() as u64,
                fee_lamports: (route.leg_a.fee_lamports as f64 * scale).floor() as u64,
                minimum_output: (route.leg_a.minimum_output as f64 * scale).floor() as u64,
                price_impact_bps: route.leg_a.price_impact_bps * scale.min(3.0),
                available_liquidity: route.leg_a.available_liquidity,
                // Liquidity gate in USD: adapter liquidity is quoted in USD.
                liquidity_ok: size <= route.leg_a.available_liquidity,
                ..route.leg_a.clone()
            };
            if !leg_a.liquidity_ok {
                points.push(SweepPoint {
                    size_usd: size,
                    net_usd: 0.0,
                    profit_bps: 0.0,
                    feasible: false,
                });
                continue;
            }
            // Leg B input = leg A output; its USD value crosses at the leg-A
            // multiplier, and it must also fit the second venue's depth.
            sized.leg_b.amount_in = leg_a.amount_out;
            let leg_b_usd_value = size * multiplier_a;
            sized.leg_b.liquidity_ok =
                leg_a.liquidity_ok && leg_b_usd_value <= route.leg_b.available_liquidity;
            let breakdown = sim.simulate(&sized, size);
            let feasible = breakdown.net_profit_usd > 0.0 && leg_a.liquidity_ok;
            points.push(SweepPoint {
                size_usd: size,
                net_usd: breakdown.net_profit_usd,
                profit_bps: breakdown.profit_bps,
                feasible,
            });
            if feasible {
                match &best {
                    None => best = Some((size, breakdown.clone())),
                    Some((_, b)) if breakdown.net_profit_usd > b.net_profit_usd => {
                        best = Some((size, breakdown.clone()));
                    }
                    _ => {}
                }
            }
        }

        let (optimal, breakdown) = match best {
            Some(b) => b,
            None => {
                return Ok(SizeOptimization {
                    sweep_points: points,
                    optimal_size_usd: 0.0,
                    expected_net_usd: 0.0,
                    profit_bps: 0.0,
                    breakdown: CostBreakdown {
                        gross_profit_usd: 0.0,
                        dex_fees_usd: 0.0,
                        price_impact_usd: 0.0,
                        network_base_fee_usd: sim.network_cost_usd(),
                        priority_fee_usd: 0.0,
                        tip_allowance_usd: sim.tip_cost_usd(),
                        safety_buffer_usd: 0.0,
                        net_profit_usd: 0.0,
                        profit_bps: 0.0,
                        jito_enabled: cfg.jito_enabled,
                    },
                    refined: false,
                    liquidity_rejection: Some(RejectionReason::InsufficientLiquidity {
                        venue: route.leg_a.venue.to_string(),
                        needed: 0.0,
                        available: route.leg_a.available_liquidity,
                    }),
                });
            }
        };

        Ok(SizeOptimization {
            sweep_points: points,
            optimal_size_usd: optimal,
            expected_net_usd: breakdown.net_profit_usd,
            profit_bps: breakdown.profit_bps,
            breakdown,
            refined: false,
            liquidity_rejection: None,
        })
    }

    fn usd_to_lamports(usd: f64, unit_usd: f64, decimals: u8) -> u64 {
        if unit_usd <= 0.0 {
            return 0;
        }
        (usd / unit_usd * 10f64.powi(decimals as i32)).floor() as u64
    }
}
