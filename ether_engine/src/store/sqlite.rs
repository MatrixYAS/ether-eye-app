// Embedded SQLite store — the single persistence layer (no external DB).
//
// Schema:
//   scanner_sessions      — one row per engine run (start/stop/status/rpc)
//   market_states         — decoded market snapshots per slot (audit trail)
//   opportunities         — candidate routes with verdicts + rejection reasons
//   opportunity_legs      — per-leg quote detail with source labels
use crate::config::EngineConfig;
use crate::error::EngineError;
use crate::types::{
    CostBreakdown, Decision, OpportunityVerdict, RejectionReason, Venue, VerificationStatus,
};
use rusqlite::{params, Connection, OpenFlags};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub type DbHandle = Arc<Mutex<Connection>>;

pub fn open_db(cfg: &EngineConfig) -> Result<DbHandle, EngineError> {
    std::fs::create_dir_all(&cfg.data_dir)
        .map_err(|e| EngineError::Database(format!("mkdir {e}")))?;
    let path = PathBuf::from(&cfg.data_dir).join("ether_engine.db");
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|e| EngineError::Database(format!("open: {e}")))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=2000; PRAGMA synchronous=NORMAL;",
    )
    .map_err(|e| EngineError::Database(format!("pragma: {e}")))?;
    migrate(&conn)?;
    Ok(Arc::new(Mutex::new(conn)))
}

fn migrate(conn: &Connection) -> Result<(), EngineError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS scanner_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at TEXT NOT NULL,
            stopped_at TEXT,
            status TEXT NOT NULL DEFAULT 'running',
            rpc_provider TEXT NOT NULL,
            engine_port INTEGER NOT NULL,
            jito_enabled INTEGER NOT NULL DEFAULT 0,
            max_quote_age_ms INTEGER NOT NULL,
            min_net_profit_usd REAL NOT NULL,
            safety_buffer_bps INTEGER NOT NULL,
            markets_scanned INTEGER NOT NULL DEFAULT 0,
            opportunities_found INTEGER NOT NULL DEFAULT 0,
            profitable INTEGER NOT NULL DEFAULT 0,
            rejected INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS market_states (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            venue TEXT NOT NULL,
            market TEXT NOT NULL,
            mint_a TEXT NOT NULL,
            mint_b TEXT NOT NULL,
            slot INTEGER NOT NULL,
            captured_at TEXT NOT NULL,
            state_hash TEXT NOT NULL,
            liquidity_usd REAL NOT NULL DEFAULT 0,
            price REAL NOT NULL DEFAULT 0,
            fee_bps REAL NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_market_states_slot ON market_states(slot);
        CREATE TABLE IF NOT EXISTS opportunities (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id INTEGER,
            detected_at TEXT NOT NULL,
            detected_slot INTEGER NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            input_mint TEXT NOT NULL,
            output_mint TEXT NOT NULL,
            input_amount TEXT NOT NULL,
            route TEXT NOT NULL,
            size_usd REAL NOT NULL DEFAULT 0,
            optimal_size_usd REAL,
            gross_profit_usd REAL NOT NULL DEFAULT 0,
            dex_fees_usd REAL NOT NULL DEFAULT 0,
            price_impact_usd REAL NOT NULL DEFAULT 0,
            network_base_fee_usd REAL NOT NULL DEFAULT 0,
            priority_fee_usd REAL NOT NULL DEFAULT 0,
            tip_allowance_usd REAL NOT NULL DEFAULT 0,
            safety_buffer_usd REAL NOT NULL DEFAULT 0,
            net_profit_usd REAL NOT NULL DEFAULT 0,
            profit_bps REAL NOT NULL DEFAULT 0,
            quote_age_ms INTEGER NOT NULL DEFAULT 0,
            state_slot INTEGER NOT NULL,
            first_check_usd REAL NOT NULL DEFAULT 0,
            second_check_usd REAL,
            verification_status TEXT NOT NULL DEFAULT 'pending',
            verification_ts TEXT,
            confidence INTEGER NOT NULL DEFAULT 0,
            rejection_reason TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS opportunity_legs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            opportunity_id INTEGER NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
            leg_index INTEGER NOT NULL,
            venue TEXT NOT NULL,
            market TEXT NOT NULL,
            token_in TEXT NOT NULL,
            token_out TEXT NOT NULL,
            amount_in TEXT NOT NULL,
            amount_out TEXT NOT NULL,
            fee_lamports INTEGER NOT NULL DEFAULT 0,
            price_impact_bps REAL NOT NULL DEFAULT 0,
            minimum_output TEXT NOT NULL,
            liquidity_usd REAL NOT NULL DEFAULT 0,
            quote_slot INTEGER NOT NULL,
            quote_ts TEXT NOT NULL,
            source TEXT NOT NULL,
            liquidity_ok INTEGER NOT NULL DEFAULT 1
        );
        ",
    )
    .map_err(|e| EngineError::Database(format!("migrate: {e}")))?;
    Ok(())
}

/// Start a new scanner session and return its id.
pub fn start_session(db: &DbHandle, cfg: &EngineConfig) -> Result<i64, EngineError> {
    let conn = db
        .lock()
        .map_err(|e| EngineError::Database(format!("lock: {e}")))?;
    conn.execute(
        "INSERT INTO scanner_sessions (started_at, rpc_provider, engine_port, jito_enabled,
         max_quote_age_ms, min_net_profit_usd, safety_buffer_bps)
         VALUES (datetime('now'), ?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            cfg.rpc_provider_url,
            cfg.engine_port,
            if cfg.jito_enabled { 1 } else { 0 },
            cfg.max_quote_age_ms,
            cfg.min_net_profit_usd,
            cfg.safety_buffer_bps,
        ],
    )
    .map_err(|e| EngineError::Database(format!("session insert: {e}")))?;
    Ok(conn.last_insert_rowid())
}

