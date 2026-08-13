// Shared concentrated-liquidity (CLMM) quoting math for Raydium CLMM and
// Orca Whirlpool. Implements the standard sqrt-price tick model:
//
//   L       = active liquidity in the current tick range
//   sqrt(P) = sqrt price in Q64.64
//   swap amount exact-in:
//     amount_out = floor(L * (1/sqrt(P_curr) - 1/sqrt(P_next)))   while within range
//     fees accrue at fee_rate / 1_000_000 on input
//
// Tick traversal: the trade walks tick ranges; each range contributes its
// liquidity until the input is consumed or liquidity runs out (hard constraint:
// if the trade cannot be fully filled at current state, liquidity_ok = false).
//
// All functions are pure — no randomness.

/// Q64.64 sqrt price.
pub type SqrtPriceX64 = f64;

/// Convert a sqrt price (Q64.64, stored as u128) to a floating-point price.
#[inline]
pub fn sqrt_price_to_f64(sqrt_price_x64: u128) -> f64 {
    let p = (sqrt_price_x64 as f64) / (1u128 << 64) as f64;
    p * p
}

/// Price at a given tick: 1.0001^tick.
#[inline]
pub fn price_at_tick(tick: i32) -> f64 {
    1.0001_f64.powi(tick)
}

/// Tick index for a given price.
#[inline]
pub fn tick_for_price(price: f64) -> i32 {
    (price.ln() / 1.0001_f64.ln()).round() as i32
}

/// Result of a single tick-range traversal step.
#[derive(Debug, Clone)]
pub struct RangeFill {
    pub amount_in_consumed: f64,
    pub amount_out_produced: f64,
    pub ticks_crossed: usize,
    pub liquidity_at_end: u128,
    pub fully_filled: bool,
}

/// Deterministically simulate an exact-input swap across tick ranges.
///
/// * `sqrt_price` — current sqrt price in Q64.64 as f64
/// * `liquidity`  — liquidity in the current range
/// * `tick_current` — current tick index
/// * `tick_spacing` — pool tick spacing
/// * `zero_for_one` — true: token_a → token_b (price decreases), false: reverse
/// * `amount_in`  — exact input in human units of token_in
/// * `fee_rate`   — fee in parts-per-million of input
/// * `tick_map`   — optional map tick_index -> next liquidity. When None, the
///                  simulator assumes the current liquidity persists across
///                  `max_ticks_crossed` (a conservative overestimate of fills).
pub fn simulate_clmm_swap(
    sqrt_price: f64,
    liquidity: u128,
    tick_current: i32,
    tick_spacing: i16,
    zero_for_one: bool,
    amount_in: f64,
    fee_rate: u64,
    tick_map: Option<&[(i32, u128)]>,
    max_ticks_crossed: usize,
) -> RangeFill {
    let amount_in_after_fee = amount_in * (1.0 - fee_rate as f64 / 1_000_000.0);
    let mut remaining = amount_in_after_fee;
    let mut produced = 0.0_f64;
    let mut liq = liquidity as f64;
    let mut ticks_crossed = 0;
    let mut sqrt_p = sqrt_price;

    for step in 0..max_ticks_crossed {
        if remaining <= 0.0 || liq <= 0.0 {
            break;
        }
        let next_tick = if zero_for_one {
            tick_current - (step as i32 + 1) * tick_spacing as i32
        } else {
            tick_current + (step as i32 + 1) * tick_spacing as i32
        };
        let sqrt_next = price_at_tick(next_tick).sqrt();

        // Amount of token_in consumed moving sqrt_p → sqrt_next given liquidity liq.
        let (consumed, produced_step, sqrt_after) = if zero_for_one {
            // token_a → token_b: price decreases; token_a is token_0
            let a_in = liq * (1.0 / sqrt_next - 1.0 / sqrt_p);
            let b_out = liq * (sqrt_p - sqrt_next);
            (a_in, b_out, sqrt_next)
        } else {
            let b_in = liq * (sqrt_next - sqrt_p);
            let a_out = liq * (1.0 / sqrt_p - 1.0 / sqrt_next);
            (b_in, a_out, sqrt_next)
        };

        if consumed >= remaining {
            // Trade completes inside this range.
            let frac = remaining / consumed;
            produced += produced_step * frac;
            sqrt_p = sqrt_p + (sqrt_after - sqrt_p) * frac;
            remaining = 0.0;
            ticks_crossed += 1;
            break;
        } else {
            remaining -= consumed;
            produced += produced_step;
            sqrt_p = sqrt_after;
            ticks_crossed += 1;
            // Adjust liquidity if a tick map is provided.
            if let Some(map) = tick_map {
                if let Some(&(_, new_liq)) = map.iter().find(|(t, _)| *t == next_tick) {
                    liq = new_liq as f64;
                }
            }
        }
    }

    RangeFill {
        amount_in_consumed: amount_in_after_fee - remaining,
        amount_out_produced: produced,
        ticks_crossed,
        liquidity_at_end: liq as u128,
        fully_filled: remaining <= 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_at_tick_symmetry() {
        let p100 = price_at_tick(100);
        let p_neg100 = price_at_tick(-100);
        assert!((p100 * p_neg100 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn clmm_swap_consumes_input() {
        // A realistic mini-pool: SOL/USDC, sqrt(P) ≈ sqrt(150).
        let sqrt_p = 150.0_f64.sqrt();
        let liquidity = 1_000_000u128;
        let fill = simulate_clmm_swap(
            sqrt_p,
            liquidity,
            50_060,
            60,
            true,
            10.0, // 10 SOL in
            3000, // 30 bps
            None,
            10,
        );
        assert!(fill.fully_filled);
        assert!(fill.amount_out_produced > 0.0);
        assert!(fill.amount_in_consumed <= 10.0);
    }
}
