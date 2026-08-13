// Meteora DLMM adapter — bin-based concentrated liquidity.
//
// Layout decoded in crate::rpc::accounts::decode_meteora_dlmm:
//   token_x_mint, token_y_mint, active_id (i64), bin_step (u16).
//
// DLMM pricing model:
//   price(bin) = (1 + bin_step_bps/10_000)^bin
// A swap walks bins away from active_id; within each bin the liquidity
// available at that price is consumed. This adapter implements deterministic
// bin traversal with a configurable max_bins_crossed. When per-bin liquidity
// mapping is not loaded, it conservatively assumes the same liquidity across
// the walk (overestimate of fills → paired with a hard liquidity check).

use crate::dex::DexAdapter;
use crate::error::EngineError;
use crate::rpc::accounts::DecodedPool;
use crate::types::{BinSegment, LegQuote, MarketDetail, MarketState, Pubkey, Venue};
use chrono::Utc;

pub struct MeteoraDlmmAdapter {
    pub sol_usd: f64,
    pub usdc_mint: Pubkey,
    pub sol_mint: Pubkey,
    pub max_bins_crossed: usize,
    pub impact_cap_bps: f64,
}

impl MeteoraDlmmAdapter {
    pub fn new(sol_usd: f64, usdc_mint: Pubkey, sol_mint: Pubkey) -> Self {
        Self {
            sol_usd,
            usdc_mint,
            sol_mint,
            max_bins_crossed: 10,
            impact_cap_bps: 200.0,
        }
    }

    fn price_at_bin(active_id: i64, offset: i64, bin_step_bps: u64) -> f64 {
        // Absolute DLMM price at bin (active_id + offset), per the bin-id
        // price model `price = base^id` with `base = 1 + bin_step_bps/10^4`.
        // `active_id` is a bin-id log, so it stays in the low thousands for
        // realistic SOL/USDC prices — keep it small (the demo anchor is
        // derived from ln(price)/ln(base)).
        let base = 1.0 + bin_step_bps as f64 / 10_000.0;
        base.powi((active_id + offset) as i32)
    }
}

impl DexAdapter for MeteoraDlmmAdapter {
    fn venue(&self) -> Venue {
        Venue::MeteoraDlmm
    }

