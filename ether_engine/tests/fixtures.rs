// Deterministic test harness for ether_engine.
//
// Fixtures cover the seven required cases from the specification:
//   1. profitable            — cross-venue route that survives full costs
//   2. unprofitable          — no edge; net stays below zero
//   3. stale-quote           — quote older than MAX_QUOTE_AGE_MS
//   4. insufficient-liquidity — size exceeds what the venue can fill
//   5. high-impact           — price impact above the 200 bps gate
//   6. fee-erases-profit     — DEX/network fees exceed the gross edge
//   7. second-quote-invalidates — second verification quote collapses the net
//
// Every fixture builds the pipeline inputs by hand (no RPC, no RNG) so the
// suite is fully deterministic and hermetic.

use ether_engine::config::EngineConfig;
use ether_engine::dex::raydium_cpmm::RaydiumCpmmAdapter;
use ether_engine::dex::DexAdapter;
use ether_engine::dex::AdapterRegistry;
use ether_engine::engine::SharedState;
use ether_engine::simulator::RouteSimulator;
use ether_engine::types::*;
use chrono::{DateTime, TimeZone, Utc};

const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const SOL: &str = "So11111111111111111111111111111111111111112";

/// Build a deterministic CPMM market state at a chosen price/liquidity.
fn market(price: f64, reserve_sol: u64, reserve_usdc: u64, slot: u64) -> MarketState {
    MarketState {
        venue: Venue::RaydiumCpmm,
        market_address: format!("{:.0}@{}", price, slot),
        mint_a: SOL.into(),
        mint_b: USDC.into(),
        decimals_a: 9,
        decimals_b: 6,
        price_b_per_a: price,
        fee_num: 2500,
        fee_den: 1_000_000,
        liquidity_quote: reserve_usdc as f64 / 1e6,
        detail: MarketDetail::Cpmm {
            reserve_a: reserve_sol,
            reserve_b: reserve_usdc,
        },
        slot,
        captured_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    }
}

fn default_config() -> EngineConfig {
    EngineConfig {
        rpc_provider_url: "http://127.0.0.1:8899".into(),
        jito_enabled: false,
        max_quote_age_ms: 3000,
        min_net_profit_usd: 0.05,
        safety_buffer_bps: 25,
        priority_fee_assumption_lamports: 25_000,
        jito_tip_assumption_lamports: 5_000_000,
        use_live_priority_fees: false,
        engine_port: 18787,
        sol_usd_price: 150.0,
        scanner_tick_ms: 2000,
        demo_mode: false,
        data_dir: "/tmp/ether_engine_test".into(),
    }
}

fn registry(sol_usd: f64) -> AdapterRegistry {
    let mut r = AdapterRegistry::new();
    r.register(Box::new(RaydiumCpmmAdapter::new(
        sol_usd,
        USDC.into(),
        SOL.into(),
    )));
    r
}

/// Route: USDC → SOL on market A, SOL → USDC on market B.
fn route(leg_a_amount_in: u64, market_a: &MarketState, market_b: &MarketState) -> CandidateRoute {
    let reg = registry(market_a.price_b_per_a / 150.0 * 150.0);
    let adapter = reg.for_venue(Venue::RaydiumCpmm).unwrap();
    let leg_a = adapter
        .quote(market_a, &USDC.to_string(), &SOL.to_string(), leg_a_amount_in)
        .expect("leg A quote");
    let leg_b = adapter
        .quote(market_b, &SOL.to_string(), &USDC.to_string(), leg_a.amount_out)
        .expect("leg B quote");
    CandidateRoute {
        input_mint: USDC.into(),
        output_mint: USDC.into(),
        leg_a,
        leg_b,
        quote_age_ms: 0,
        state_slot: market_a.slot,
    }
}

