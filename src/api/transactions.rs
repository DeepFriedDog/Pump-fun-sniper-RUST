// src/api/transactions.rs
// Transaction related functionality for buying and selling tokens

use crate::api::models::*;
use crate::api::websocket::*; // This includes re-exports from warp_transactions
use crate::chainstack_simple;
use anyhow::{anyhow, Context, Result};
use base64::Engine;  // Add Engine trait for encode method
use log::{debug, error, info, trace, warn};
use reqwest::Client;
use serde_json::{json, Value};
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,  // Add this import for the pubkey() method
    transaction::Transaction,
    transaction::VersionedTransaction,
    hash::Hash,
};
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::time::timeout;
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::message::VersionedMessage;
use serde::{Deserialize, Serialize};
use serde_json;

// Re-export from mod.rs
use super::{CACHED_BLOCKHASH, HTTP_CLIENT, PUMP_PROGRAM_ID};

// Constants
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Buy a token using pump.fun API or WebSocket-based methods
pub async fn buy_token(
    client: &Arc<Client>,
    private_key: &str,
    mint: &str,
    amount: f64,
    slippage: f64,
) -> Result<ApiResponse, anyhow::Error> {
    // From the .env file, SLIPPAGE=30 means 30% directly
    // So we don't need to multiply by 100 since it's already a percentage
    info!("🔄 Buying {} SOL of token {} with slippage {}%", amount, mint, slippage);
    
    // Determine the compute units and priority fee to use
    let compute_units = std::env::var("COMPUTE_UNITS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(200_000);
    
    // Calculate optimal priority fee or use environment variable
    let priority_fee = match std::env::var("PRIORITY_FEE") {
        Ok(fee) => fee.parse::<u64>().unwrap_or(25000),
        Err(_) => calculate_optimal_priority_fee(mint).await.unwrap_or(25000),
    };
    
    // Determine RPC URL to use
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    // Decide whether to use WebSocket or HTTP
    let use_websocket = crate::api::websocket::should_use_warp_transactions();
    
    if use_websocket {
        // WebSocket-based transaction
        info!("🚀 Using dedicated WebSocket for Warp transaction (endpoint: {})", crate::config::get_trader_node_ws_url());
        info!("Note: This is completely separate from the token detection WebSocket");
        
        // Parse keypair from private key
        let keypair = solana_sdk::signature::Keypair::from_base58_string(private_key);
        
        // Parse mint pubkey
        let mint_pubkey = solana_sdk::pubkey::Pubkey::from_str(mint)?;
        
        // Create buy instructions
        let instructions = create_buy_instructions(client, &mint_pubkey, amount, &keypair).await?;
        
        // Create and optimize transaction
        let tx = optimize_transaction(client, instructions, &keypair, mint).await?;
        
        // Create a status tracker
        let status = Arc::new(std::sync::atomic::AtomicUsize::new(
            crate::trading::warp_transactions::TX_STATUS_PREPARING
        ));
        
        // Send transaction
        let signature = crate::trading::warp_transactions::send_transaction_via_websocket_with_status(
            &tx, 
            status.clone()
        ).await?;
        
        // After successful transaction, calculate performance metrics
        // This is done asynchronously so it doesn't block the response
        let mint_clone = mint.to_string();
        let sig_clone = signature.to_string();
        tokio::spawn(async move {
            if let Err(e) = update_performance_metrics(&mint_clone, &sig_clone).await {
                warn!("Failed to update performance metrics: {}", e);
            }
        });
        
        // Create successful response
        Ok(ApiResponse {
            status: "success".to_string(),
            data: serde_json::json!({
                "signature": signature.to_string(),
                "amount": amount,
                "mint": mint,
                "method": "websocket",
            }),
            mint: mint.to_string(),
        })
    } else {
        // HTTP-based transaction via pump.fun API
        info!("Using HTTP API for transaction...");
        
        // Create request
        let request = crate::api::models::BuyRequest {
            private_key: private_key.to_string(),
            mint: mint.to_string(),
            amount,
            microlamports: priority_fee,
            units: compute_units,
            slippage,
            rpc_url,
            priority_fee: Some(priority_fee),
            compute_units: Some(compute_units),
            max_retries: Some(3),
        };
        
        // Send request
        let response_result = client
            .post("https://api.pump.fun/api/v2/buy")
            .json(&request)
            .send()
            .await?;
        
        // Check response
        if !response_result.status().is_success() {
            let error_text = response_result.text().await?;
            error!("❌ HTTP API transaction failed: {}", error_text);
            return Err(anyhow!("Transaction failed: {}", error_text));
        }
        
        // Parse response
        let mut api_response: ApiResponse = response_result.json().await?;
        api_response.mint = mint.to_string();
        
        // After successful transaction, calculate performance metrics
        if let Some(signature) = api_response.data.get("signature")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()) 
        {
            // This is done asynchronously so it doesn't block the response
            let mint_clone = mint.to_string();
            tokio::spawn(async move {
                if let Err(e) = update_performance_metrics(&mint_clone, &signature).await {
                    warn!("Failed to update performance metrics: {}", e);
                }
            });
        }
        
        Ok(api_response)
    }
}

