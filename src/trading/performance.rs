//! Performance tracking for mint-to-buy transactions

use anyhow::{anyhow, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Signature};
use std::str::FromStr;
use solana_client::rpc_config::RpcTransactionConfig;
use solana_transaction_status::UiTransactionEncoding;
use log::{warn, debug};

/// Asynchronously fetches the block information for a given transaction signature.
/// Returns a tuple: (slot, block_time) where block_time is in seconds.
/// Returns an error if the transaction failed or doesn't exist.
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

    // Configure transaction retrieval to get both slot and block time
    let config = RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::Base64),
        commitment: Some(CommitmentConfig::confirmed()),
        max_supported_transaction_version: Some(0),
    };

    // Implement retry logic with exponential backoff
    let mut retry_count = 0;
    let max_retries = 10;
    let mut backoff_ms = 500; // Start with 500ms

    loop {
        // Get transaction details with proper config
        let tx_result = client.get_transaction_with_config(&sig, config).await;
        
        match tx_result {
            Ok(tx) => {
                // Check if transaction has error status
                if let Some(meta) = &tx.transaction.meta {
                    if meta.err.is_some() {
                        return Err(anyhow!("Transaction {} failed: {:?}", signature, meta.err));
                    }
                }
                
                // Extract slot and block time
                let slot = tx.slot;
                let block_time = tx.block_time
                    .ok_or_else(|| anyhow!("Block time not available for transaction {}", signature))?;

                debug!("Successfully retrieved transaction {} at slot {} with block time {}", signature, slot, block_time);
                return Ok((slot, block_time));
            },
            Err(e) => {
                retry_count += 1;
                
                if retry_count >= max_retries {
                    warn!("Final attempt failed to retrieve transaction {}: {}", signature, e);
                    return Err(anyhow!("Failed to retrieve transaction after {} attempts: {}", max_retries, e));
                }
                
                // Log the retry attempt
                warn!("Attempt {}/{} - Failed to retrieve transaction {}: {}. Retrying in {}ms...", 
                      retry_count, max_retries, signature, e, backoff_ms);
                
                // Wait with exponential backoff
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                
                // Increase backoff for next retry (exponential with a max of 10 seconds)
                backoff_ms = std::cmp::min(backoff_ms * 2, 10000);
            }
        }
    }
}

/// Computes performance metrics between a mint and buy transaction.
/// Returns a tuple: (block difference, time difference in milliseconds).
/// Returns an error if either transaction failed or doesn't exist.
pub async fn compute_performance_metrics(mint_sig: &str, buy_sig: &str) -> Result<(u64, i64)> {
    debug!("Computing performance metrics between mint {} and buy {}", mint_sig, buy_sig);
    
    // Add an initial delay to allow the transaction to be processed
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    
    // Get mint transaction block info
    let mint_result = get_transaction_block_info(mint_sig).await;
    // Get buy transaction block info
    let buy_result = get_transaction_block_info(buy_sig).await;
    
    match (mint_result, buy_result) {
        (Ok((mint_slot, mint_time)), Ok((buy_slot, buy_time))) => {
            // Calculate time diff in milliseconds
            let time_diff_ms = (buy_time - mint_time) * 1000; // Convert seconds to milliseconds
            
            // Calculate block diff
            let block_diff = buy_slot.saturating_sub(mint_slot);
            
            Ok((block_diff, time_diff_ms))
        },
        (Err(e), _) => {
            warn!("Failed to get mint transaction info: {}", e);
            Err(anyhow!("Failed to get mint transaction info: {}", e))
        },
        (_, Err(e)) => {
            warn!("Failed to get buy transaction info: {}", e);
            Err(anyhow!("Failed to get buy transaction info: {}", e))
        }
    }
} 