pub fn stop_session(db: &DbHandle, session_id: i64) -> Result<(), EngineError> {
    let conn = db
        .lock()
        .map_err(|e| EngineError::Database(format!("lock: {e}")))?;
    conn.execute(
        "UPDATE scanner_sessions SET stopped_at = datetime('now'), status = 'stopped'
         WHERE id = ?1",
        params![session_id],
    )
    .map_err(|e| EngineError::Database(format!("session stop: {e}")))?;
    Ok(())
}

pub fn increment_markets_scanned(db: &DbHandle, session_id: i64) -> Result<(), EngineError> {
    let conn = db
        .lock()
        .map_err(|e| EngineError::Database(format!("lock: {e}")))?;
    conn.execute(
        "UPDATE scanner_sessions SET markets_scanned = markets_scanned + 1 WHERE id = ?1",
        params![session_id],
    )
    .map_err(|e| EngineError::Database(format!("increment: {e}")))?;
    Ok(())
}

/// Persist a verified verdict. Returns the opportunity id.
pub fn persist_verdict(
    db: &DbHandle,
    session_id: i64,
    v: &OpportunityVerdict,
) -> Result<i64, EngineError> {
    let conn = db
        .lock()
        .map_err(|e| EngineError::Database(format!("lock: {e}")))?;
    let status = match v.decision {
        Decision::Profitable => "profitable",
        Decision::Rejected => "rejected",
    };
    let rejection = v.rejection.as_ref().map(ToString::to_string);
    let vs = match v.verification_status {
        VerificationStatus::Pending => "pending",
        VerificationStatus::SingleVerified => "single_verified",
        VerificationStatus::DoubleVerified => "double_verified",
        VerificationStatus::Invalidated => "invalidated",
    };
    let route = format!(
        "{} → {}",
        venue_name(&v.route.leg_a.venue),
        venue_name(&v.route.leg_b.venue)
    );
    conn.execute(
        "INSERT INTO opportunities
         (session_id, detected_at, detected_slot, status, input_mint, output_mint,
          input_amount, route, size_usd, optimal_size_usd, gross_profit_usd,
          dex_fees_usd, price_impact_usd, network_base_fee_usd, priority_fee_usd,
          tip_allowance_usd, safety_buffer_usd, net_profit_usd, profit_bps,
          quote_age_ms, state_slot, first_check_usd, second_check_usd,
          verification_status, confidence, rejection_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
        params![
            session_id,
            v.detected_at.to_rfc3339(),
            v.route.state_slot,
            status,
            v.route.input_mint,
            v.route.output_mint,
            v.route.leg_a.amount_in.to_string(),
            route,
            v.size_usd,
            v.optimal_size_usd,
            v.costs.gross_profit_usd,
            v.costs.dex_fees_usd,
            v.costs.price_impact_usd,
            v.costs.network_base_fee_usd,
            v.costs.priority_fee_usd,
            v.costs.tip_allowance_usd,
            v.costs.safety_buffer_usd,
            v.costs.net_profit_usd,
            v.costs.profit_bps,
            v.route.quote_age_ms,
            v.route.state_slot,
            v.first_check_usd,
            v.second_check_usd,
            vs,
            v.confidence,
            rejection,
        ],
    )
    .map_err(|e| EngineError::Database(format!("verdict insert: {e}")))?;
    let opp_id = conn.last_insert_rowid();
    for (i, leg) in [v.route.leg_a.clone(), v.route.leg_b.clone()].iter().enumerate() {
        conn.execute(
            "INSERT INTO opportunity_legs
             (opportunity_id, leg_index, venue, market, token_in, token_out,
              amount_in, amount_out, fee_lamports, price_impact_bps,
              minimum_output, liquidity_usd, quote_slot, quote_ts, source,
              liquidity_ok)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16)",
            params![
                opp_id,
                i,
                venue_name(&leg.venue),
                leg.market_address,
                leg.token_in,
                leg.token_out,
                leg.amount_in.to_string(),
                leg.amount_out.to_string(),
                // SQLite INTEGER maps to i64; store u64s as text to avoid
                // "out of range integral type conversion" on extreme values.
                leg.fee_lamports.to_string(),
                leg.price_impact_bps,
                leg.minimum_output.to_string(),
                leg.available_liquidity,
                leg.quote_slot.to_string(),
                leg.quote_ts.to_rfc3339(),
                leg.source,
                if leg.liquidity_ok { 1 } else { 0 },
            ],
        )
        .map_err(|e| EngineError::Database(format!("leg insert: {e}")))?;
    }
    let kind = if v.decision == Decision::Profitable {
        "profitable"
    } else {
        "rejected"
    };
    conn.execute(
        &format!(
            "UPDATE scanner_sessions SET opportunities_found = opportunities_found + 1,
             {kind} = {kind} + 1 WHERE id = ?1"
        ),
        params![session_id],
    )
    .map_err(|e| EngineError::Database(format!("session bump: {e}")))?;
    Ok(opp_id)
}

pub fn venue_name(venue: &Venue) -> String {
    venue.to_string()
}