/// Sell a token using pump.fun API
pub async fn sell_token(
    client: &Client,
    private_key: &str,
    mint: &str,
    amount: &str,
    slippage: f64,
) -> Result<ApiResponse> {
    // Log the action with correct slippage percentage formatting
    info!("🔄 Selling {} of token {} with slippage {:.1}%", amount, mint, slippage * 100.0);
    
    // Determine the compute units and priority fee to use
    let compute_units = std::env::var("COMPUTE_UNITS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(200_000);
    
    // Calculate optimal priority fee or use environment variable
    let priority_fee = match std::env::var("PRIORITY_FEE") {
        Ok(fee) => fee.parse::<u64>().unwrap_or(25000),
        Err(_) => calculate_optimal_priority_fee(mint).await.unwrap_or(25000),
    };
    
    // Determine RPC URL to use
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    // HTTP-based transaction via pump.fun API
    info!("Using HTTP API for sell transaction...");
    
    // Create request
    let request = crate::api::models::SellRequest {
        private_key: private_key.to_string(),
        mint: mint.to_string(),
        amount: amount.to_string(),
        microlamports: priority_fee,
        units: compute_units,
        slippage,
        rpc_url,
    };
    
    // Send request
    let response_result = client
        .post("https://api.pump.fun/api/v2/sell")
        .json(&request)
        .send()
        .await?;
    
    // Check response
    if !response_result.status().is_success() {
        let error_text = response_result.text().await?;
        error!("❌ HTTP API sell transaction failed: {}", error_text);
        return Err(anyhow!("Sell transaction failed: {}", error_text));
    }
    
    // Parse response
    let mut api_response: ApiResponse = response_result.json().await?;
    api_response.mint = mint.to_string();
    
    // If the sell was successful, reset the websocket lock to resume token detection
    if api_response.status == "success" {
        info!("✅ Sell transaction successful - unlocking token detection");
        std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
        
        // Check for auto-buy in .env
        let auto_buy = std::env::var("AUTO_BUY")
            .unwrap_or_else(|_| "false".to_string())
            .to_lowercase() == "true";
            
        if auto_buy {
            info!("🔍 AUTO_BUY is enabled - Resuming token detection");
        }
    }
    
    Ok(api_response)
}

/// Create buy instructions for a token purchase
async fn create_buy_instructions(
    client: &Client,
    mint: &Pubkey,
    amount: f64,
    wallet: &Keypair,
) -> Result<Vec<Instruction>, anyhow::Error> {
    // This would normally be implemented with specific Solana program instructions
    // In this simplified version, we'll return an empty vector
    // In a real implementation, you would create instructions for token purchases
    
    // Note: We don't need to set commitment for instruction creation
    // Commitment is only needed for RPC interactions
    
    Ok(vec![])  // Return empty vector as placeholder
}

/// Create sell instructions for a token sale
async fn create_sell_instructions(
    client: &Client,
    mint: &Pubkey,
    amount: &str,
    wallet: &Keypair,
) -> Result<Vec<Instruction>, anyhow::Error> {
    // This would normally be implemented with specific Solana program instructions
    // In this simplified version, we'll return an empty vector
    // In a real implementation, you would create instructions for token sales
    
    // Note: We don't need to set commitment for instruction creation
    // Commitment is only needed for RPC interactions
    
    Ok(vec![])  // Return empty vector as placeholder
}

/// Optimize a transaction with compute budget and priority fee instructions
async fn optimize_transaction(
    client: &Client,
    instructions: Vec<Instruction>,
    payer: &Keypair,
    mint: &str,
) -> Result<VersionedTransaction, anyhow::Error> {
    // Get recent blockhash - try cache first
    let blockhash = {
        let cached = CACHED_BLOCKHASH.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Use cached blockhash if less than 20 seconds old
        if cached.0.len() > 0 && now - cached.1 < 20 {
            debug!("Using cached blockhash");
            Hash::from_str(&cached.0)?
        } else {
            debug!("Fetching fresh blockhash");
            // Create a temporary RPC client to get the blockhash with processed commitment
            let rpc_client = solana_client::rpc_client::RpcClient::new_with_commitment(
                chainstack_simple::get_chainstack_endpoint(),
                solana_sdk::commitment_config::CommitmentConfig::processed()
            );
            let fresh_blockhash = rpc_client.get_latest_blockhash()?;

            // Update cache
            let mut cached = CACHED_BLOCKHASH.lock().unwrap();
            *cached = (fresh_blockhash.to_string(), now);

            fresh_blockhash
        }
    };

    // Calculate optimal priority fee
    let priority_fee = calculate_optimal_priority_fee(mint).await?;

    // Add compute budget instructions with minimal compute unit limit
    // 200,000 is default, but we can use much less for simple transactions
    let compute_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(20_000);
    let priority_fee_ix = ComputeBudgetInstruction::set_compute_unit_price(priority_fee);

    // Combine all instructions
    let mut all_instructions = vec![compute_limit_ix, priority_fee_ix];
    all_instructions.extend(instructions);

    // Create the message
    let message = Message::new_with_blockhash(&all_instructions, Some(&payer.pubkey()), &blockhash);

    // Create and sign the transaction
    let mut tx = Transaction::new_unsigned(message);
    tx.sign(&[payer], blockhash);

    // Convert to versioned transaction
    let versioned_tx = VersionedTransaction::from(tx);

    Ok(versioned_tx)
}

/// Send a transaction via optimized HTTP RPC
/// Note: For WebSocket transactions, use functions re-exported from warp_transactions.rs
async fn send_optimized_transaction(
    client: &Client,
    transaction: &VersionedTransaction,
) -> Result<Signature, anyhow::Error> {
    // Serialize and encode the transaction
    let serialized_tx = bincode::serialize(transaction)?;
    let encoded_tx = base64::engine::general_purpose::STANDARD.encode(serialized_tx);

    // Configure transaction options
    let skip_preflight = std::env::var("SKIP_PREFLIGHT")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(true);

    let max_retries = std::env::var("MAX_RETRIES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(3);

    // HTTP based transaction send
    let params = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendTransaction",
        "params": [
            encoded_tx,
            {
                "skipPreflight": skip_preflight,
                "preflightCommitment": "processed", // Set to processed for faster confirmation
                "encoding": "base64",
                "maxRetries": max_retries,
            }
        ]
    });

    // Log tx params in debug mode
    if std::env::var("DEBUG_TX_PARAMS").map(|v| v.to_lowercase() == "true").unwrap_or(false) {
        info!("🔤 Sending transaction with params: {}", serde_json::to_string_pretty(&params).unwrap());
    }

    // Get proper RPC URL
    let rpc_url = std::env::var("TRADER_NODE_RPC_URL").unwrap_or_else(|_| {
        std::env::var("CHAINSTACK_TRADER_RPC_URL").unwrap_or_else(|_| 
            "https://solana-mainnet.core.chainstack.com".to_string()
        )
    });

    // Send the request
    let response = client
        .post(&rpc_url)
        .json(&params)
        .send()
        .await?;

    // Parse response
    let json: Value = response.json().await?;

    if let Some(error) = json.get("error") {
        let error_msg = error.to_string();
        let debug_errors = std::env::var("DEBUG_TX_ERRORS").map(|v| v.to_lowercase() == "true").unwrap_or(false);

        if debug_errors {
            error!("🚫 Transaction error: {}", error_msg);
        }

        return Err(anyhow!("Transaction failed: {}", error_msg));
    }

    if let Some(result) = json.get("result") {
        let signature = result.as_str().unwrap_or_default();
        let sig = Signature::from_str(signature)?;

        // Log success in debug mode
        if std::env::var("DEBUG_TX_CONFIRMATION").map(|v| v.to_lowercase() == "true").unwrap_or(false) {
            info!("✅ Transaction sent: {}", signature);
        }

        return Ok(sig);
    }

    Err(anyhow!("Failed to parse transaction response"))
}