#[test]
fn fixture_profitable() {
    // Market A buys SOL at 150.00, Market B sells SOL at 153.00 → 2% edge,
    // enough to survive all costs at $500.
    let a = market(150.0, 28_000_000_000, 4_200_000_000_000, 100);
    let b = market(153.0, 27_450_000_000, 4_200_000_000_000, 100);
    let rt = route(500_000_000, &a, &b); // $500
    assert!(rt.leg_a.liquidity_ok && rt.leg_b.liquidity_ok);

    let cfg = default_config();
    let reg = registry(150.0);
    let sim = RouteSimulator::new(&cfg, &reg);
    let costs = sim.simulate(&rt, 500.0);
    assert!(
        costs.net_profit_usd > cfg.min_net_profit_usd,
        "expected profitable, got net {}",
        costs.net_profit_usd
    );
}

#[test]
fn fixture_unprofitable() {
    // Identical prices → no edge; costs push net negative.
    let a = market(150.0, 28_000_000_000, 4_200_000_000_000, 100);
    let b = market(150.0, 28_000_000_000, 4_200_000_000_000, 100);
    let rt = route(500_000_000, &a, &b);

    let cfg = default_config();
    let reg = registry(150.0);
    let sim = RouteSimulator::new(&cfg, &reg);
    let costs = sim.simulate(&rt, 500.0);
    assert!(
        costs.net_profit_usd <= 0.0,
        "expected unprofitable, got net {}",
        costs.net_profit_usd
    );
}

#[test]
fn fixture_stale_quote() {
    let a = market(150.0, 28_000_000_000, 4_200_000_000_000, 100);
    let b = market(153.0, 27_450_000_000, 4_200_000_000_000, 100);
    let mut rt = route(500_000_000, &a, &b);
    // Older than MAX_QUOTE_AGE_MS (3000 ms).
    rt.quote_age_ms = 3500;

    let rej = RouteSimulator::pre_check(&rt, &default_config());
    assert!(matches!(rej, Some(RejectionReason::QuoteStale { .. })));
}

#[test]
fn fixture_insufficient_liquidity() {
    // $1M input against a $40k venue — hard liquidity constraint rejects.
    let a = market(150.0, 266_666_666, 40_000_000_000, 100);
    let b = market(153.0, 27_450_000_000, 4_200_000_000_000, 100);
    let rt = route(1_000_000_000_000, &a, &b); // $1M = 1e12 lamports-ish (1e6 decimals)
    assert!(!rt.leg_a.liquidity_ok, "venue must flag insufficient liquidity");

    let rej = RouteSimulator::pre_check(&rt, &default_config());
    assert!(matches!(rej, Some(RejectionReason::InsufficientLiquidity { .. })));
}

#[test]
fn fixture_high_impact() {
    // Tiny venue: quote large size so price impact exceeds the 200 bps gate.
    let a = market(150.0, 66_666_666, 10_000_000_000, 100); // ~$10k
    let b = market(153.0, 27_450_000_000, 4_200_000_000_000, 100);
    let rt = route(8_000_000_000, &a, &b); // $8k into a $10k pool
    assert!(rt.leg_a.price_impact_bps > 200.0, "impact too low: {}", rt.leg_a.price_impact_bps);

    let rej = RouteSimulator::pre_check(&rt, &default_config());
    assert!(matches!(rej, Some(RejectionReason::HighPriceImpact { .. })));
}

#[test]
fn fixture_fee_erases_profit() {
    // Small 0.3% edge on $100 = $0.30 gross, but raise fees so the net is negative.
    let a = market(150.0, 28_000_000_000, 4_200_000_000_000, 100);
    let b = market(150.45, 27_865_000_000, 4_200_000_000_000, 100);
    let rt = route(100_000_000, &a, &b); // $100

    let mut cfg = default_config();
    // Crank priority fee so network cost exceeds the small edge.
    cfg.priority_fee_assumption_lamports = 100_000_000; // $15
    let reg = registry(150.0);
    let sim = RouteSimulator::new(&cfg, &reg);
    let costs = sim.simulate(&rt, 100.0);
    assert!(
        costs.net_profit_usd < 0.0,
        "expected fees to erase profit, got net {}",
        costs.net_profit_usd
    );
}

