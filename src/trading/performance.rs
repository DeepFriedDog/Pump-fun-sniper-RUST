//! Performance tracking for mint-to-buy transactions

use anyhow::{anyhow, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Signature};
use std::str::FromStr;
use solana_client::rpc_config::RpcTransactionConfig;
use solana_transaction_status::UiTransactionEncoding;
use log::{warn, debug, info};

/// Asynchronously fetches the block information for a given transaction signature.
/// Returns a tuple: (slot, block_time) where block_time is in seconds.
/// Returns an error if the transaction failed or doesn't exist.
pub async fn get_transaction_block_info(signature: &str) -> Result<(u64, i64)> {
    // Get the RPC URL from config, defaulting to mainnet
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    // IMPORTANT: Always use confirmed or finalized commitment since many RPC endpoints
    // don't support processed for transaction lookup
    let commitment_str = std::env::var("COMMITMENT_LEVEL")
        .unwrap_or_else(|_| "confirmed".to_string());
    
    let commitment = match commitment_str.to_lowercase().as_str() {
        "processed" => CommitmentConfig::confirmed(), // Override to confirmed even if processed is requested
        "confirmed" => CommitmentConfig::confirmed(),
        "finalized" => CommitmentConfig::finalized(),
        _ => CommitmentConfig::confirmed(), // Default to confirmed (not processed) for compatibility
    };
    
    info!("Using {} commitment for performance metrics", "confirmed");
    
    let client = RpcClient::new_with_commitment(rpc_url, commitment);

    let sig = Signature::from_str(signature)
        .map_err(|e| anyhow!("Invalid signature '{}': {}", signature, e))?;

    // Configure transaction retrieval to get both slot and block time
    let config = RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::Base64),
        commitment: Some(commitment),
        max_supported_transaction_version: Some(0),
    };

    // Implement retry logic with exponential backoff
    let mut retry_count = 0;
    let max_retries = 15; // Increased from 10 to 15
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
                
                // Increase backoff for next retry (exponential with a max of 15 seconds)
                backoff_ms = std::cmp::min(backoff_ms * 2, 15000); // Increased from 10s to 15s
            }
        }
    }
}

/// Compute performance metrics between two transactions (mint and buy)
pub async fn compute_performance_metrics(
    mint_signature: &str,
    buy_signature: &str,
) -> Result<(u64, i64), String> {
    // Log that we're starting calculation
    info!("Using confirmed commitment for performance metrics");
    
    // Create a client for RPC calls with a commitment level that works for both transactions
    let client = reqwest::Client::new();
    
    // Get mint transaction block info
    let mint_result = get_transaction_block_info(mint_signature).await;
    // Get buy transaction block info
    let buy_result = get_transaction_block_info(buy_signature).await;
    
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
            Err(e.to_string())
        },
        (_, Err(e)) => {
            warn!("Failed to get buy transaction info: {}", e);
            Err(e.to_string())
        }
    }
} 