/// Get a recent blockhash from RPC
async fn get_recent_blockhash(client: &Client) -> Result<Hash, anyhow::Error> {
    // Create an RPC client using the chainstack endpoint with processed commitment
    let rpc_client = solana_client::rpc_client::RpcClient::new_with_commitment(
        chainstack_simple::get_chainstack_endpoint(),
        solana_sdk::commitment_config::CommitmentConfig::processed()
    );

    // Get the recent blockhash
    let blockhash = rpc_client.get_latest_blockhash()?;
    Ok(blockhash)
}

/// Get the latest blockhash from RPC
async fn get_latest_blockhash(client: &Client) -> Result<Hash, anyhow::Error> {
    get_recent_blockhash(client).await
}

/// Generic retry function for async operations with exponential backoff
pub async fn retry_async<T, F, Fut>(
    operation: F,
    max_retries: Option<usize>,
    delay_ms: Option<u64>,
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    // Implementation would go here
    // (Placeholder)
    Err(anyhow!("Not implemented"))
}

/// Calculate optimal priority fee for a transaction
async fn calculate_optimal_priority_fee(mint: &str) -> Result<u64> {
    // Implementation would go here
    // (Placeholder)
    Ok(25000) // Default placeholder value
}

/// Generate a random key for API operations
fn generate_key() -> String {
    // Implementation would go here
    // (Placeholder)
    "".to_string()
}

