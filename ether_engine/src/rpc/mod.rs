// Solana RPC provider layer.
//
// The only required configuration is RPC_PROVIDER_URL. The provider abstraction
// means Helius, QuikNode, or any compatible endpoint can be swapped later with
// no code change. Public RPC works for development.
//
// Provides:
//   - get_multiple_accounts (batched, max 100 per RPC call per Solana docs)
//   - current slot tracking
//   - get_recent_prioritization_fees estimation
pub mod accounts;

use crate::error::EngineError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MAX_ACCOUNTS_PER_REQUEST: usize = 100;

/// JSON-RPC envelope sent to any Solana-compatible endpoint.
#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: serde_json::Value,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RpcContext {
    pub slot: u64,
    #[serde(rename = "apiVersion")]
    pub api_version: Option<String>,
}

/// Live priority-fee snapshot for one slot, returned by getRecentPrioritizationFees.
#[derive(Deserialize, Debug, Clone)]
pub struct PrioritizationFeeSample {
    pub slot: u64,
    #[serde(rename = "prioritizationFee")]
    pub prioritization_fee: u64,
}

pub struct RpcProvider {
    client: Client,
    endpoint: String,
    /// Monotonic request id to keep JSON-RPC calls ordered per connection.
    next_id: AtomicU64,
}

impl RpcProvider {
    pub fn new(endpoint: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client builder is infallible with valid config");
        Self {
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            next_id: AtomicU64::new(1),
        }
    }

    /// Raw JSON-RPC call.
    pub async fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, EngineError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = RpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let resp = self
            .client
            .post(&self.endpoint)
            .json(&req)
            .send()
            .await
            .map_err(|e| EngineError::Rpc(format!("transport: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(EngineError::Rpc(format!("HTTP {status}")));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| EngineError::Rpc(format!("decode: {e}")))?;
        if let Some(err) = body.get("error") {
            return Err(EngineError::Rpc(format!("RPC error: {err}")));
        }
        body.get("result")
            .cloned()
            .ok_or_else(|| EngineError::Rpc("missing result field".into()))
    }

    /// Current cluster slot via getSlot.
    pub async fn current_slot(&self) -> Result<u64, EngineError> {
        let v = self.call("getSlot", serde_json::json!([])).await?;
        v.as_u64().ok_or_else(|| EngineError::Rpc("invalid slot".into()))
    }

    /// Recent prioritization fees; returns samples across the last N slots.
    pub async fn recent_prioritization_fees(
        &self,
        num_slots: Option<u64>,
    ) -> Result<Vec<PrioritizationFeeSample>, EngineError> {
        let params = match num_slots {
            Some(n) => serde_json::json!([[null], { "numSlots": n }]),
            None => serde_json::json!([[]]),
        };
        let v = self.call("getRecentPrioritizationFees", params).await?;
        serde_json::from_value(v)
            .map_err(|e| EngineError::Rpc(format!("fee decode: {e}")))
    }

    /// Batch-fetch up to ~100 accounts per request, chunking transparently.
    pub async fn get_multiple_accounts(
        &self,
        pubkeys: &[String],
    ) -> Result<Vec<accounts::RawAccountResult>, EngineError> {
        let mut out = Vec::with_capacity(pubkeys.len());
        for chunk in pubkeys.chunks(MAX_ACCOUNTS_PER_REQUEST) {
            let v = self
                .call(
                    "getMultipleAccounts",
                    serde_json::json!([chunk, {
                        "encoding": "base64",
                        "commitment": "confirmed"
                    }]),
                )
                .await?;
            let ctx: RpcContext = serde_json::from_value(v.get("context").cloned().unwrap_or_default())
                .map_err(|e| EngineError::Rpc(format!("context decode: {e}")))?;
            let values: Vec<Option<accounts::RpcAccountInfo>> =
                serde_json::from_value(v.get("value").cloned().unwrap_or_default())
                    .map_err(|e| EngineError::Rpc(format!("accounts decode: {e}")))?;
            out.extend(
                chunk
                    .iter()
                    .zip(values.into_iter())
                    .map(|(pk, acct)| accounts::RawAccountResult {
                        pubkey: pk.clone(),
                        slot: ctx.slot,
                        account: acct,
                    }),
            );
        }
        Ok(out)
    }

    /// Human-readable endpoint for auditing.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Clone for RpcProvider {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            endpoint: self.endpoint.clone(),
            next_id: AtomicU64::new(self.next_id.load(Ordering::Relaxed)),
        }
    }
}

impl std::fmt::Debug for RpcProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcProvider")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

/// Fetch and decode a single venue market's on-chain accounts at the latest
/// slot, producing a DecodedPool ready for the venue adapter.
pub async fn fetch_pool(
    provider: &RpcProvider,
    market_address: &str,
) -> Result<accounts::DecodedPool, EngineError> {
    let resp = provider
        .get_multiple_accounts(&[market_address.to_string()])
        .await?;
    let first = resp
        .into_iter()
        .next()
        .ok_or_else(|| EngineError::Rpc("account not returned".into()))?;
    let rpc_acct = first
        .account
        .ok_or_else(|| EngineError::Rpc(format!("{market_address} not found")))?;
    let slot = first.slot;
    accounts::decode_pool(&first.pubkey, &rpc_acct, slot)
        .ok_or_else(|| EngineError::Decode(format!("unrecognized pool at {market_address}")))
}
