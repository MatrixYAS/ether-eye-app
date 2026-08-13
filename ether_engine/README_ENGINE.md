# Ether Eye — Deterministic Paper-Arbitrage Detector

Live, no-execution Solana cross-venue arbitrage scanner. A Rust engine sidecar performs route discovery, quote double-verification, trade-size optimization, and full-cost accounting, while a Node.js + React front end presents a dark-terminal audit trail. The system never signs or sends transactions: every output is a paper verdict with an explicit cost breakdown.

## Architecture

```
Browser (React 19 + Tailwind 4)
      │ tRPC polling
      ▼
Node.js server (Express + tRPC)   ── localhost proxy ──►  ether_engine (Rust)
server/engine.ts  (spawn + probe)                        tokio + axum
server/routers/engine.ts                                 SQLite (rusqlite)
                                                         4 DEX adapters
```

**`ether_engine` (Rust).** Reads market state from the Solana RPC and decodes pool accounts deterministically for five venues:

| Adapter | Model |
|---|---|
| Raydium CPMM | Constant product, fee-on-input |
| Raydium CLMM | Tick-array traversal, liquidity crossing |
| Orca Whirlpool | Fee growth, tick traversal |
| Meteora DLMM | Bin-state traversal with VWAP fills |
| Phoenix | Order-book bid/ask walking → VWAP |

The simulator computes, per leg: quoted output, DEX fees, price impact, minimum output, and available liquidity. Net profit is gross minus DEX fees, network base fee, priority-fee allowance, optional Jito tip, and the safety buffer. Every number carries a source label so the UI can explain **why** a route was profitable or rejected.

**Deterministic guarantees.** There is zero randomness anywhere in the signal path. Given identical account state, the engine always produces identical verdicts. An 8-case fixture suite (`tests/fixtures.rs`) pins known AMM states, order books, fee schedules, and trade sizes.

## Quick start

```bash
# 1. Build the Rust engine (requires Rust toolchain)
cd ether_engine
cargo build --release        # → target/release/ether_engine
cargo test --release --lib   # 3/3 core tests
cargo test --release --test fixtures  # 8/8 deterministic fixtures

# 2. Build the Node app
cd ..
pnpm install
pnpm build                   # or: pnpm dev
```

The Node server spawns `ether_engine` automatically on first request (see `server/engine.ts`). The web app is available at `http://localhost:3000`.

## Configuration

Copy `env.example` to `.env` and edit. All engine variables can also be changed live from the dashboard Config panel.

| Variable | Meaning |
|---|---|
| `RPC_PROVIDER_URL` | Solana RPC endpoint. If unreachable, the engine falls back to deterministic demo mode automatically |
| `JITO_ENABLED` | `1` = include a Jito tip assumption in the cost model; `0` = plain priority fees |
| `MAX_QUOTE_AGE_MS` | Maximum allowed age of a quote before it is rejected as stale |
| `MIN_NET_PROFIT_USD` | Minimum net profit a route must clear to be emitted |
| `SAFETY_BUFFER_BPS` | Extra safety margin in basis points applied on top of costs |
| `ENGINE_PORT` | Localhost port the Rust engine binds (default `18787`) |
| `DEMO_MODE` | `1` = synthetic markets (no RPC); `0` = live mainnet. Auto-enabled when RPC is unreachable |

## Engine HTTP API

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/health` | Engine status, mode, last tick |
| GET | `/api/config` | Current thresholds |
| POST | `/api/config` | Update thresholds (JSON body, same fields) |
| POST | `/api/scan/once` | Trigger a one-shot scan tick |
| GET | `/api/opportunities?limit=N` | Verdict audit trail |
| GET | `/api/opportunities/:id` | Detail: legs, costs, rejection reasons |
| GET | `/api/session` | Scanner session statistics |
| GET | `/api/priority-fees` | Live priority-fee estimate |

## SQLite audit trail

All verdicts persist to an embedded SQLite database (`data/ether_eye.db` by default) across four tables: `opportunities`, `opportunity_legs`, `market_states`, `scanner_sessions`. Rejected routes record explicit human-readable rejection reasons (stale quote, insufficient liquidity, excessive impact, fees erase profit, second quote invalidated), which power the **Why Profitable?** / **Rejected** panels in the dashboard.

## Testing

```bash
cd ether_engine && cargo test --release --lib --test fixtures
cd .. && npx vitest run        # 9/9 tRPC integration tests
```