#[test]
fn fixture_second_quote_invalidates() {
    // First check passes, but a second quote from a stale/collapsed market
    // drops the net below the floor — the double-verification gate must catch
    // it. Simulated by re-quoting leg A with a degraded market.
    let a = market(150.0, 28_000_000_000, 4_200_000_000_000, 100);
    let b = market(153.0, 27_450_000_000, 4_200_000_000_000, 100);
    let a_collapsed = market(152.0, 28_000_000_000, 4_256_000_000_000, 101); // edge gone

    let cfg = default_config();
    let reg = registry(150.0);
    let sim = RouteSimulator::new(&cfg, &reg);
    let rt = route(500_000_000, &a, &b);
    let first = sim.simulate(&rt, 500.0);
    assert!(first.net_profit_usd > cfg.min_net_profit_usd);

    let adapter = reg.for_venue(Venue::RaydiumCpmm).unwrap();
    let leg_a2 = adapter
        .quote(&a_collapsed, &USDC.to_string(), &SOL.to_string(), rt.leg_a.amount_in)
        .unwrap();
    let leg_b2 = adapter
        .quote(&b, &SOL.to_string(), &USDC.to_string(), leg_a2.amount_out)
        .unwrap();
    let rt2 = CandidateRoute {
        leg_a: leg_a2,
        leg_b: leg_b2,
        ..rt.clone()
    };
    let second = sim.simulate(&rt2, 500.0);
    assert!(
        second.net_profit_usd < first.net_profit_usd * 0.5,
        "second quote did not collapse: first={} second={}",
        first.net_profit_usd,
        second.net_profit_usd
    );
}

#[test]
fn fixture_persistence_roundtrip() {
    // Verify the embedded SQLite schema writes and reads a verdict losslessly.
    let mut cfg = default_config();
    cfg.data_dir = "/tmp/ether_engine_persist_test".into();
    std::fs::remove_dir_all(&cfg.data_dir).ok();
    let db = ether_engine::store::sqlite::open_db(&cfg).unwrap();
    let sid = ether_engine::store::sqlite::start_session(&db, &cfg).unwrap();
    assert!(sid > 0);

    let a = market(150.0, 28_000_000_000, 4_200_000_000_000, 100);
    let b = market(153.0, 27_450_000_000, 4_200_000_000_000, 100);
    let rt = route(500_000_000, &a, &b);
    let verdict = OpportunityVerdict {
        decision: Decision::Profitable,
        route: rt,
        size_usd: 500.0,
        optimal_size_usd: Some(500.0),
        costs: CostBreakdown {
            gross_profit_usd: 9.5,
            dex_fees_usd: 2.5,
            price_impact_usd: 0.4,
            network_base_fee_usd: 0.0009,
            priority_fee_usd: 0.00375,
            tip_allowance_usd: 0.0,
            safety_buffer_usd: 0.125,
            net_profit_usd: 6.47,
            profit_bps: 129.4,
            jito_enabled: false,
        },
        first_check_usd: 6.8,
        second_check_usd: Some(6.47),
        verification_status: VerificationStatus::DoubleVerified,
        rejection: None,
        confidence: 90,
        detected_at: Utc::now(),
    };
    let id = ether_engine::store::sqlite::persist_verdict(&db, sid, &verdict).unwrap();
    assert!(id > 0);

    let conn = db.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT status, net_profit_usd, verification_status, rejection_reason FROM opportunities WHERE id = ?1")
        .unwrap();
    let (status, net, vs, rej) = stmt
        .query_row(rusqlite::params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap();
    assert_eq!(status, "profitable");
    assert!((net - 6.47).abs() < 1e-9);
    assert_eq!(vs, "double_verified");
    assert!(rej.is_none());
}