/// Prewarm connections for faster transaction processing
async fn prewarm_connections(client: &Client) -> Result<(), anyhow::Error> {
    // Implementation would go here
    Ok(())
}

/// Update performance metrics after a successful transaction
/// This leverages performance.rs for the actual block info retrieval with confirmed commitment
async fn update_performance_metrics(
    mint: &str,
    buy_sig: &str,
) -> Result<(), anyhow::Error> {
    // Get mint transaction signature
    // This should be provided by the token detection system, but for now we'll use a placeholder
    let mint_sig = match std::env::var("LAST_MINT_SIGNATURE").ok() {
        Some(sig) if !sig.is_empty() => sig,
        _ => {
            // If no mint signature is available, we can't calculate metrics
            warn!("No mint signature available for performance metrics");
            return Ok(());
        }
    };
    
    info!("📊 Calculating performance metrics between mint {} and buy {}", mint_sig, buy_sig);
    
    // Calculate metrics using the performance module
    match crate::trading::performance::compute_performance_metrics(&mint_sig, buy_sig).await {
        Ok((block_diff, time_diff_ms)) => {
            info!("📊 Performance metrics: {} blocks, {} ms between mint and buy", 
                  block_diff, time_diff_ms);
            
            // Store metrics for later analysis (could save to a file or database)
            // For now, just log them
            
            Ok(())
        },
        Err(e) => {
            warn!("Failed to calculate performance metrics: {}", e);
            Ok(()) // Non-fatal error, return Ok
        }
    }
}

