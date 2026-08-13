// Opportunity pipeline: discover → simulate → size-optimize → verify → verdict.
//
// The loop is fully deterministic given the same account state. The optional
// DEMO_MODE generates a fixed synthetic market so the dashboard works with no
// RPC endpoint at all (no randomness either — fixture data is constant).

pub mod demo;

use crate::config::EngineConfig;
use crate::dex::{
    phoenix::PhoenixAdapter, raydium_clmm::RaydiumClmmAdapter, raydium_cpmm::RaydiumCpmmAdapter,
    AdapterRegistry,
};
use crate::error::EngineError;
use crate::optimizer::{SizeOptimization, SizeOptimizer};
use crate::simulator::RouteSimulator;
use crate::store::sqlite::{self, DbHandle};
use crate::types::*;
use chrono::Utc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Shared, thread-safe engine state consumed by the Axum API.
pub struct SharedState {
    pub config: std::sync::Mutex<EngineConfig>,
    pub registry: AdapterRegistry,
    pub db: DbHandle,
    pub session_id: AtomicI64,
    pub running: AtomicBool,
    pub demo_mode: bool,
    pub last_tick_at: RwLock<Option<chrono::DateTime<Utc>>>,
    pub latest_prioritization_fee: RwLock<Option<u64>>,
    pub recent_verdicts: RwLock<Vec<OpportunityVerdict>>,
}

impl SharedState {
    pub fn new(config: EngineConfig) -> Result<Arc<Self>, EngineError> {
        let mut registry = AdapterRegistry::new();
        registry.register(Box::new(RaydiumCpmmAdapter::new(
            config.sol_usd_price,
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
            "So11111111111111111111111111111111111111112".into(),
        )));
        registry.register(Box::new(RaydiumClmmAdapter::new(
            config.sol_usd_price,
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
            "So11111111111111111111111111111111111111112".into(),
        )));
        registry.register(Box::new(PhoenixAdapter::new(
            config.sol_usd_price,
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
            "So11111111111111111111111111111111111111112".into(),
        )));
        let cfg_clone = config.clone();
        let db = sqlite::open_db(&cfg_clone)?;
        let session_id = sqlite::start_session(&db, &config)?;
        Ok(Arc::new(Self {
            config: std::sync::Mutex::new(config),
            registry,
            db,
            session_id: AtomicI64::new(session_id),
            running: AtomicBool::new(true),
            demo_mode: cfg_clone.demo_mode,
            last_tick_at: RwLock::new(None),
            latest_prioritization_fee: RwLock::new(None),
            recent_verdicts: RwLock::new(Vec::new()),
        }))
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = sqlite::stop_session(&self.db, self.session_id.load(Ordering::Relaxed));
    }
}

/// Main entry: starts the HTTP API and the scanner loop.
pub async fn run(config: EngineConfig) -> Result<(), EngineError> {
    let state = SharedState::new(config.clone())?;

    // Scanner loop runs in a background task; the Axum server runs on the
    // main task so Ctrl-C shuts both down cleanly.
    let scan_state = state.clone();
    tokio::spawn(async move {
        scanner_loop(scan_state).await;
    });

    let addr: std::net::SocketAddr = ([127, 0, 0, 1], config.engine_port).into();
    info!(addr = %addr, "engine API listening");
    let app = crate::api::build_router(state.clone());
    axum::serve(
        tokio::net::TcpListener::bind(&addr).await
            .map_err(|e| EngineError::Config(format!("bind: {e}")))?,
        app,
    )
    .await
    .map_err(|e| EngineError::Config(format!("serve: {e}")))?;
    Ok(())
}

/// Deterministic scanner tick.
async fn scanner_loop(state: Arc<SharedState>) {
    let cfg_snapshot = state.config.lock().unwrap().clone();
    let rpc = crate::rpc::RpcProvider::new(&cfg_snapshot.rpc_provider_url);

    // Auto-fallback: when no reachable RPC endpoint is available (and DEMO_MODE
    // was not explicitly disabled by the user), run the deterministic demo
    // dataset so the dashboard works with zero configuration. A live deployment
    // sets RPC_PROVIDER_URL and runs live ticks.
    if !state.demo_mode {
        match rpc.current_slot().await {
            Ok(_) => {}
            Err(e) => {
                warn!(err = %e, "rpc unreachable at startup, switching to demo mode");
                state.config.lock().unwrap().demo_mode = true;
            }
        }
    }

    while state.running.load(Ordering::Relaxed) {
        let result = if state.config.lock().unwrap().demo_mode {
            run_tick_demo(&state).await
        } else {
            run_tick_live(&state, &rpc).await
        };
        if let Err(e) = result {
            warn!(err = %e, "tick failed");
        }
        {
            let mut last = state.last_tick_at.write().await;
            *last = Some(Utc::now());
        }
        let sleep_ms = state.config.lock().unwrap().scanner_tick_ms;
        tokio::time::sleep(tokio::time::Duration::from_millis(sleep_ms)).await;
    }
}

