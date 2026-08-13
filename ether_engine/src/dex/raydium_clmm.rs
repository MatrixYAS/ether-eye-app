// Raydium CLMM adapter — concentrated liquidity with tick-array traversal.
//
// Uses the shared CLMM math (crate::dex::clmm_math) to deterministically walk
// tick ranges and compute exact-in quotes, fees, price impact and liquidity.

use crate::dex::clmm_math::{simulate_clmm_swap, sqrt_price_to_f64, tick_for_price};
use crate::dex::DexAdapter;
use crate::error::EngineError;
use crate::rpc::accounts::DecodedPool;
use crate::types::{LegQuote, MarketDetail, MarketState, Pubkey, Venue};
use chrono::Utc;

pub struct RaydiumClmmAdapter {
    pub sol_usd: f64,
    pub usdc_mint: Pubkey,
    pub sol_mint: Pubkey,
    pub max_ticks_crossed: usize,
    /// Fee-erase threshold: reject quotes whose impact exceeds this (bps).
    pub impact_cap_bps: f64,
}

impl RaydiumClmmAdapter {
    pub fn new(sol_usd: f64, usdc_mint: Pubkey, sol_mint: Pubkey) -> Self {
        Self {
            sol_usd,
            usdc_mint,
            sol_mint,
            max_ticks_crossed: 8,
            impact_cap_bps: 200.0,
        }
    }
}

impl DexAdapter for RaydiumClmmAdapter {
    fn venue(&self) -> Venue {
        Venue::RaydiumClmm
    }

    fn market_state_from_pool(&self, pool: &DecodedPool) -> Result<MarketState, EngineError> {
        match pool {
            DecodedPool::RaydiumClmm {
                token_mint_a,
                token_mint_b,
                sqrt_price_x64,
                liquidity,
                tick_current,
                tick_spacing,
                fee_rate,
                slot,
            } => {
                let price = sqrt_price_to_f64(*sqrt_price_x64);
                Ok(MarketState {
                    venue: Venue::RaydiumClmm,
                    market_address: String::new(),
                    mint_a: token_mint_a.clone(),
                    mint_b: token_mint_b.clone(),
                    decimals_a: 9,
                    decimals_b: 6,
                    price_b_per_a: price,
                    fee_num: *fee_rate,
                    fee_den: 1_000_000,
                    liquidity_quote: (*liquidity as f64) * self.sol_usd * 0.01,
                    detail: MarketDetail::Clmm {
                        sqrt_price_x64: *sqrt_price_x64 as f64,
                        liquidity: *liquidity,
                        ticks_crossed: vec![],
                        tick_spacing: *tick_spacing as i32,
                        current_tick: *tick_current,
                    },
                    slot: *slot,
                    captured_at: Utc::now(),
                })
            }
            _ => Err(EngineError::Decode("not a CLMM pool".into())),
        }
    }

    fn quote(
        &self,
        market: &MarketState,
        token_in: &Pubkey,
        token_out: &Pubkey,
        amount_in: u64,
    ) -> Result<LegQuote, EngineError> {
        let zero_for_one = token_in == &market.mint_a && token_out == &market.mint_b;
        if !(zero_for_one
            || (token_in == &market.mint_b && token_out == &market.mint_a))
        {
            return Err(EngineError::NoMarkets);
        }

        let (dec_in, dec_out, fee_rate) = if zero_for_one {
            (market.decimals_a, market.decimals_b, market.fee_num)
        } else {
            (market.decimals_b, market.decimals_a, market.fee_num)
        };

        let (sqrt_price, liquidity, tick_current, tick_spacing) = match market.detail {
            MarketDetail::Clmm {
                sqrt_price_x64,
                liquidity,
                tick_spacing,
                current_tick,
                ..
            } => (sqrt_price_x64.sqrt(), liquidity, current_tick, tick_spacing),
            _ => return Err(EngineError::Decode("missing CLMM detail".into())),
        };

        let amount_in_f = amount_in as f64 / 10f64.powi(dec_in as i32);
        let fill = simulate_clmm_swap(
            sqrt_price,
            liquidity,
            tick_current,
            tick_spacing as i16,
            zero_for_one,
            amount_in_f,
            fee_rate,
            None,
            self.max_ticks_crossed,
        );

        let amount_out = (fill.amount_out_produced * 10f64.powi(dec_out as i32)).floor() as u64;
        let ideal_out = amount_in_f * market.price_b_per_a
            * if zero_for_one { 1.0 } else { 1.0 / market.price_b_per_a };
        let impact = if ideal_out > 0.0 {
            (1.0 - fill.amount_out_produced / ideal_out) * 10_000.0
        } else {
            0.0
        };

        let now = Utc::now();
        Ok(LegQuote {
            venue: Venue::RaydiumClmm,
            market_address: market.market_address.clone(),
            token_in: token_in.clone(),
            token_out: token_out.clone(),
            amount_in,
            amount_out,
            fee_lamports: ((amount_in_f - fill.amount_in_consumed) * 10f64.powi(dec_in as i32))
                .floor() as u64,
            decimals_in: dec_in,
            decimals_out: dec_out,
            usd_per_output_unit: if dec_out == 9 { self.sol_usd } else { 1.0 },
            price_impact_bps: impact,
            minimum_output: (fill.amount_out_produced * 0.995 * 10f64.powi(dec_out as i32))
                .floor() as u64,
            available_liquidity: if dec_out == 9 { liquidity as f64 * 1e-9 * self.sol_usd } else { liquidity as f64 * 1e-9 },
            quote_slot: market.slot,
            quote_ts: now,
            source: format!(
                "Raydium CLMM tick traversal: {} ticks × {} spacing, fee {}bps, L={}, sqrtP→{} (price {price:.4})",
                fill.ticks_crossed,
                tick_spacing,
                fee_rate / 100,
                liquidity,
                if zero_for_one { "↓" } else { "↑" },
                price = sqrt_price * sqrt_price,
            ),
            liquidity_ok: fill.fully_filled && impact < self.impact_cap_bps,
        })
    }

    fn calculate_fees(&self, market: &MarketState, amount_in: u64) -> Result<u64, EngineError> {
        let q = self.quote(market, &market.mint_a, &market.mint_b, amount_in)?;
        Ok(q.fee_lamports)
    }

    fn calculate_price_impact(
        &self,
        market: &MarketState,
        token_in: &Pubkey,
        amount_in: u64,
    ) -> Result<f64, EngineError> {
        Ok(self
            .quote(market, token_in, &market.mint_b, amount_in)?
            .price_impact_bps)
    }

    fn get_liquidity(&self, market: &MarketState, _sol_usd: f64) -> Result<f64, EngineError> {
        Ok(market.liquidity_quote)
    }
}