    fn market_state_from_pool(&self, pool: &DecodedPool) -> Result<MarketState, EngineError> {
        match pool {
            DecodedPool::MeteoraDlmm {
                token_x_mint,
                token_y_mint,
                active_id,
                bin_step,
                slot,
            } => {
                let bin_step_bps = (*bin_step as u64) * 5;
                let price = Self::price_at_bin(*active_id, 0, bin_step_bps);
                Ok(MarketState {
                    venue: Venue::MeteoraDlmm,
                    market_address: String::new(),
                    mint_a: token_x_mint.clone(),
                    mint_b: token_y_mint.clone(),
                    decimals_a: 9,
                    decimals_b: 6,
                    price_b_per_a: price,
                    // DLMM fees are ~half the bin step (2–12 bps typical).
                    fee_num: bin_step_bps / 2,
                    fee_den: 10_000,
                    liquidity_quote: 500_000.0,
                    detail: MarketDetail::Dlmm {
                        bins_crossed: vec![],
                        active_bin: *active_id,
                        bin_step_bps,
                    },
                    slot: *slot,
                    captured_at: Utc::now(),
                })
            }
            _ => Err(EngineError::Decode("not a Meteora DLMM pair".into())),
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

        let (active_bin, bin_step_bps) = match market.detail {
            MarketDetail::Dlmm {
                active_bin,
                bin_step_bps,
                ..
            } => (active_bin, bin_step_bps),
            _ => return Err(EngineError::Decode("missing DLMM detail".into())),
        };

        // Selling token_a (SOL→USDC) walks to higher bin ids where SOL is
        // dearer (more USDC per SOL); buying token_a walks to lower ids.
        let (dec_in, dec_out, sign) = if zero_for_one {
            (market.decimals_a, market.decimals_b, 1i64)
        } else {
            (market.decimals_b, market.decimals_a, -1i64)
        };

        let amount_in_f = amount_in as f64 / 10f64.powi(dec_in as i32);
        // Fee per DLMM fee model (fees are implicit in bin pricing; fee_num /
        // fee_den matches the decoded market-state fee convention).
        let fee_rate = market.fee_num as f64 / market.fee_den as f64;
        let amount_in_after = amount_in_f * (1.0 - fee_rate);

        // Walk bins: within each bin, price is constant; the quote token value
        // of bin liquidity = liquidity_at_bin * price_at_bin.
        // Without per-bin state loaded, use an estimated bin liquidity derived
        // from the advertised market liquidity spread across walked bins.
        let est_bin_liquidity_usd = market.liquidity_quote / (self.max_bins_crossed as f64);
        let mut remaining = amount_in_after;
        let mut produced = 0.0_f64;
        let mut bins_crossed = 0;
        let start_price = market.price_b_per_a;
        let mut end_price = start_price;

        for step in 1..=self.max_bins_crossed {
            if remaining <= 0.0 {
                break;
            }
            let offset = sign * step as i64;
            // Absolute USDC-per-SOL price at the walked bin (base^(id+offset)).
            let p = Self::price_at_bin(active_bin, offset, bin_step_bps);
            // Bin liquidity is spread evenly across walked bins, in USD
            // (USDC) terms. Unit handling depends on swap direction:
            //   * SOL → USDC (zero_for_one): output is USDC; the bin absorbs
            //     up to bin_quote_units USD of output, consuming the
            //     equivalent SOL at price p.
            //   * USDC → SOL: output is SOL; the bin absorbs up to
            //     bin_quote_units USD of input, yielding input_usd / p SOL.
            let bin_quote_units = est_bin_liquidity_usd;
            if zero_for_one {
                let take_out = bin_quote_units.min(remaining * p);
                produced += take_out;
                remaining -= take_out / p;
            } else {
                let take_in = bin_quote_units.min(remaining);
                produced += take_in / p;
                remaining -= take_in;
            }
            end_price = p;
            bins_crossed += 1;
        }

        let amount_out = (produced * 10f64.powi(dec_out as i32)).floor() as u64;
        // Ideal (zero-impact) output: SOL→USDC multiplies by price; USDC→SOL
        // divides by price (USDC at the $1 reference).
        let ideal_out = if zero_for_one {
            amount_in_f * market.price_b_per_a
        } else {
            amount_in_f / market.price_b_per_a
        };
        let impact = if ideal_out > 0.0 {
            (1.0 - produced / ideal_out) * 10_000.0
        } else {
            0.0
        };

        Ok(LegQuote {
            venue: Venue::MeteoraDlmm,
            market_address: market.market_address.clone(),
            token_in: token_in.clone(),
            token_out: token_out.clone(),
            amount_in,
            amount_out,
            fee_lamports: ((amount_in_f - amount_in_after) * 10f64.powi(dec_in as i32))
                .floor() as u64,
            decimals_in: dec_in,
            decimals_out: dec_out,
            usd_per_output_unit: if dec_out == 9 { self.sol_usd } else { 1.0 },
            price_impact_bps: impact,
            minimum_output: (produced * 0.995 * 10f64.powi(dec_out as i32)).floor() as u64,
            available_liquidity: est_bin_liquidity_usd * bins_crossed as f64,
            quote_slot: market.slot,
            quote_ts: Utc::now(),
            source: format!(
                "Meteora DLMM bin traversal: {} bins × {}bps step, active bin {}, start price {:.4} → {:.4}",
                bins_crossed, bin_step_bps, active_bin, start_price, end_price,
            ),
            liquidity_ok: remaining <= 0.0 && impact < self.impact_cap_bps,
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