/// Live tick: fetch slots + priority fees, build candidate routes from the
/// registered venues, run the pipeline on each.
async fn run_tick_live(
    state: &Arc<SharedState>,
    rpc: &crate::rpc::RpcProvider,
) -> Result<(), EngineError> {
    let slot = match rpc.current_slot().await {
        Ok(s) => s,
        Err(e) => {
            error!(err = %e, "slot fetch failed");
            return Err(e);
        }
    };
    if state.config.lock().unwrap().use_live_priority_fees {
        match rpc.recent_prioritization_fees(Some(20)).await {
            Ok(samples) => {
                let med = samples
                    .iter()
                    .map(|s| s.prioritization_fee)
                    .collect::<Vec<_>>();
                let fee = if med.is_empty() {
                    None
                } else {
                    let mut sorted = med;
                    sorted.sort();
                    Some(sorted[sorted.len() / 2])
                };
                let mut lf = state.latest_prioritization_fee.write().await;
                *lf = fee;
            }
            Err(e) => warn!(err = %e, "priority fee fetch failed, using assumption"),
        }
    }

    // Build candidate routes between every pair of venue markets for the
    // tracked token pair. In production this is driven by a cached market
    // registry; here we use the demo markets as the canonical route set.
    let markets = demo::canonical_markets(slot);
    let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let sol = "So11111111111111111111111111111111111111112";
    let unit_usd = 1.0;

    for i in 0..markets.len() {
        for j in 0..markets.len() {
            if i == j {
                continue;
            }
            let ma = &markets[i];
            let mb = &markets[j];
            // Leg B input must equal leg A output — chain the quotes so the
            // route is a consistent round trip (USDC → SOL → USDC).
            let leg_a = demo::quote_from_market(ma, usdc, sol, 1_000_000);
            let leg_b = demo::quote_from_market(mb, sol, usdc, leg_a.amount_out);
            let route = CandidateRoute {
                input_mint: leg_a.token_in.as_str().into(),
                output_mint: leg_b.token_out.as_str().into(),
                leg_a: leg_a.clone(),
                leg_b: leg_b.clone(),
                quote_age_ms: 0,
                state_slot: slot,
            };
            let verdict = evaluate_route(state, route, unit_usd).await;
            persist_verdict(state, verdict).await;
            let _ = sqlite::increment_markets_scanned(&state.db, state.session_id.load(Ordering::Relaxed));
        }
    }
    Ok(())
}

/// Public one-shot demo tick (called by the /api/scan/once endpoint).
pub async fn run_tick_demo_for_api(state: &Arc<SharedState>) -> Result<(), EngineError> {
    run_tick_demo(state).await
}

async fn run_tick_demo(state: &Arc<SharedState>) -> Result<(), EngineError> {
    let slot = 320_000_000u64;
    let markets = demo::canonical_markets(slot);
    let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let sol = "So11111111111111111111111111111111111111112";
    let unit_usd = 1.0;
    for i in 0..markets.len() {
        for j in 0..markets.len() {
            if i == j {
                continue;
            }
            let ma = &markets[i];
            let mb = &markets[j];
            let leg_a = demo::quote_from_market(ma, usdc, sol, 1_000_000);
            let leg_b = demo::quote_from_market(mb, sol, usdc, leg_a.amount_out);
            let route = CandidateRoute {
                input_mint: leg_a.token_in.as_str().into(),
                output_mint: leg_b.token_out.as_str().into(),
                leg_a: leg_a.clone(),
                leg_b: leg_b.clone(),
                quote_age_ms: 0,
                state_slot: slot,
            };
            info!(
                i,
                j,
                leg_a_liq = leg_a.liquidity_ok,
                leg_a_avail = leg_a.available_liquidity,
                leg_a_out = leg_a.amount_out,
                leg_b_liq = leg_b.liquidity_ok,
                leg_b_avail = leg_b.available_liquidity,
                leg_b_out = leg_b.amount_out,
                "demo leg quotes"
            );
            let route = CandidateRoute {
                input_mint: usdc.into(),
                output_mint: usdc.into(),
                leg_a,
                leg_b,
                quote_age_ms: 0,
                state_slot: slot,
            };
            let verdict = evaluate_route(state, route, unit_usd).await;
            persist_verdict(state, verdict).await;
        }
    }
    Ok(())
}