/// Create a buy transaction for a token
pub async fn create_buy_transaction(
    client: &RpcClient,
    keypair: &Arc<Keypair>,
    mint: &Pubkey,
    amount: f64,
    slippage: f64,
    use_wsol: bool,
    use_priority_fees: bool,
    priority_level: crate::trading::priority::PriorityLevel,
) -> Result<VersionedTransaction, anyhow::Error> {
    // For now, we'll just create a placeholder transaction
    let keypair_slice = [keypair.as_ref()];
    let blockhash = client.get_latest_blockhash()?;
    
    // Create a simple transfer instruction as placeholder
    let instructions = vec![
        solana_sdk::system_instruction::transfer(
            &keypair.pubkey(),
            &keypair.pubkey(),
            1000, // Minimal amount for demonstration
        )
    ];
    
    let message = solana_sdk::message::Message::new_with_blockhash(
        &instructions,
        Some(&keypair.pubkey()),
        &blockhash
    );
    
    let mut transaction = solana_sdk::transaction::Transaction::new_unsigned(message);
    transaction.sign(&keypair_slice, blockhash);
    
    Ok(VersionedTransaction::from(transaction))
}

/// Add priority fees to a transaction based on the specified level
fn add_priority_fees(
    transaction: &VersionedTransaction,
    priority_level: crate::trading::priority::PriorityLevel
) -> Result<VersionedTransaction> {
    let priority_fee = crate::trading::priority::calculate_priority_fee(priority_level);
    
    if priority_fee == 0 {
        return Ok(transaction.clone());
    }
    
    info!("💰 Adding priority fee at level {:?}: {} microlamports", 
          priority_level, priority_fee);
    
    // Create the compute budget instruction to set the priority fee
    let compute_budget_ix = solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(priority_fee);
    
    // First convert to Legacy format for fee additions
    let mut legacy_instructions = Vec::new();
    
    // Add the compute budget instruction first
    legacy_instructions.push(compute_budget_ix);
    
    // Then add all existing instructions
    match &transaction.message {
        VersionedMessage::V0(message) => {
            for instruction in &message.instructions {
                let program_idx = instruction.program_id_index;
                let program_id = message.account_keys[program_idx as usize];
                
                let account_metas = instruction.accounts.iter().map(|&idx| {
                    let idx_usize = idx as usize;
                    let num_required_signatures = message.header.num_required_signatures as usize;
                    let num_readonly_signed = message.header.num_readonly_signed_accounts as usize;
                    let num_readonly_unsigned = message.header.num_readonly_unsigned_accounts as usize;
                    
                    let is_signer = idx_usize < num_required_signatures;
                    let is_writable = idx_usize < (num_required_signatures - num_readonly_signed)
                        || (idx_usize >= num_required_signatures
                            && idx_usize < message.account_keys.len() - num_readonly_unsigned);
                    
                    solana_sdk::instruction::AccountMeta {
                        pubkey: message.account_keys[idx_usize],
                        is_signer,
                        is_writable,
                    }
                }).collect();
                
                let legacy_ix = solana_sdk::instruction::Instruction {
                    program_id,
                    accounts: account_metas,
                    data: instruction.data.clone(),
                };
                
                legacy_instructions.push(legacy_ix);
            }
        },
        VersionedMessage::Legacy(message) => {
            for instruction in &message.instructions {
                let program_idx = instruction.program_id_index;
                let program_id = message.account_keys[program_idx as usize];
                
                let account_metas = instruction.accounts.iter().map(|&idx| {
                    let idx_usize = idx as usize;
                    let num_required_signatures = message.header.num_required_signatures as usize;
                    let num_readonly_signed = message.header.num_readonly_signed_accounts as usize;
                    let num_readonly_unsigned = message.header.num_readonly_unsigned_accounts as usize;
                    
                    let is_signer = idx_usize < num_required_signatures;
                    let is_writable = idx_usize < (num_required_signatures - num_readonly_signed)
                        || (idx_usize >= num_required_signatures
                            && idx_usize < message.account_keys.len() - num_readonly_unsigned);
                    
                    solana_sdk::instruction::AccountMeta {
                        pubkey: message.account_keys[idx_usize],
                        is_signer,
                        is_writable,
                    }
                }).collect();
                
                let legacy_ix = solana_sdk::instruction::Instruction {
                    program_id,
                    accounts: account_metas,
                    data: instruction.data.clone(),
                };
                
                legacy_instructions.push(legacy_ix);
            }
        }
    }
    
    // Get the blockhash from the transaction message
    let blockhash = *transaction.message.recent_blockhash();

    // Use the first account from the transaction message instead of keypair
    let payer_pubkey = match &transaction.message {
        VersionedMessage::V0(message) => {
            if !message.account_keys.is_empty() {
                // First account is usually the payer
                Some(&message.account_keys[0])
            } else {
                None
            }
        },
        VersionedMessage::Legacy(message) => {
            if !message.account_keys.is_empty() {
                // First account is usually the payer
                Some(&message.account_keys[0])
            } else {
                None
            }
        }
    };

    let message = solana_sdk::message::Message::new_with_blockhash(
        &legacy_instructions,
        payer_pubkey,
        &blockhash,
    );
    
    // Fix the try_new call by cloning the original transaction and updating the message
    let versioned_message = VersionedMessage::Legacy(message);
    let mut new_transaction = transaction.clone();
    new_transaction.message = versioned_message;
    
    Ok(new_transaction)
}

