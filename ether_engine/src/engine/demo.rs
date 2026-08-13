// Deterministic demo market fixtures.
//
// These represent stable, known AMM states used by DEMO_MODE (and as the
// canonical route set in live mode when no market registry is loaded). Every
// number is a constant — no randomness. They let the dashboard render a fully
// functional live pipeline with zero RPC dependency.

use crate::dex::raydium_cpmm::RaydiumCpmmAdapter;
use crate::dex::DexAdapter;
use crate::types::*;
use chrono::Utc;

const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const SOL: &str = "So11111111111111111111111111111111111111112";

/// Canonical demo markets — SOL/USDC across four venues at slightly
/// different price points, the exact state a live scanner would observe.
/// Market spread ≈ 0.5% between the best buy and best sell, which is enough
/// to survive DEX fees + network costs at a few hundred dollars of size and
/// produces a handful of accepted (profitable) and rejected opportunities.
pub fn canonical_markets(slot: u64) -> Vec<MarketState> {
    vec![
        // Cheap SOL to buy: Raydium CPMM at 149.20
        MarketState {
            venue: Venue::RaydiumCpmm,
            market_address: "58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2".into(),
            mint_a: SOL.into(),
            mint_b: USDC.into(),
            decimals_a: 9,
            decimals_b: 6,
            price_b_per_a: 148.20,
            fee_num: 2500,
            fee_den: 1_000_000,
            // Reserves balanced at the quoted price: 28.2 SOL ≈ $4,207 USDC
            // (price 148.20 USDC per SOL).
            liquidity_quote: 4_200.0,
            detail: MarketDetail::Cpmm {
                reserve_a: 28_200_000_000,
                reserve_b: 4_207_440_000,
            },
            slot,
            captured_at: Utc::now(),
        },
        // Orca Whirlpool: SOL slightly dearer (a realistic cross-venue offset)
        MarketState {
            venue: Venue::OrcaWhirlpool,
            market_address: "2u1JHSzgDa71f9JCRi9K5PL6j6X8xREp6GGENYNqu9hP".into(),
            mint_a: SOL.into(),
            mint_b: USDC.into(),
            decimals_a: 9,
            decimals_b: 6,
            price_b_per_a: 149.30,
            fee_num: 3000,
            fee_den: 1_000_000,
            liquidity_quote: 3_100_000.0,
            detail: MarketDetail::Clmm {
                // sqrt(price) in Q64 fixed point: sqrt(1.0001^-277261) * 2^64.
                sqrt_price_x64: 17_602_510_716_258.0,
                liquidity: 3_000_000_000_000,
                tick_spacing: 64,
                current_tick: -277_261,
                ticks_crossed: vec![TickSegment {
                    start_tick: -277_261,
                    end_tick: -277_197,
                    liquidity: 3_000_000_000_000,
                    fee_growth_in: 0,
                }],
            },
            slot,
            captured_at: Utc::now(),
        },
        // Meteora DLMM: SOL dearer still — the sell side of one route
        MarketState {
            venue: Venue::MeteoraDlmm,
            market_address: "24CcwN7QRRN4a7YhP1v3Y4974c8Z951qL299T7ZzV3Kq".into(),
            mint_a: SOL.into(),
            mint_b: USDC.into(),
            decimals_a: 9,
            decimals_b: 6,
            price_b_per_a: 150.70,
            fee_num: 2000,
            fee_den: 1_000_000,
            liquidity_quote: 2_600_000.0,
            detail: MarketDetail::Dlmm {
                // Bin-id anchor: id = ln(price) / ln(1 + bin_step_bps/10^4) so
                // that base^id ≈ price_b_per_a (150.70 USDC per SOL).
                active_bin: 5_068,
                bin_step_bps: 10,
                bins_crossed: vec![BinSegment {
                    start_bin: 5_068,
                    end_bin: 5_078,
                    liquidity: 900_000_000,
                }],
            },
            slot,
            captured_at: Utc::now(),
        },
        // Phoenix order book: most expensive SOL — sell leg of the classic
        // Raydium→Phoenix route (buy @149.20, sell @150.60 ≈ 0.9% gross).
        MarketState {
            venue: Venue::Phoenix,
            market_address: "4DoNfFBfF7UokCC2FQzriy7yHK6DY6NVdYpuekQ5pRkf".into(),
            mint_a: SOL.into(),
            mint_b: USDC.into(),
            decimals_a: 9,
            decimals_b: 6,
            price_b_per_a: 151.40,
            fee_num: 2000,
            fee_den: 1_000_000,
            liquidity_quote: 5_800_000.0,
            detail: MarketDetail::PhoenixBook {
                best_bid: 151.39,
                best_ask: 151.41,
                levels: vec![
                    BookLevel {
                        price: 151.41,
                        quantity_base: 120.0,
                        side: Side::Ask,
                    },
                    BookLevel {
                        price: 151.43,
                        quantity_base: 90.0,
                        side: Side::Ask,
                    },
                    BookLevel {
                        price: 151.46,
                        quantity_base: 150.0,
                        side: Side::Ask,
                    },
                ],
            },
            slot,
            captured_at: Utc::now(),
        },
    ]
}

/// Quote a leg against a demo market. Deterministic: same inputs → same output.
pub fn quote_from_market(
    market: &MarketState,
    token_in: &str,
    token_out: &str,
    amount_in: u64,
) -> LegQuote {
    // Dispatcher: each venue quotes with its own adapter so the demo ticks
    // exercise the same deterministic pipeline as live mode. Every adapter is
    // seeded with a 150.0 SOL/USD reference and the canonical USDC/SOL mints.
    let adapter: Box<dyn crate::dex::DexAdapter> = match market.venue {
        Venue::RaydiumCpmm | Venue::RaydiumClmm => {
            Box::new(RaydiumCpmmAdapter::new(150.0, USDC.into(), SOL.into()))
        }
        Venue::OrcaWhirlpool => Box::new(crate::dex::orca_whirlpool::OrcaWhirlpoolAdapter::new(
            150.0,
            USDC.into(),
            SOL.into(),
        )),
        Venue::MeteoraDlmm => {
            Box::new(crate::dex::meteora_dlmm::MeteoraDlmmAdapter::new(
                150.0,
                USDC.into(),
                SOL.into(),
            ))
        }
        Venue::Phoenix => {
            Box::new(crate::dex::phoenix::PhoenixAdapter::new(
                150.0,
                USDC.into(),
                SOL.into(),
            ))
        }
    };
    let tin = token_in.to_string();
    let tout = token_out.to_string();
    let result = adapter.quote(market, &tin, &tout, amount_in);
    result.unwrap_or_else(|err| {
            tracing::info!(
                market = %market.market_address,
                venue = %market.venue,
                err = %err,
                "demo quote failed"
            );
            LegQuote {
                venue: market.venue,
            market_address: market.market_address.clone(),
            token_in: token_in.into(),
            token_out: token_out.into(),
            amount_in,
            amount_out: 0,
            fee_lamports: 0,
            decimals_in: if token_in == SOL { 9 } else { 6 },
            decimals_out: if token_out == SOL { 9 } else { 6 },
            usd_per_output_unit: if token_out == SOL { 150.0 } else { 1.0 },
            price_impact_bps: 0.0,
            minimum_output: 0,
            available_liquidity: 0.0,
            quote_slot: market.slot,
            quote_ts: Utc::now(),
                source: "demo quote failed".into(),
                liquidity_ok: false,
            }
        })
}
