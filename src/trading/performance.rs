//! Performance tracking for mint-to-buy transactions

use anyhow::{anyhow, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Signature};
use std::str::FromStr;
use solana_client::rpc_config::RpcTransactionConfig;
use solana_transaction_status::UiTransactionEncoding;

/// Asynchronously fetches the block information for a given transaction signature.
/// Returns a tuple: (slot, block_time) where block_time is in seconds.
pub async fn get_transaction_block_info(signature: &str) -> Result<(u64, i64)> {
    // Get the RPC URL from config. Assumes a function in config exists.
    let rpc_url = "https://api.mainnet-beta.solana.com".to_string();
    // For performance metrics, we use confirmed commitment for faster but still reliable results
    let client = RpcClient::new_with_commitment(
        rpc_url, 
        CommitmentConfig::confirmed()
    );

    let sig = Signature::from_str(signature)
        .map_err(|e| anyhow!("Invalid signature '{}': {}", signature, e))?;

    let tx = client.get_transaction_with_config(&sig, RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::Json),
        commitment: Some(CommitmentConfig::confirmed()),
        max_supported_transaction_version: Some(0),
    }).await?;

    // Ensure block time is available
    let block_time = tx.block_time
        .ok_or_else(|| anyhow!("Block time not available for transaction {}", signature))?;
    let slot = tx.slot;
    Ok((slot, block_time))
}

/// Computes performance metrics between a mint and buy transaction.
/// Returns a tuple: (time difference in milliseconds, block difference).
/// Expects that block_time is in seconds (as returned by Solana RPC).
pub async fn compute_performance_metrics(mint_sig: &str, buy_sig: &str) -> Result<(u64, i64)> {
    // Get mint transaction block info
    let (mint_slot, mint_time) = get_transaction_block_info(mint_sig).await?;
    
    // Get buy transaction block info
    let (buy_slot, buy_time) = get_transaction_block_info(buy_sig).await?;
    
    // Calculate time diff in milliseconds
    let time_diff_ms = (buy_time - mint_time) * 1000; // Convert seconds to milliseconds
    
    // Calculate block diff
    let block_diff = buy_slot.saturating_sub(mint_slot);
    
    Ok((block_diff, time_diff_ms))
} 