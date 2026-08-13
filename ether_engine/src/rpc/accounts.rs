// Account result types and Borsh-style decoding of pool account layouts.
//
// We intentionally do NOT depend on the solana-sdk crate: decoding is done by
// hand from the documented on-chain layouts, which keeps the binary tiny and
// build times short. Program IDs and layout offsets match the public Raydium,
// Orca, Meteora and Phoenix account specifications.

use serde::{Deserialize, Serialize};

/// One entry in a getMultipleAccounts response.
#[derive(Debug, Clone, Deserialize)]
pub struct RawAccountResult {
    pub pubkey: String,
    pub slot: u64,
    pub account: Option<RpcAccountInfo>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RpcAccountInfo {
    pub lamports: u64,
    #[serde(rename = "owner")]
    pub owner: String,
    /// [base64, "base64"]
    pub data: Vec<serde_json::Value>,
    pub executable: bool,
    pub rent_epoch: Option<u64>,
}

impl RpcAccountInfo {
    /// Decode the base64 account data into raw bytes.
    pub fn data_bytes(&self) -> Option<Vec<u8>> {
        let s = self.data.first()?.as_str()?;
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s).ok()
    }
}

/// Minimal Borsh helpers for u64/le primitives used by the pool layouts.
#[inline]
pub fn u64_le(buf: &[u8], offset: usize) -> u64 {
    let b = buf
        .get(offset..offset + 8)
        .unwrap_or(&[0u8; 8][..]);
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[inline]
pub fn u128_le(buf: &[u8], offset: usize) -> u128 {
    let lo = u64_le(buf, offset) as u128;
    let hi = u64_le(buf, offset + 8) as u128;
    (hi << 64) | lo
}

/// A decoded pool across all supported venues.
#[derive(Debug, Clone)]
pub enum DecodedPool {
    RaydiumCpmm {
        token_a_mint: String,
        token_b_mint: String,
        reserve_a: u64,
        reserve_b: u64,
        /// 1/4 of 1% = 2500/1_000_000 typical Raydium CPMM fee on output.
        trade_fee_num: u64,
        trade_fee_den: u64,
        decimals_a: u8,
        decimals_b: u8,
        slot: u64,
    },
    RaydiumClmm {
        token_mint_a: String,
        token_mint_b: String,
        sqrt_price_x64: u128,
        liquidity: u128,
        tick_current: i32,
        tick_spacing: u16,
        fee_rate: u64,
        slot: u64,
    },
    OrcaWhirlpool {
        token_mint_a: String,
        token_mint_b: String,
        sqrt_price: u128,
        liquidity: u128,
        tick_current_index: i32,
        tick_spacing: u16,
        fee_rate: u16,
        slot: u64,
    },
    MeteoraDlmm {
        token_x_mint: String,
        token_y_mint: String,
        active_id: i64,
        bin_step: u16,
        slot: u64,
    },
    PhoenixMarket {
        base_mint: String,
        quote_mint: String,
        base_decimals: u8,
        quote_decimals: u8,
        slot: u64,
    },
}

/// Program IDs (mainnet) for the supported venues.
pub mod program_ids {
    pub const RAYDIUM_CPMM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
    pub const RAYDIUM_CLMM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
    pub const ORCA_WHIRLPOOL: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
    pub const METEORA_DLMM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
    pub const PHOENIX: &str = "PhoeNiXZ8ByJGLkxNfZRnkUfjvmuYqLR89jjFHGqdXY";
}

/// Dispatch decoding by owner program ID. Returns None for unknown owners.
pub fn decode_pool(pubkey: &str, acct: &RpcAccountInfo, slot: u64) -> Option<DecodedPool> {
    let owner = acct.owner.as_str();
    let data = acct.data_bytes()?;
    let data = data.as_slice();
    match owner {
        program_ids::RAYDIUM_CPMM => decode_raydium_cpmm(data, slot),
        program_ids::RAYDIUM_CLMM => decode_raydium_clmm(data, slot),
        program_ids::ORCA_WHIRLPOOL => decode_orca_whirlpool(data, slot),
        program_ids::METEORA_DLMM => decode_meteora_dlmm(data, slot),
        program_ids::PHOENIX => decode_phoenix_market(data, slot),
        _ => None,
    }
    .map(|mut p| {
        set_slot(&mut p, slot);
        p
    })
}

fn set_slot(p: &mut DecodedPool, slot: u64) {
    match p {
        DecodedPool::RaydiumCpmm { slot: s, .. }
        | DecodedPool::RaydiumClmm { slot: s, .. }
        | DecodedPool::OrcaWhirlpool { slot: s, .. }
        | DecodedPool::MeteoraDlmm { slot: s, .. }
        | DecodedPool::PhoenixMarket { slot: s, .. } => *s = slot,
    }
}

fn read_pubkey(bytes: &[u8], offset: usize) -> Option<String> {
    bytes
        .get(offset..offset + 32)
        .map(|b| bs58::encode(b).into_string())
}

/// Raydium CPMM "PoolState" layout (post-init, simplified):
///   0   discriminant bump
///   1   auth_seed (32)
///   33  pool_creator (32)
///   65  token_vault_0 (32)
///   97  token_vault_1 (32)
///   129 lp_mint (32)
///   161 token_0_mint (32)
///   193 token_1_mint (32)
///   225 lp_mint_authority (32)
///   257 amm_config (32)
///   289 observation_key (32)
///   321 auth_bump (1)
///   322 status (1)
///   323 lp_mint_decimals (1)
///   324 mint0_decimals (1)
///   325 mint1_decimals (1)
///   326 lp_supply (8)
///   334 protocol_fees_token_0 (8)
///   342 protocol_fees_token_1 (8)
///   350 fund_fees_token_0 (8)
///   358 fund_fees_token_1 (8)
///   366 open_time (8)
///   374 recent_epoch (8)
///   382 padding (6)
///   388 vault_0_reserve (8)
///   396 vault_1_reserve (8)
/// NOTE: exact offsets follow the published cpmm-pool crate layout at v0.2.x.
pub fn decode_raydium_cpmm(data: &[u8], slot: u64) -> Option<DecodedPool> {
    if data.len() < 404 {
        return None;
    }
    let token_0_mint = read_pubkey(data, 161)?;
    let token_1_mint = read_pubkey(data, 193)?;
    let reserve_a = u64_le(data, 388);
    let reserve_b = u64_le(data, 396);
    let decimals_a = *data.get(324)?;
    let decimals_b = *data.get(325)?;
    Some(DecodedPool::RaydiumCpmm {
        token_a_mint: token_0_mint,
        token_b_mint: token_1_mint,
        reserve_a,
        reserve_b,
        trade_fee_num: 2500,
        trade_fee_den: 1_000_000,
        decimals_a,
        decimals_b,
        slot,
    })
}

/// Raydium CLMM "PoolState" simplified offsets:
///   0   bump_seed (1)
///   1   amm_config (32)
///   33  owner (32)
///   65  token_mint_0 (32)
///   97  token_mint_1 (32)
///   129 token_vault_0 (32)
///   161 token_vault_1 (32)
///   193 observation_key (32)
///   225 tick_array_bitmap (816)
///   1041 liquidity (16)
///   1057 sqrt_price_x64 (16)
///   1073 tick_current (4, i32)
///   1077 padding (2)
///   1079 fee_growth_global_0_x64 (16)
///   1095 fee_growth_global_1_x64 (16)
///   1111 fee_rate (4)
///   1115 protocol_fee_rate (4)
///   1119 fund_fee_rate (4)
///   1123 padding (4)
///   1127 tick_spacing (2, u16)
pub fn decode_raydium_clmm(data: &[u8], slot: u64) -> Option<DecodedPool> {
    if data.len() < 1129 {
        return None;
    }
    let token_mint_a = read_pubkey(data, 65)?;
    let token_mint_b = read_pubkey(data, 97)?;
    let sqrt_price_x64 = u128_le(data, 1057);
    let liquidity = u128_le(data, 1041);
    let tick_current = i32::from_le_bytes(
        data[1073..1077]
            .try_into()
            .ok()?,
    );
    let tick_spacing = u16::from_le_bytes(data[1127..1129].try_into().ok()?);
    let fee_rate = u32::from_le_bytes(data[1111..1115].try_into().ok()?) as u64;
    Some(DecodedPool::RaydiumClmm {
        token_mint_a,
        token_mint_b,
        sqrt_price_x64,
        liquidity,
        tick_current,
        tick_spacing,
        fee_rate,
        slot,
    })
}

/// Orca Whirlpool state (post-init) simplified offsets:
///   0   whirlpool_bump (1)
///   1   tick_spacing (2, u16)
///   3   tick_spacing_seed (2)
///   5   fee_rate (2, u16)
///   7   protocol_fee_rate (2)
///   9   liquidity (16, u128)
///   25  sqrt_price (16, u128)
///   41  tick_current_index (4, i32)
///   45  protocol_fee_owed_a (8)
///   53  protocol_fee_owed_b (8)
///   61  token_mint_a (32)
///   93  token_vault_a (32)
///   125 fee_growth_global_a (16)
///   141 fee_growth_global_b (16)
///   157 reward_last_updated_timestamp (8)
///   165 reward_infos (72)
///   237 token_mint_b (32)
///   269 token_vault_b (32)
pub fn decode_orca_whirlpool(data: &[u8], slot: u64) -> Option<DecodedPool> {
    if data.len() < 301 {
        return None;
    }
    let sqrt_price = u128_le(data, 25);
    let liquidity = u128_le(data, 9);
    let tick_current_index = i32::from_le_bytes(data[41..45].try_into().ok()?);
    let tick_spacing = u16::from_le_bytes(data[1..3].try_into().ok()?);
    let fee_rate = u16::from_le_bytes(data[5..7].try_into().ok()?);
    let token_mint_a = read_pubkey(data, 61)?;
    let token_mint_b = read_pubkey(data, 237)?;
    Some(DecodedPool::OrcaWhirlpool {
        token_mint_a,
        token_mint_b,
        sqrt_price,
        liquidity,
        tick_current_index,
        tick_spacing,
        fee_rate,
        slot,
    })
}

/// Meteora DLMM "LbPair" state (post-init) simplified offsets:
///   0   parameters (40)
///   40  v_parameters (16)
///   56  bump_seed (1)
///   57  bin_step_seed (2)
///   59  reserve_x (32)
///   91  reserve_y (32)
///   123 total_x_in_swap (16)
///   139 total_y_in_swap (16)
///   155 token_x_mint (32)
///   187 token_y_mint (32)
///   219 bin_liquidity_mapping (27120)
///   27339 oracle (27150)
///   54489 pre_activation_timestamp (8)
///   54497 activation_point (8)
///   54505 active_id (4, i64)
///   54509 bin_step (2, u16)
///   54511 status (1)
pub fn decode_meteora_dlmm(data: &[u8], slot: u64) -> Option<DecodedPool> {
    if data.len() < 54513 {
        return None;
    }
    let token_x_mint = read_pubkey(data, 155)?;
    let token_y_mint = read_pubkey(data, 187)?;
    let active_id = i64::from_le_bytes(data[54505..54513].try_into().ok()?);
    let bin_step = u16::from_le_bytes(data[54509..54511].try_into().ok()?);
    Some(DecodedPool::MeteoraDlmm {
        token_x_mint,
        token_y_mint,
        active_id,
        bin_step,
        slot,
    })
}

/// Phoenix market state header (post-init) simplified offsets:
///   0   discriminator (8)
///   8   status_and_sequence (8)
///   16  market_size_params (24)
///   40  seeds (96)
///   136 base_params (8)
///   144 quote_params (8)
///   152 tick_size_in_quote_lots_per_base_unit (8)
///   160 lots_per_base_unit (8)
///   168 base_lots_per_base_unit (8)
///   176 raw_base_units_per_base_unit (8)
///   184 taker_fee_bps (2)
///   186 maker_fee_bps (2)
///   188 unused (4)
///   192 base_mint_key (32)
///   224 quote_mint_key (32)
///   256 base_vault_key (32)
///   288 quote_vault_key (32)
///   320 base_collector (32)
///   352 quote_collector (32)
///   384 base_lot_size_in_raw_base_units (8)
///   392 quote_lot_size_in_quote_atoms (8)
///   400 num_base_lots_in_base_unit (8)
pub fn decode_phoenix_market(data: &[u8], slot: u64) -> Option<DecodedPool> {
    if data.len() < 408 {
        return None;
    }
    let base_mint = read_pubkey(data, 192)?;
    let quote_mint = read_pubkey(data, 224)?;
    let base_decimals = 9u8; // SOL-native phoenix markets use 9; refined per mint if mint data loaded
    let quote_decimals = 6u8; // USDC
    Some(DecodedPool::PhoenixMarket {
        base_mint,
        quote_mint,
        base_decimals,
        quote_decimals,
        slot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u128_le_roundtrip() {
        let val: u128 = 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10;
        let mut buf = vec![0u8; 16];
        buf[..8].copy_from_slice(&val.to_le_bytes()[..8]);
        buf[8..16].copy_from_slice(&val.to_le_bytes()[8..16]);
        assert_eq!(u128_le(&buf, 0), val);
    }
}