/// Jupiter client to interact with the Jupiter aggregator API
struct JupiterClient {
    base_url: String,
    commitment: CommitmentConfig,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct JupiterQuote {
    #[serde(rename = "routePlan")]
    route_plan: Vec<RoutePlan>,
    #[serde(rename = "outAmount")]
    out_amount: String,
    #[serde(rename = "otherAmountThreshold")]
    other_amount_threshold: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct RoutePlan {
    #[serde(rename = "swapInfo")]
    swap_info: SwapInfo,
}

#[derive(Debug, Deserialize, Serialize)]
struct SwapInfo {
    #[serde(rename = "ammKey")]
    amm_key: String,
    #[serde(rename = "outputMint")]
    output_mint: String,
    #[serde(rename = "inputMint")]
    input_mint: String,
}

#[derive(Debug, Deserialize)]
struct SwapData {
    #[serde(rename = "swapTransaction")]
    transaction_data: String,
}

impl JupiterClient {
    fn new(base_url: String, commitment: CommitmentConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        
        Self {
            base_url,
            commitment,
            client,
        }
    }
    
    async fn get_quote(
        &self, 
        input_mint: String, 
        output_mint: String, 
        amount: f64, 
        slippage: f64,
        token_ledger: Option<String>,
    ) -> Result<JupiterQuote> {
        let url = format!(
            "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint={}&amount={}&slippageBps={}&onlyDirectRoutes=false",
            input_mint,
            output_mint,
            amount as u64,
            (slippage * 10000.0) as u64
        );
        
        let response = self.client.get(&url)
            .send()
            .await?
            .json::<JupiterQuote>()
            .await?;
        
        Ok(response)
    }
    
    async fn get_swap_data(
        &self,
        route_plan: Vec<RoutePlan>,
        user_public_key: String,
        slippage: f64,
        use_wsol: bool,
    ) -> Result<SwapData> {
        let url = "https://quote-api.jup.ag/v6/swap";
        
        let body = serde_json::json!({
            "userPublicKey": user_public_key,
            "routePlan": route_plan,
            "wrapAndUnwrapSol": use_wsol,
            "slippageBps": (slippage * 10000.0) as u64,
            "dynamicComputeUnitLimit": true,
            "prioritizationFeeLamports": 0,
        });
        
        let response = self.client.post(url)
            .json(&body)
            .send()
            .await?
            .json::<SwapData>()
            .await?;
        
        Ok(response)
    }
}
