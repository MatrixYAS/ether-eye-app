// Raydium CPMM adapter — constant-product AMM with a fee taken on input.
//
// Math (deterministic):
//   quoted_output = floor(reserve_out * amount_in * (1 - fee) /
//                        (reserve_in * (1 - fee) + amount_in))
//   price impact  = 1 - (quoted_output / (reserve_out * amount_in / reserve_in))
// Everything derives from the two reserve balances; there is no randomness.

use crate::dex::DexAdapter;
use crate::error::EngineError;
use crate::rpc::accounts::DecodedPool;
use crate::types::{LegQuote, MarketDetail, MarketState, Pubkey, Venue};
use chrono::Utc;

/// Fee numerator/denominator on INPUT for Raydium CPMM (0.25% typical).
const TRADE_FEE_NUM: u64 = 2500;
const TRADE_FEE_DEN: u64 = 1_000_000;

pub struct RaydiumCpmmAdapter {
    pub sol_usd: f64,
    pub usdc_mint: Pubkey,
    pub sol_mint: Pubkey,
}

impl RaydiumCpmmAdapter {
    pub fn new(sol_usd: f64, usdc_mint: Pubkey, sol_mint: Pubkey) -> Self {
        Self {
            sol_usd,
            usdc_mint,
            sol_mint,
        }
    }
}

impl DexAdapter for RaydiumCpmmAdapter {
    fn venue(&self) -> Venue {
        Venue::RaydiumCpmm
    }

    fn market_state_from_pool(&self, pool: &DecodedPool) -> Result<MarketState, EngineError> {
        match pool {
            DecodedPool::RaydiumCpmm {
                token_a_mint,
                token_b_mint,
                reserve_a,
                reserve_b,
                decimals_a,
                decimals_b,
                slot,
                ..
            } => {
                let ra = (*reserve_a as f64) / 10f64.powi(*decimals_a as i32);
                let rb = (*reserve_b as f64) / 10f64.powi(*decimals_b as i32);
                let price = if ra > 0.0 { rb / ra } else { 0.0 };
                let liquidity_quote = rb * self.sol_usd; // rough USD of the quote side
                Ok(MarketState {
                    venue: Venue::RaydiumCpmm,
                    market_address: String::new(),
                    mint_a: token_a_mint.clone(),
                    mint_b: token_b_mint.clone(),
                    decimals_a: *decimals_a,
                    decimals_b: *decimals_b,
                    price_b_per_a: price,
                    fee_num: TRADE_FEE_NUM,
                    fee_den: TRADE_FEE_DEN,
                    liquidity_quote,
                    detail: MarketDetail::Cpmm {
                        reserve_a: *reserve_a,
                        reserve_b: *reserve_b,
                    },
                    slot: *slot,
                    captured_at: Utc::now(),
                })
            }
            _ => Err(EngineError::Decode("not a CPMM pool".into())),
        }
    }

    fn quote(
        &self,
        market: &MarketState,
        token_in: &Pubkey,
        token_out: &Pubkey,
        amount_in: u64,
    ) -> Result<LegQuote, EngineError> {
        let (reserve_in, reserve_out, dec_in, dec_out) =
            reserves_for_direction(market, token_in, token_out)?;
        let amount_in_f = amount_in as f64 / 10f64.powi(dec_in as i32);
        let fee_taken = amount_in_f * (TRADE_FEE_NUM as f64 / TRADE_FEE_DEN as f64);
        let amount_in_after = amount_in_f - fee_taken;
        let quoted_out_f = reserve_out * amount_in_after / (reserve_in + amount_in_after);
        let quoted_out = (quoted_out_f * 10f64.powi(dec_out as i32)).floor() as u64;
        let ideal_out = reserve_out * amount_in_f / reserve_in;
        let impact = if ideal_out > 0.0 {
            (1.0 - quoted_out_f / ideal_out) * 10_000.0
        } else {
            0.0
        };
        // Quote-side depth in human units: for the USDC leg this equals USD;
        // for the SOL leg, convert at the reference price so the liquidity
        // gate (USD-based) stays consistent across venues.
        let liquidity = if dec_out == 9 {
            reserve_out * self.sol_usd
        } else {
            reserve_out
        };
        let now = Utc::now();
        Ok(LegQuote {
            venue: Venue::RaydiumCpmm,
            market_address: market.market_address.clone(),
            token_in: token_in.clone(),
            token_out: token_out.clone(),
            amount_in,
            amount_out: quoted_out,
            fee_lamports: (fee_taken * 10f64.powi(dec_in as i32)).floor() as u64,
            decimals_in: dec_in,
            decimals_out: dec_out,
            usd_per_output_unit: if dec_out == 9 { self.sol_usd } else { 1.0 },
            price_impact_bps: impact,
            minimum_output: (quoted_out_f * 0.995 * 10f64.powi(dec_out as i32)).floor() as u64,
            available_liquidity: liquidity,
            quote_slot: market.slot,
            quote_ts: now,
            source: format!(
                "Raydium CPMM constant-product: floor(R_out * a*(1-25bp) / (R_in + a*(1-25bp))), R_in={reserve_in:.2}, R_out={reserve_out:.2}, a={amount_in_f:.6}"
            ),
            liquidity_ok: amount_in_f <= reserve_in,
        })
    }

    fn calculate_fees(&self, market: &MarketState, amount_in: u64) -> Result<u64, EngineError> {
        let (reserve_in, _, dec_in, _) =
            reserves_for_direction(market, &market.mint_a, &market.mint_b)?;
        let in_f = amount_in as f64 / 10f64.powi(dec_in as i32);
        let fee = in_f * (TRADE_FEE_NUM as f64 / TRADE_FEE_DEN as f64);
        Ok((fee * 10f64.powi(dec_in as i32)).floor() as u64)
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

/// Resolve the (reserve_in, reserve_out, decimals_in, decimals_out) tuple for
/// the requested swap direction.
fn reserves_for_direction(
    market: &MarketState,
    token_in: &Pubkey,
    token_out: &Pubkey,
) -> Result<(f64, f64, u8, u8), EngineError> {
    match market.detail {
        MarketDetail::Cpmm { reserve_a, reserve_b } => {
            if token_in == &market.mint_a && token_out == &market.mint_b {
                Ok((
                    reserve_a as f64 / 10f64.powi(market.decimals_a as i32),
                    reserve_b as f64 / 10f64.powi(market.decimals_b as i32),
                    market.decimals_a,
                    market.decimals_b,
                ))
            } else if token_in == &market.mint_b && token_out == &market.mint_a {
                Ok((
                    reserve_b as f64 / 10f64.powi(market.decimals_b as i32),
                    reserve_a as f64 / 10f64.powi(market.decimals_a as i32),
                    market.decimals_b,
                    market.decimals_a,
                ))
            } else {
                Err(EngineError::NoMarkets)
            }
        }
        _ => Err(EngineError::Decode("not a CPMM market".into())),
    }
}

/// Re-export used by other modules for USD conversion of token amounts.
pub fn usd_value(
    amount_lamports: u64,
    decimals: u8,
    price_usd_per_unit: f64,
) -> f64 {
    amount_lamports as f64 / 10f64.powi(decimals as i32) * price_usd_per_unit
}
