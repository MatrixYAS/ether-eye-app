// Engine HTTP API (Axum) — served on localhost only.
//
// Endpoints (all JSON):
//   GET  /api/health                  engine health + last tick
//   GET  /api/config                  current configuration
//   POST /api/config                  update thresholds (persisted in env at runtime)
//   GET  /api/session                 current scanner session stats
//   GET  /api/opportunities?limit=N   recent verdicts
//   GET  /api/opportunities/:id       full verdict with legs
//   GET  /api/priority-fees           latest estimated priority fee
//   POST /api/scan/once               run one deterministic demo tick
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::engine::SharedState;
use crate::store::sqlite;
use crate::types::*;

pub fn build_router(state: Arc<SharedState>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(get_config).post(update_config))
        .route("/api/session", get(session))
        .route("/api/opportunities", get(opportunities))
        .route("/api/opportunities/{id}", get(opportunity_by_id))
        .route("/api/priority-fees", get(priority_fees))
        .route("/api/scan/once", post(scan_once))
        .with_state(state)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    engine: &'static str,
    running: bool,
    demo_mode: bool,
    last_tick_at: Option<String>,
}

async fn health(State(s): State<Arc<SharedState>>) -> Json<Health> {
    let last = s.last_tick_at.read().await;
    Json(Health {
        status: "ok",
        engine: "ether_engine",
        running: s.running.load(std::sync::atomic::Ordering::Relaxed),
        demo_mode: s.demo_mode,
        last_tick_at: last.as_ref().map(|t| t.to_rfc3339()),
    })
}

#[derive(Serialize)]
struct ConfigView {
    rpc_provider_url: String,
    jito_enabled: bool,
    max_quote_age_ms: u64,
    min_net_profit_usd: f64,
    safety_buffer_bps: u64,
    priority_fee_assumption_lamports: u64,
    jito_tip_assumption_lamports: u64,
    engine_port: u16,
    sol_usd_price: f64,
    scanner_tick_ms: u64,
    demo_mode: bool,
}

async fn get_config(State(s): State<Arc<SharedState>>) -> Json<ConfigView> {
    let c = s.config.lock().unwrap();
    Json(ConfigView {
        rpc_provider_url: c.rpc_provider_url.clone(),
        jito_enabled: c.jito_enabled,
        max_quote_age_ms: c.max_quote_age_ms,
        min_net_profit_usd: c.min_net_profit_usd,
        safety_buffer_bps: c.safety_buffer_bps,
        priority_fee_assumption_lamports: c.priority_fee_assumption_lamports,
        jito_tip_assumption_lamports: c.jito_tip_assumption_lamports,
        engine_port: c.engine_port,
        sol_usd_price: c.sol_usd_price,
        scanner_tick_ms: c.scanner_tick_ms,
        demo_mode: c.demo_mode,
    })
}

#[derive(Deserialize)]
struct ConfigUpdate {
    max_quote_age_ms: Option<u64>,
    min_net_profit_usd: Option<f64>,
    safety_buffer_bps: Option<u64>,
    jito_enabled: Option<bool>,
    scanner_tick_ms: Option<u64>,
}

async fn update_config(
    State(s): State<Arc<SharedState>>,
    Json(body): Json<ConfigUpdate>,
) -> Json<ConfigView> {
    // NOTE: runtime mutation of thresholds. In a real deployment you would also
    // persist these to disk so a restart keeps them; the config is kept in the
    // shared state here for simplicity.
    if let Some(v) = body.max_quote_age_ms {
        s.config.lock().unwrap().max_quote_age_ms = v;
    }
    if let Some(v) = body.min_net_profit_usd {
        s.config.lock().unwrap().min_net_profit_usd = v;
    }
    if let Some(v) = body.safety_buffer_bps {
        s.config.lock().unwrap().safety_buffer_bps = v;
    }
    if let Some(v) = body.jito_enabled {
        s.config.lock().unwrap().jito_enabled = v;
    }
    if let Some(v) = body.scanner_tick_ms {
        s.config.lock().unwrap().scanner_tick_ms = v;
    }
    get_config(State(s)).await
}

#[derive(Serialize)]
struct SessionView {
    id: i64,
    status: String,
    rpc_provider: String,
    markets_scanned: i64,
    opportunities_found: i64,
    profitable: i64,
    rejected: i64,
}