/// Full pipeline for one candidate route: pre-check → simulate → size-optimize
/// → freshness gate → double verification → verdict.
pub async fn evaluate_route(
    state: &Arc<SharedState>,
    route: CandidateRoute,
    unit_usd: f64,
) -> OpportunityVerdict {
    let cfg = state.config.lock().unwrap().clone();
    let sim = RouteSimulator::new(&cfg, &state.registry);
    let now = Utc::now();
    let size_usd = route.leg_a.amount_in as f64 / 1e6;

    // Step 1: pre-check (freshness + liquidity + impact gate).
    if let Some(rej) = RouteSimulator::pre_check(&route, &cfg) {
        return OpportunityVerdict {
            decision: Decision::Rejected,
            route,
            size_usd,
            optimal_size_usd: None,
            costs: CostBreakdown {
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
            first_check_usd: 0.0,
            second_check_usd: None,
            verification_status: VerificationStatus::Pending,
            rejection: Some(rej),
            confidence: 0,
            detected_at: now,
        };
    }

    // Step 2: simulate at candidate size, then sweep for the profit-maximizing
    // trade size. The unit-size simulation is only an early-exit hint for
    // outright dead routes (negative edge at any size); the actual floor check
    // happens after the optimizer so that small unit quotes (e.g. $1 demo
    // probes) do not mask profitable routes at larger sizes.
    let mut sized = route.clone();
    let first_costs = sim.simulate(&sized, size_usd);
    let first_usd = first_costs.net_profit_usd;
    let edge_dead = first_usd <= 0.0;

    let opt = if edge_dead {
        SizeOptimization {
            sweep_points: vec![],
            optimal_size_usd: size_usd,
            expected_net_usd: first_usd,
            profit_bps: first_costs.profit_bps,
            breakdown: first_costs.clone(),
            refined: false,
            liquidity_rejection: None,
        }
    } else {
        // Step 3: size optimization (sweep + refinement).
        SizeOptimizer::optimize(&sim, &sized, unit_usd, &cfg).unwrap_or(SizeOptimization {
            sweep_points: vec![],
            optimal_size_usd: size_usd,
            expected_net_usd: first_usd,
            profit_bps: first_costs.profit_bps,
            breakdown: first_costs.clone(),
            refined: false,
            liquidity_rejection: None,
        })
    };

    if let Some(rej) = opt.liquidity_rejection {
        return OpportunityVerdict {
            decision: Decision::Rejected,
            route,
            size_usd,
            optimal_size_usd: None,
            costs: opt.breakdown,
            first_check_usd: first_usd,
            second_check_usd: None,
            verification_status: VerificationStatus::Pending,
            rejection: Some(rej),
            confidence: 10,
            detected_at: now,
        };
    }

    // Apply the optimizer's chosen size to the sized route before
    // re-quoting: the sweep works on rescaled clones, so `sized` itself still
    // carries the original (often $1) probe size.
    if opt.optimal_size_usd != size_usd {
        sized.leg_a.amount_in = ((opt.optimal_size_usd / size_usd) as f64
            * sized.leg_a.amount_in as f64)
            .floor() as u64;
    }

    // Step 4: second-quote re-verification (fresh quote against the SAME
    // markets the route was built on — re-quoting at the new size).
    // Live path: re-fetch and decode the route's actual on-chain markets so
    // the second quote reads fresh account state at the same venues.
    async fn refetch_market(
        state: &SharedState,
        cfg: &EngineConfig,
        leg: &LegQuote,
        slot: u64,
    ) -> Result<MarketState, EngineError> {
        let provider = crate::rpc::RpcProvider::new(&cfg.rpc_provider_url);
        let pool = crate::rpc::fetch_pool(&provider, &leg.market_address).await?;
        state
            .registry
            .for_venue(leg.venue)
            .ok_or(EngineError::NoMarkets)?
            .market_state_from_pool(&pool)
    }
    // Demo path: re-quote the canonical fixture market for this venue, which is
    // stable by construction and fully deterministic.
    fn demo_market(venue: Venue, slot: u64) -> Option<MarketState> {
        demo::canonical_markets(slot)
            .into_iter()
            .find(|m| m.venue == venue)
    }
    let rejected_second = |e: &EngineError| OpportunityVerdict {
        decision: Decision::Rejected,
        route: route.clone(),
        size_usd,
        optimal_size_usd: None,
        costs: opt.breakdown.clone(),
        first_check_usd: first_usd,
        second_check_usd: None,
        verification_status: VerificationStatus::Invalidated,
        rejection: Some(RejectionReason::RouteNotExecutable {
            detail: format!("second-quote re-verification failed: {e}"),
        }),
        confidence: 5,
        detected_at: now,
    };
    let leg_a_market = match if state.demo_mode {
        demo_market(route.leg_a.venue, route.state_slot).ok_or(EngineError::NoMarkets)
    } else {
        refetch_market(&state, &cfg, &route.leg_a, route.state_slot).await
    } {
        Ok(m) => m,
        Err(e) => return rejected_second(&e),
    };
    let second_leg_a = demo::quote_from_market(
        &leg_a_market,
        &route.input_mint,
        &route.leg_a.token_out,
        sized.leg_a.amount_in,
    );
    let is_demo = state.config.lock().unwrap().demo_mode;
    let leg_b_market = match if is_demo {
        demo_market(route.leg_b.venue, route.state_slot).ok_or(EngineError::NoMarkets)
    } else {
        refetch_market(&state, &cfg, &route.leg_b, route.state_slot).await
    } {
        Ok(m) => m,
        Err(e) => return rejected_second(&e),
    };
    let second_leg_b = demo::quote_from_market(
        &leg_b_market,
        &route.leg_b.token_in,
        &route.output_mint,
        second_leg_a.amount_out,
    );
    let mut second_route = sized.clone();
    second_route.leg_a = second_leg_a;
    second_route.leg_b = second_leg_b;
    let second_costs = sim.simulate(&second_route, opt.optimal_size_usd);
    let second_usd = second_costs.net_profit_usd;
    let invalidated = second_usd < cfg.min_net_profit_usd || second_usd <= 0.0
        || (first_usd > 0.0 && second_usd < first_usd * 0.5);

    if invalidated {
        return OpportunityVerdict {
            decision: Decision::Rejected,
            route: second_route,
            size_usd: opt.optimal_size_usd,
            optimal_size_usd: Some(opt.optimal_size_usd),
            costs: second_costs,
            first_check_usd: first_usd,
            second_check_usd: Some(second_usd),
            verification_status: VerificationStatus::Invalidated,
            rejection: Some(RejectionReason::SecondQuoteInvalidated {
                first_usd,
                second_usd,
            }),
            confidence: 30,
            detected_at: now,
        };
    }

    // Step 5: final floor check after the safety buffer.
    if second_usd < cfg.min_net_profit_usd {
        return OpportunityVerdict {
            decision: Decision::Rejected,
            route: second_route,
            size_usd: opt.optimal_size_usd,
            optimal_size_usd: Some(opt.optimal_size_usd),
            costs: second_costs,
            first_check_usd: first_usd,
            second_check_usd: Some(second_usd),
            verification_status: VerificationStatus::SingleVerified,
            rejection: Some(RejectionReason::BelowProfitFloor {
                net_usd: second_usd,
                floor_usd: cfg.min_net_profit_usd,
            }),
            confidence: 50,
            detected_at: now,
        };
    }

    OpportunityVerdict {
        decision: Decision::Profitable,
        route: second_route,
        size_usd: opt.optimal_size_usd,
        optimal_size_usd: Some(opt.optimal_size_usd),
        costs: second_costs,
        first_check_usd: first_usd,
        second_check_usd: Some(second_usd),
        verification_status: VerificationStatus::DoubleVerified,
        rejection: None,
        confidence: 90,
        detected_at: now,
    }
}

async fn persist_verdict(state: &Arc<SharedState>, verdict: OpportunityVerdict) {
    match sqlite::persist_verdict(&state.db, state.session_id.load(Ordering::Relaxed), &verdict) {
        Ok(_) => {
            info!(
                decision = ?verdict.decision,
                net = verdict.costs.net_profit_usd,
                "verdict persisted"
            );
        }
        Err(e) => error!(err = %e, "persist failed"),
    }
    {
        let mut recents = state.recent_verdicts.write().await;
        recents.push(verdict);
        let len = recents.len();
        if len > 200 {
            recents.drain(0..len - 200);
        }
    }
}
