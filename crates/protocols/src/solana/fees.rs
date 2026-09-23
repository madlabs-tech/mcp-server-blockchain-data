//! Priority-fee percentiles + Jito tip. Owner: `solana` (T1.S2).
//!
//! `getRecentPrioritizationFees` returns, per recent slot (≤150), the *minimum* fee paid by a
//! landed tx that locked the given accounts: often 0, so these percentiles are a floor, not a
//! market price. Vendor oracles (Helius `getPriorityFeeEstimate`, QuickNode
//! `qn_estimatePriorityFees`) sit before `rpc` in the default order for that reason.
//! Prices are micro-lamports per compute unit. Total for a tx: [`total_fee_lamports`].

use bdm_config::ChainEntry;
use bdm_domain::{Amount, FeeEstimate, FeeSpeed, FeeTier};
use bdm_ports::{PortResult, ProviderError, SolanaRpc};
use serde_json::json;

/// Base fee per signature. <https://solana.com/docs/core/fees>
pub const BASE_FEE_LAMPORTS_PER_SIGNATURE: u64 = 5_000;
/// Jito minimum tip. <https://docs.jito.wtf/lowlatencytxnsend/>
pub const JITO_MIN_TIP_LAMPORTS: u64 = 1_000;
/// Helius Sender minimum tip (default dual-route mode, 0.001 SOL).
/// <https://www.helius.dev/docs/sending-transactions/sender>
pub const HELIUS_SENDER_MIN_TIP_LAMPORTS: u64 = 1_000_000;
/// Suggested tip: the Sender minimum, which also clears Jito's, so one signed tx is valid for
/// every relay in the default `private_relay` order (`jito`, `helius_sender`).
pub const SUGGESTED_TIP_LAMPORTS: u64 = HELIUS_SENDER_MIN_TIP_LAMPORTS;

/// Nearest-rank percentile of an ascending slice (`p` in 0..=100). Empty → 0.
pub fn percentile(sorted: &[u64], p: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p.min(100) as usize * sorted.len()).div_ceil(100);
    sorted[rank.saturating_sub(1)]
}

pub fn tier(speed: FeeSpeed, micro_lamports_per_cu: u64) -> FeeTier {
    FeeTier {
        speed,
        max_fee_per_gas: None,
        max_priority_fee_per_gas: None,
        compute_unit_price_micro_lamports: Some(micro_lamports_per_cu),
        estimated_total: None,
        estimated_total_fiat: None,
    }
}

/// Slow / Standard / Fast = p25 / p50 / p75 of the samples.
pub fn tiers_from_samples(mut samples: Vec<u64>) -> Vec<FeeTier> {
    samples.sort_unstable();
    [
        (FeeSpeed::Slow, 25),
        (FeeSpeed::Standard, 50),
        (FeeSpeed::Fast, 75),
    ]
    .into_iter()
    .map(|(s, p)| tier(s, percentile(&samples, p)))
    .collect()
}

/// Lamports for a tx: base fee per signature + `ceil(price × cu_limit / 1e6)` priority fee.
pub fn total_fee_lamports(signatures: u64, cu_limit: u64, micro_lamports_per_cu: u64) -> u64 {
    let priority = (micro_lamports_per_cu as u128 * cu_limit as u128).div_ceil(1_000_000);
    BASE_FEE_LAMPORTS_PER_SIGNATURE
        .saturating_mul(signatures)
        .saturating_add(u64::try_from(priority).unwrap_or(u64::MAX))
}

/// Suggested Jito/Sender tip as a 9-decimal SOL amount.
pub fn suggested_tip(chain: &ChainEntry) -> Amount {
    Amount::from_u128(SUGGESTED_TIP_LAMPORTS.into(), chain.native.decimals)
}

/// Fee estimate from `getRecentPrioritizationFees` over `accounts` (empty = global).
pub async fn fee_estimate(
    rpc: &dyn SolanaRpc,
    chain: &ChainEntry,
    accounts: &[String],
) -> PortResult<FeeEstimate> {
    let v = rpc
        .request("getRecentPrioritizationFees", json!([accounts]))
        .await?;
    let samples: Vec<u64> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["prioritizationFee"].as_u64())
        .collect();
    if samples.is_empty() {
        return Err(ProviderError::Transient(
            "getRecentPrioritizationFees returned no samples".into(),
        ));
    }
    Ok(FeeEstimate {
        chain: chain.id.clone(),
        tiers: tiers_from_samples(samples),
        l1_data_fee: None,
        tip: Some(suggested_tip(chain)),
        as_of: chrono::Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solana::testutil::{mainnet, FnRpc};
    use alloy_primitives::U256;

    #[test]
    fn nearest_rank() {
        let s: Vec<u64> = (1..=10).collect();
        assert_eq!(percentile(&s, 25), 3);
        assert_eq!(percentile(&s, 50), 5);
        assert_eq!(percentile(&s, 75), 8);
        assert_eq!(percentile(&s, 100), 10);
        assert_eq!(percentile(&s, 0), 1);
        assert_eq!(percentile(&[], 50), 0);
    }

    #[test]
    fn totals() {
        assert_eq!(total_fee_lamports(1, 200_000, 0), 5_000);
        assert_eq!(total_fee_lamports(2, 200_000, 50_000), 10_000 + 10_000);
        assert_eq!(total_fee_lamports(1, 1, 1), 5_001); // rounds up
    }

    #[tokio::test]
    async fn estimate_from_rpc_samples() {
        let rpc = FnRpc::new(|m, p| {
            assert_eq!(m, "getRecentPrioritizationFees");
            assert_eq!(p, &json!([[]]));
            Some(json!([
                {"slot": 1, "prioritizationFee": 0},
                {"slot": 2, "prioritizationFee": 0},
                {"slot": 3, "prioritizationFee": 1000},
                {"slot": 4, "prioritizationFee": 5000},
            ]))
        });
        let e = fee_estimate(&rpc, &mainnet(), &[]).await.unwrap();
        let prices: Vec<_> = e
            .tiers
            .iter()
            .map(|t| t.compute_unit_price_micro_lamports.unwrap())
            .collect();
        assert_eq!(prices, [0, 0, 1000]);
        assert_eq!(e.tip.unwrap().raw, U256::from(1_000_000u64));

        let empty = FnRpc::new(|_, _| Some(json!([])));
        assert!(fee_estimate(&empty, &mainnet(), &[]).await.is_err());
    }
}