async fn session(State(s): State<Arc<SharedState>>) -> Json<SessionView> {
    let conn = s.db.lock().expect("db lock poisoned");
    let sid = s.session_id.load(std::sync::atomic::Ordering::Relaxed);
    let mut stmt = conn
        .prepare(
            "SELECT id, status, rpc_provider, markets_scanned, opportunities_found,
                    profitable, rejected
             FROM scanner_sessions WHERE id = ?1",
        )
        .expect("invalid query");
    let row = stmt
        .query_row(params![sid], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })
        .unwrap_or_else(|_| panic!("session {} missing", sid));
    Json(SessionView {
        id: row.0,
        status: row.1,
        rpc_provider: row.2,
        markets_scanned: row.3,
        opportunities_found: row.4,
        profitable: row.5,
        rejected: row.6,
    })
}

#[derive(Serialize, Deserialize, Clone)]
pub struct OpportunityRow {
    pub id: i64,
    pub detected_at: String,
    pub detected_slot: i64,
    pub status: String,
    pub input_mint: String,
    pub output_mint: String,
    pub route: String,
    pub size_usd: f64,
    pub optimal_size_usd: Option<f64>,
    pub gross_profit_usd: f64,
    pub dex_fees_usd: f64,
    pub price_impact_usd: f64,
    pub network_base_fee_usd: f64,
    pub priority_fee_usd: f64,
    pub tip_allowance_usd: f64,
    pub safety_buffer_usd: f64,
    pub net_profit_usd: f64,
    pub profit_bps: f64,
    pub quote_age_ms: i64,
    pub state_slot: i64,
    pub first_check_usd: f64,
    pub second_check_usd: Option<f64>,
    pub verification_status: String,
    pub confidence: i64,
    pub rejection_reason: Option<String>,
    pub created_at: String,
}

#[derive(Deserialize)]
struct OpportunitiesQuery {
    limit: Option<u32>,
}

