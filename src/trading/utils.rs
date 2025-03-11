//! Utility functions for trading operations

use log::{debug, warn};
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::Signature;
use anyhow::{Result, anyhow};
use std::time::Duration;
use std::str::FromStr;

/// Get a recent blockhash with specified commitment
pub async fn get_recent_blockhash(client: &RpcClient, commitment: CommitmentConfig) -> Result<[u8; 32]> {
    for attempt in 1..=3 {
        debug!("Getting recent blockhash (attempt {}/3) with {:?} commitment", attempt, commitment);
        
        match client.get_latest_blockhash_with_commitment(commitment) {
            Ok((blockhash, _)) => {
                debug!("Got recent blockhash: {}", blockhash);
                return Ok(blockhash.to_bytes());
            },
            Err(e) => {
                if attempt < 3 {
                    warn!("Failed to get recent blockhash (attempt {}/3): {}", attempt, e);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                } else {
                    return Err(anyhow!("Failed to get recent blockhash after 3 attempts: {}", e));
                }
            }
        }
    }
    
    // This should never be reached due to the return in the loop above
    Err(anyhow!("Failed to get recent blockhash after 3 attempts"))
}

/// Check transaction confirmation on the Solana blockchain
pub async fn confirm_transaction(client: &RpcClient, signature: &str) -> Result<bool> {
    // Try to confirm using the client with a confirmed commitment
    let confirmation_config = solana_sdk::commitment_config::CommitmentConfig::confirmed();
    
    match client.confirm_transaction_with_commitment(
        &solana_sdk::signature::Signature::from_str(signature)?,
        confirmation_config,
    ) {
        Ok(confirmed) => {
            Ok(confirmed.value)
        },
        Err(e) => {
            warn!("Failed to confirm transaction {}: {}", signature, e);
            Ok(false)
        }
    }
} 