async fn opportunities(
    State(s): State<Arc<SharedState>>,
    Query(q): Query<OpportunitiesQuery>,
) -> Result<Json<Vec<OpportunityRow>>, StatusCode> {
    let conn = s
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let limit = q.limit.unwrap_or(50).min(500);
    let mut stmt = conn
        .prepare(
            "SELECT id, detected_at, detected_slot, status, input_mint, output_mint, route,
                    size_usd, optimal_size_usd, gross_profit_usd, dex_fees_usd,
                    price_impact_usd, network_base_fee_usd, priority_fee_usd,
                    tip_allowance_usd, safety_buffer_usd, net_profit_usd, profit_bps,
                    quote_age_ms, state_slot, first_check_usd, second_check_usd,
                    verification_status, confidence, rejection_reason, created_at
             FROM opportunities ORDER BY id DESC LIMIT ?1",
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let rows = stmt
        .query_map(params![limit], |r| {
            Ok(OpportunityRow {
                id: r.get(0)?,
                detected_at: r.get(1)?,
                detected_slot: r.get(2)?,
                status: r.get(3)?,
                input_mint: r.get(4)?,
                output_mint: r.get(5)?,
                route: r.get(6)?,
                size_usd: r.get(7)?,
                optimal_size_usd: r.get(8)?,
                gross_profit_usd: r.get(9)?,
                dex_fees_usd: r.get(10)?,
                price_impact_usd: r.get(11)?,
                network_base_fee_usd: r.get(12)?,
                priority_fee_usd: r.get(13)?,
                tip_allowance_usd: r.get(14)?,
                safety_buffer_usd: r.get(15)?,
                net_profit_usd: r.get(16)?,
                profit_bps: r.get(17)?,
                quote_age_ms: r.get(18)?,
                state_slot: r.get(19)?,
                first_check_usd: r.get(20)?,
                second_check_usd: r.get(21)?,
                verification_status: r.get(22)?,
                confidence: r.get(23)?,
                rejection_reason: r.get(24)?,
                created_at: r.get(25)?,
            })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let rows: Vec<OpportunityRow> = rows.filter_map(|r| r.ok()).collect();
    Ok(Json(rows))
}

#[derive(Serialize, Deserialize, Clone)]
pub struct LegRow {
    pub id: i64,
    pub opportunity_id: i64,
    pub leg_index: i64,
    pub venue: String,
    pub market: String,
    pub token_in: String,
    pub token_out: String,
    pub amount_in: String,
    pub amount_out: String,
    pub fee_lamports: i64,
    pub price_impact_bps: f64,
    pub minimum_output: String,
    pub liquidity_usd: f64,
    pub quote_slot: i64,
    pub quote_ts: String,
    pub source: String,
    pub liquidity_ok: bool,
}

#[derive(Serialize)]
struct OpportunityDetail {
    opportunity: OpportunityRow,
    legs: Vec<LegRow>,
}

async fn opportunity_by_id(
    State(s): State<Arc<SharedState>>,
    Path(id): Path<i64>,
) -> Result<Json<OpportunityDetail>, StatusCode> {
    let conn = s
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, detected_at, detected_slot, status, input_mint, output_mint, route,
                    size_usd, optimal_size_usd, gross_profit_usd, dex_fees_usd,
                    price_impact_usd, network_base_fee_usd, priority_fee_usd,
                    tip_allowance_usd, safety_buffer_usd, net_profit_usd, profit_bps,
                    quote_age_ms, state_slot, first_check_usd, second_check_usd,
                    verification_status, confidence, rejection_reason, created_at
             FROM opportunities WHERE id = ?1",
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let opp = stmt
        .query_row(params![id], |r| {
            Ok(OpportunityRow {
                id: r.get(0)?,
                detected_at: r.get(1)?,
                detected_slot: r.get(2)?,
                status: r.get(3)?,
                input_mint: r.get(4)?,
                output_mint: r.get(5)?,
                route: r.get(6)?,
                size_usd: r.get(7)?,
                optimal_size_usd: r.get(8)?,
                gross_profit_usd: r.get(9)?,
                dex_fees_usd: r.get(10)?,
                price_impact_usd: r.get(11)?,
                network_base_fee_usd: r.get(12)?,
                priority_fee_usd: r.get(13)?,
                tip_allowance_usd: r.get(14)?,
                safety_buffer_usd: r.get(15)?,
                net_profit_usd: r.get(16)?,
                profit_bps: r.get(17)?,
                quote_age_ms: r.get(18)?,
                state_slot: r.get(19)?,
                first_check_usd: r.get(20)?,
                second_check_usd: r.get(21)?,
                verification_status: r.get(22)?,
                confidence: r.get(23)?,
                rejection_reason: r.get(24)?,
                created_at: r.get(25)?,
            })
        })
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let mut lstmt = conn
        .prepare(
            "SELECT id, opportunity_id, leg_index, venue, market, token_in, token_out,
                    amount_in, amount_out, fee_lamports, price_impact_bps, minimum_output,
                    liquidity_usd, quote_slot, quote_ts, source, liquidity_ok
             FROM opportunity_legs WHERE opportunity_id = ?1 ORDER BY leg_index",
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let legs = lstmt
        .query_map(params![id], |r| {
            Ok(LegRow {
                id: r.get(0)?,
                opportunity_id: r.get(1)?,
                leg_index: r.get(2)?,
                venue: r.get(3)?,
                market: r.get(4)?,
                token_in: r.get(5)?,
                token_out: r.get(6)?,
                amount_in: r.get(7)?,
                amount_out: r.get(8)?,
                fee_lamports: r.get(9)?,
                price_impact_bps: r.get(10)?,
                minimum_output: r.get(11)?,
                liquidity_usd: r.get(12)?,
                quote_slot: r.get(13)?,
                quote_ts: r.get(14)?,
                source: r.get(15)?,
                liquidity_ok: r.get::<_, i32>(16)? != 0,
            })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let legs: Vec<LegRow> = legs.filter_map(|r| r.ok()).collect();
    Ok(Json(OpportunityDetail {
        opportunity: opp,
        legs,
    }))
}

#[derive(Serialize)]
struct PriorityFeeView {
    lamports: Option<u64>,
    assumption_lamports: u64,
    using_live: bool,
}

async fn priority_fees(State(s): State<Arc<SharedState>>) -> Json<PriorityFeeView> {
    let lf = s.latest_prioritization_fee.read().await;
    Json(PriorityFeeView {
        lamports: *lf,
        assumption_lamports: { let __c = s.config.lock().unwrap(); __c.priority_fee_assumption_lamports },
        using_live: { let __c = s.config.lock().unwrap(); __c.use_live_priority_fees },
    })
}

#[derive(Serialize)]
struct ScanOnceResult {
    triggered: bool,
    demo: bool,
}

async fn scan_once(State(s): State<Arc<SharedState>>) -> Json<ScanOnceResult> {
    // One synchronous demo tick for on-demand testing.
    let _ = crate::engine::run_tick_demo_for_api(&s);
    Json(ScanOnceResult {
        triggered: true,
        demo: s.demo_mode,
    })
}
