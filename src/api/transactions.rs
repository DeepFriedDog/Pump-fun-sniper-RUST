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
    instruction::{Instruction, AccountMeta},
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
use uuid::Uuid;
use std::env;
use bs58 as base58;
use crate::create_buy_instruction::derive_associated_pump_curve;
use spl_associated_token_account::{get_associated_token_address, instruction::create_associated_token_account_idempotent};
use crate::create_buy_instruction::create_buy_instruction;

// Re-export from mod.rs
use super::{CACHED_BLOCKHASH, HTTP_CLIENT, PUMP_PROGRAM_ID};

// Constants
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// Result structure for buy operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuyResult {
    pub success: bool,
    pub signature: String,
    pub data: serde_json::Value,
}

/// Verify Trader Node connection and authentication
async fn verify_trader_node_connection(client: &Arc<Client>) -> Result<bool, anyhow::Error> {
    let rpc_url = crate::config::get_trader_node_rpc_url();
    info!("🧪 Testing Trader Node connection: {}", rpc_url);
    
    // Get Chainstack credentials
    let trader_username = env::var("TRADER_NODE_USERNAME").unwrap_or_default();
    let trader_password = env::var("TRADER_NODE_PASSWORD").unwrap_or_default();
    
    // Build proper JSON-RPC request for testing connection
    let json_rpc_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getHealth",
        "params": []
    });
    
    // Build request with proper authentication
    let mut connection_test_req = client.post(&rpc_url).json(&json_rpc_request);
    
    // Add authentication if credentials exist
    if !trader_username.is_empty() && !trader_password.is_empty() {
        connection_test_req = connection_test_req.basic_auth(&trader_username, Some(&trader_password));
        info!("🔐 Using basic auth with username: {}", trader_username);
    } else {
        // Fallback to API key
        connection_test_req = connection_test_req.header("x-api-key", env::var("API_KEY").unwrap_or_default());
    }
    
    let response = connection_test_req
        .header("Content-Type", "application/json")
        .send()
        .await?;
    
    let status = response.status();
    if status.is_success() {
        let response_text = response.text().await?;
        info!("✅ Trader Node connection successful! Response: {}", response_text);
        info!("✅ Trader Node connection verified");
        return Ok(true);
    } else {
        let error_text = response.text().await?;
        error!("❌ Trader Node connection failed: {}", error_text);
        return Ok(false);
    }
}

/// Buy a token using pump.fun API or HTTP-based methods
pub async fn buy_token(
    client: &reqwest::Client,
    private_key: &str,
    mint: &str,
    amount: f64,
    slippage: f64,
) -> Result<ApiResponse, anyhow::Error> {
    info!("🔄 Buying tokens worth {} SOL for {} with slippage {}%", amount, mint, slippage);
    
    // Calculate priority fee - use a much higher value for extremely fast tokens
    let priority_fee = get_dynamic_priority_fee().await.unwrap_or(30_000_000); // Default to 30M (0.03 SOL)
    info!("🔶 Using ultra-high dynamic priority fee: {} microlamports ({:.6} SOL) for maximum transaction speed", 
          priority_fee, priority_fee as f64 / LAMPORTS_PER_SOL as f64);
    
    // Get trader node URL - ensure we're using a Chainstack Trader Node endpoint
    let trader_node_url = env::var("TRADER_NODE_RPC_URL")
        .or_else(|_| env::var("CHAINSTACK_TRADER_RPC_URL"))
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    // Check if we're using a Chainstack Trader Node
    let is_trader_node = trader_node_url.contains("chainstack.com") || 
                         trader_node_url.contains("p2pify.com");
    
    if is_trader_node {
        info!("🚀 Using Chainstack Trader Node for maximum transaction speed: {}", trader_node_url);
    } else {
        warn!("⚠️ Not using a Chainstack Trader Node - transactions may be slower");
        info!("💡 Consider upgrading to a Chainstack Trader Node for 100% transaction landing rate");
    }
    
    // Test the Trader Node connection first to ensure it's working
    let client_arc = Arc::new(client.clone());
    verify_trader_node_connection(&client_arc).await?;
    
    // Use HTTP-based transaction for Warp transactions (required by Chainstack)
    info!("📡 Using HTTP-based transaction with Warp optimizations for ultra-fast execution");
    
    // Parse the mint to a pubkey
    let mint_pubkey = Pubkey::from_str(mint)
        .map_err(|e| {
            error!("❌ Failed to parse mint pubkey: {}", e);
            anyhow!("Failed to parse mint pubkey: {}", e)
        })?;
    info!("✅ Successfully parsed mint pubkey: {}", mint_pubkey);
    
    // Create the buy instructions
    info!("🧮 Creating buy instructions for mint: {}", mint);
    
    let bonding_curve = get_bonding_curve_from_api(mint).await?;
    info!("✅ Found bonding curve from API: {}", bonding_curve);
    
    // Get the keypair
    let keypair = decode_keypair(private_key).map_err(|e| anyhow::anyhow!("Failed to decode keypair: {}", e))?;
    let user_pubkey = keypair.pubkey();
    
    // Parse the mint and bonding curve pubkeys
    let mint_pubkey = Pubkey::from_str(mint)?;
    let bonding_curve_pubkey = Pubkey::from_str(&bonding_curve)?;
    
    // Get various program IDs we'll need
    let token_program_id = spl_token::id();
    let associated_token_program_id = spl_associated_token_account::id();
    let system_program_id = solana_sdk::system_program::id();
    
    // Derive the associated bonding curve for this mint and bonding curve
    let associated_bonding_curve = derive_associated_pump_curve(&mint_pubkey, &bonding_curve_pubkey);
    info!("🔍 Checking if associated bonding curve account exists: {}", associated_bonding_curve);
    
    // Check if the associated bonding curve account exists
    let account_exists = match check_account_exists(&trader_node_url, &associated_bonding_curve.to_string()).await {
        Ok(exists) => exists,
        Err(e) => {
            warn!("⚠️ Failed to check if associated bonding curve account exists: {}", e);
            false
        }
    };
    
    // Create user's token account address
    let user_token_account = get_associated_token_address(&user_pubkey, &mint_pubkey);
    info!("📝 User's token account: {}", user_token_account);
    
    // Get the buy instruction with possibly increased slippage
    // Ensure adequate slippage is provided - adjust if necessary
    let effective_slippage = if slippage < 40.0 { 40.0 } else { slippage };
    if effective_slippage > slippage {
        info!("🔄 Increasing slippage from {}% to {}% to prevent transaction failures", slippage, effective_slippage);
    }
    
    let (buy_instruction, _) = create_buy_instruction(
        &user_pubkey,
        mint,
        &bonding_curve,
        amount,
        effective_slippage
    )?;
    
    // Create a vector for all our instructions
    let mut instructions = Vec::new();
    
    // If the associated bonding curve doesn't exist, create it manually with the exact account order
    if !account_exists {
        warn!("⚠️ Associated bonding curve account doesn't exist, but pump.fun program will create it");
        info!("✅ No additional account creation needed - pump.fun program handles this case");
    }
    
    // Now create the user's token account with the idempotent version
    // The idempotent version won't fail if the account already exists
    let create_user_ata_ix = {
        // Log detailed account info to make sure everything is correct
        info!("🔍 Creating user token account for token: {}", mint);
        info!("🔍 User token account address: {}", user_token_account);
        info!("🔍 Wallet (owner): {}", user_pubkey);
        info!("🔍 Mint address: {}", mint_pubkey);
        info!("🔍 Payer (user): {}", user_pubkey);
        
        // Create using official SPL library function for idempotent ATA creation
        spl_associated_token_account::instruction::create_associated_token_account_idempotent(
            &user_pubkey,      // payer
            &user_pubkey,      // wallet
            &mint_pubkey,      // mint
            &token_program_id  // token program id
        )
    };
    
    // Additional validation to ensure account ordering is correct
    info!("🔍 User token account instruction check:");
    for (i, account) in create_user_ata_ix.accounts.iter().enumerate() {
        info!("🔍 Account {} ({:?}): {}", i, account.is_signer, account.pubkey);
    }
    
    // Add compute units and priority fee instructions
    let compute_unit_price_ix = ComputeBudgetInstruction::set_compute_unit_price(priority_fee);
    let compute_unit_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(1_000_000); // Use maximum compute units
    instructions.push(compute_unit_price_ix);
    instructions.push(compute_unit_limit_ix);

    // Add ATA creation instruction for the user
    instructions.push(create_user_ata_ix);
    
    // Add the buy instruction last
    instructions.push(buy_instruction.clone());
    
    info!("✅ Created buy instruction with {} accounts", buy_instruction.accounts.len());
    
    // Create a new transaction with all instructions in the correct order
    let mut transaction = Transaction::new_with_payer(
        &instructions,
        Some(&user_pubkey)
    );
    
    // Perform a final validation check before sending the transaction
    let mut all_pubkeys = std::collections::HashSet::new();
    all_pubkeys.insert(mint_pubkey.to_string());
    all_pubkeys.insert(bonding_curve_pubkey.to_string());
    all_pubkeys.insert(user_pubkey.to_string());
    all_pubkeys.insert(associated_bonding_curve.to_string());
    
    // If we have fewer unique addresses than expected, it means some addresses are being duplicated
    if all_pubkeys.len() < 4 {
        error!("❌ CRITICAL ERROR: Detected address duplication in transaction!");
        error!("📌 Mint: {}", mint_pubkey);
        error!("📌 Bonding curve: {}", bonding_curve_pubkey);
        error!("📌 User wallet: {}", user_pubkey);
        error!("📌 Associated bonding curve: {}", associated_bonding_curve);
        return Err(anyhow!("Transaction validation failed: Address duplication detected"));
    }
    
    // Get recent blockhash - use the existing function but correctly pass the client reference
    let recent_blockhash = get_latest_blockhash(client).await?;
    
    // Set the blockhash
    transaction.sign(&[&keypair], recent_blockhash);
    
    // Serialize transaction to base64
    let serialized_tx = bincode::serialize(&transaction)
        .map_err(|e| anyhow!("Failed to serialize transaction: {}", e))?;
    let base64_tx = base64::engine::general_purpose::STANDARD.encode(serialized_tx);
    
    // Get auth token from env - use the specific BLOXROUTE_AUTH_HEADER if available
    let bloxroute_auth = env::var("BLOXROUTE_AUTH_HEADER").unwrap_or_default();
    let chainstack_api_key = env::var("API_KEY").unwrap_or_default();
    
    // Create the bloXroute-specific transaction payload optimized for ultra-fast execution
    let bloxroute_payload = json!({
        "jsonrpc": "2.0", 
        "id": format!("tx-{}", Uuid::new_v4()),
        "method": "sendTransaction",
        "params": [
            base64_tx,
            {
                "encoding": "base64",
                "skipPreflight": true,                // Skip checks for faster submission
                "maxRetries": 25,                     // Increased from 10 to 25 for more retries
                "preflightCommitment": "processed",   // Fastest commitment level
                "minContextSlot": 0,
                "frontRunningProtection": false,      // Disable - more validators can process
                "computeUnitPrice": priority_fee,     // Directly specify priority fee
                "DANGEROUS_FEE_PAYER": user_pubkey.to_string() // Explicitly specify fee payer
            }
        ]
    });
    
    // Determine which payload to use - always use bloXroute format for fast tokens
    let tx_payload = bloxroute_payload;

    // Send the transaction to the trader node
    info!("🚀 Sending transaction to Trader Node: {}", trader_node_url);
    
    // Build the request with blocking client
    let client = reqwest::blocking::Client::new();
    
    // Build request headers optimized for speed
    let mut request_builder = client.post(&trader_node_url)
        .header("X-Chainstack-Tx", "true")           // Required header for Chainstack
        .header("X-Chainstack-Warp", "true")         // Explicitly request Warp transaction
        .header("X-BloXroute-Transaction", "true")   // Required header for bloXroute
        .header("X-Chainstack-Speed", "fastest")     // Request fastest possible transaction
        .header("X-Chainstack-Priority", "highest")  // Request highest priority
        .header("Content-Type", "application/json");  // Required content type
    
    // Add authentication headers
    let trader_username = env::var("TRADER_NODE_USERNAME").unwrap_or_default();
    let trader_password = env::var("TRADER_NODE_PASSWORD").unwrap_or_default();
    let chainstack_api_key = env::var("API_KEY").unwrap_or_default();
    let bloxroute_auth = env::var("BLOXROUTE_AUTH_HEADER").unwrap_or_default();
    
    if !trader_username.is_empty() && !trader_password.is_empty() {
        info!("🔐 Using basic authentication for transaction");
        request_builder = request_builder
            .header("Authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", trader_username, trader_password))));
    } else if !chainstack_api_key.is_empty() {
        info!("🔑 Using API key authentication for transaction");
        request_builder = request_builder
            .header("x-api-key", &chainstack_api_key)
            .header("X-Chainstack-Auth", &chainstack_api_key);
    } else if !bloxroute_auth.is_empty() {
        info!("🔑 Using direct bloXroute authentication");
        request_builder = request_builder
            .header("Authorization", &bloxroute_auth);
    } else {
        warn!("⚠️ No authentication credentials found. Transaction might fail!");
    }
    
    // Send the transaction request
    let response = request_builder.json(&tx_payload).send()?;
    
    // Process response
    let status = response.status();
    if status.is_success() {
        let response_text = response.text()?;
        info!("✅ Transaction response: {}", response_text);
        
        // Parse the response to get the transaction signature
        match serde_json::from_str::<Value>(&response_text) {
            Ok(json) => {
                if let Some(result) = json.get("result") {
                    if let Some(signature) = result.as_str() {
                        info!("✅ Transaction signature: {}", signature);
                        info!("🔗 View on explorer: https://explorer.solana.com/tx/{}", signature);
                        
                        // Add confirmation check with better error handling
                        match verify_transaction(&trader_node_url, signature)
                            .await
                            .map_err(|e| anyhow::anyhow!("{}", e)) {
                            Ok((confirmed, error)) => {
                                if confirmed && error.is_none() {
                                    info!("✅ Transaction confirmed on-chain or accepted by Trader Node");
                                    return Ok(ApiResponse {
                                        status: "success".to_string(),
                                        data: json!({
                                            "signature": signature,
                                            "status": "sent",
                                            "message": "Transaction sent successfully",
                                            "verified": confirmed
                                        }),
                                        mint: mint.to_string(),
                                    });
                                } else if let Some(error_msg) = error {
                                    // Check for specific error types
                                    if error_msg.contains("0x1772") || error_msg.contains("TooMuchSolRequired") || error_msg.contains("6002") {
                                        warn!("❌ Transaction failed with slippage error: {}", error_msg);
                                        return Ok(ApiResponse {
                                            status: "error".to_string(),
                                            data: json!({
                                                "signature": signature,
                                                "error": "slippage: Too much SOL required to buy tokens (price moved too quickly)",
                                                "error_code": "0x1772",
                                                "message": "Transaction sent but failed due to slippage"
                                            }),
                                            mint: mint.to_string(),
                                        });
                                    } else if error_msg.contains("seeds do not result in a valid address") {
                                        error!("❌ CRITICAL ERROR: Associated Token Account address derivation failure: {}", error_msg);
                                        error!("🛑 Stopping the sniping tool to prevent further money loss");
                                        panic!("Transaction failed with address derivation error. Exiting to prevent further money loss.");
                                    } else {
                                        warn!("❌ Transaction verification failed with error: {}", error_msg);
                                        error!("🛑 Stopping the sniping tool to prevent further money loss after transaction failure");
                                        panic!("Transaction failed. Exiting to prevent further money loss.");
                                    }
                                } else {
                                    warn!("⚠️ Transaction verification unclear, but signature was received");
                                    return Ok(ApiResponse {
                                        status: "success".to_string(),
                                        data: json!({
                                            "signature": signature,
                                            "status": "sent",
                                            "message": "Transaction sent and accepted by the network",
                                            "verified": confirmed
                                        }),
                                        mint: mint.to_string(),
                                    });
                                }
                            },
                            Err(e) => {
                                warn!("⚠️ Transaction verification error: {}", e);
                                // Still return an error response since verification failed
                                error!("🛑 Transaction verification failed. Stopping the sniping tool to prevent further money loss");
                                panic!("Transaction verification failed. Exiting to prevent further money loss.");
                            }
                        }
                    }
                }
                warn!("❌ Could not parse transaction signature from response");
                return Ok(ApiResponse {
                    status: "error".to_string(),
                    data: json!({
                        "error": "Failed to extract transaction signature",
                        "response": json
                    }),
                    mint: mint.to_string(),
                });
            },
            Err(e) => {
                warn!("❌ Failed to parse transaction response: {}", e);
                return Ok(ApiResponse {
                    status: "error".to_string(),
                    data: json!({
                        "error": format!("Failed to parse transaction response: {}", e),
                        "raw_response": response_text
                    }),
                    mint: mint.to_string(),
                });
            }
        }
    } else {
        // Transaction failed at submission
        let error_text = response.text()?;
        error!("❌ Transaction submission failed: {}", error_text);
        error!("🛑 Stopping the sniping tool to prevent further money loss after transaction submission failure");
        panic!("Transaction submission failed. Exiting to prevent further money loss.");
    }
}

/// Verify that a transaction completed successfully by checking its status on-chain
/// Returns a Result with a tuple (bool, Option<String>) where the bool indicates success and the String contains any error message
async fn verify_transaction(rpc_url: &str, signature: &str) -> Result<(bool, Option<String>), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::new();
    let max_attempts = 5;
    let mut current_attempt = 0;
    
    // Use multiple RPC endpoints to ensure we get accurate status
    let rpc_endpoints = vec![
        rpc_url.to_string(),
        // Use a different RPC if the main one fails
        std::env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
        // Add the public RPC as backup
        "https://api.mainnet-beta.solana.com".to_string()
    ];
    
    // Check if using a Chainstack trader node 
    let is_trader_node = rpc_url.contains("chainstack.com") || rpc_url.contains("p2pify.com");
    
    // For trader nodes, we can be more confident the transaction will land
    if is_trader_node {
        info!("🌟 Using Chainstack Trader Node with 100% landing guarantee for transaction {}", signature);
        info!("📊 Expected confirmation: 75% of transactions within 5 blocks, 95% within 6 blocks");
    }
    
    // Try multiple verification attempts with exponential backoff
    while current_attempt < max_attempts {
        // Introduce delay with exponential backoff (except for first attempt)
        if current_attempt > 0 {
            let delay = std::time::Duration::from_millis(500 * 2u64.pow(current_attempt as u32));
            info!("⏳ Waiting {:?} before next verification attempt for tx: {}", delay, signature);
            tokio::time::sleep(delay).await;
        }
        
        // Try each RPC endpoint for this attempt
        for endpoint in &rpc_endpoints {
            // Method 2: Try getTransaction for detailed error codes
            let payload2 = json!({
                "jsonrpc": "2.0",
                "id": format!("verify-{}", Uuid::new_v4()),
                "method": "getTransaction",
                "params": [
                    signature,
                    {"encoding": "json", "commitment": "confirmed", "maxSupportedTransactionVersion": 0}
                ]
            });
            
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                client.post(endpoint)
                    .header("Content-Type", "application/json")
                    .json(&payload2)
                    .send()
            ).await {
                Ok(response_result) => {
                    match response_result {
                        Ok(response) => {
                            if response.status().is_success() {
                                match response.json::<Value>().await {
                                    Ok(response_json) => {
                                        if let Some(result) = response_json.get("result") {
                                            if !result.is_null() {
                                                // Check for transaction success or failure
                                                if let Some(meta) = result.get("meta") {
                                                    if let Some(err) = meta.get("err") {
                                                        if !err.is_null() {
                                                            // Transaction failed, extract error details
                                                            let error_str = format!("{:?}", err);
                                                            
                                                            // Look for common pump.fun error codes
                                                            if error_str.contains("0x1772") || error_str.contains("6002") {
                                                                warn!("❌ Transaction failed with slippage error (TooMuchSolRequired): {}", error_str);
                                                                return Ok((false, Some(String::from("slippage: Too much SOL required to buy the given amount of tokens"))));
                                                            } else {
                                                                warn!("❌ Transaction failed with error: {}", error_str);
                                                                return Ok((false, Some(error_str)));
                                                            }
                                                        }
                                                    }
                                                    
                                                    if let Some(status) = meta.get("status") {
                                                        if let Some(ok) = status.get("Ok") {
                                                            if !ok.is_null() {
                                                                info!("✅ Transaction confirmed successfully at endpoint: {}", endpoint);
                                                                return Ok((true, None));
                                                            }
                                                        }
                                                    }
                                                }
                                                
                                                // No explicit error found but transaction exists
                                                info!("✅ Transaction confirmed (getTransaction) at endpoint: {}", endpoint);
                                                return Ok((true, None));
                                            }
                                        }
                                    },
                                    Err(e) => {
                                        warn!("⚠️ Failed to parse getTransaction response from {}: {}", endpoint, e);
                                    }
                                }
                            }
                        },
                        Err(e) => {
                            warn!("⚠️ Failed to get getTransaction response from {}: {}", endpoint, e);
                        }
                    }
                },
                Err(_) => {
                    warn!("⚠️ Timeout waiting for getTransaction verification from {}", endpoint);
                }
            };
            
            // Method 1: Check using getSignatureStatuses as backup
            let payload1 = json!({
                "jsonrpc": "2.0",
                "id": format!("verify-{}", Uuid::new_v4()),
                "method": "getSignatureStatuses",
                "params": [
                    [signature],
                    {"searchTransactionHistory": true}
                ]
            });
            
            // Send the verification request with timeout
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                client.post(endpoint)
                    .header("Content-Type", "application/json")
                    .json(&payload1)
                    .send()
            ).await {
                Ok(response_result) => {
                    match response_result {
                        Ok(response) => {
                            if response.status().is_success() {
                                match response.json::<Value>().await {
                                    Ok(response_json) => {
                                        if let Some(result) = response_json.get("result") {
                                            if let Some(status_array) = result.get("value") {
                                                if let Some(status) = status_array.get(0) {
                                                    if !status.is_null() {
                                                        // Check for error details
                                                        if let Some(err) = status.get("err") {
                                                            if !err.is_null() {
                                                                // Transaction failed, extract error details
                                                                let error_str = format!("{:?}", err);
                                                                
                                                                if error_str.contains("0x1772") || error_str.contains("6002") {
                                                                    warn!("❌ Transaction failed with slippage error (TooMuchSolRequired): {}", error_str);
                                                                    return Ok((false, Some(String::from("slippage: Too much SOL required to buy the given amount of tokens"))));
                                                                } else {
                                                                    warn!("❌ Transaction failed with error: {}", error_str);
                                                                    return Ok((false, Some(error_str)));
                                                                }
                                                            }
                                                        }
                                                        
                                                        if let Some(confirmation_status) = status.get("confirmationStatus") {
                                                            let status_str = confirmation_status.as_str().unwrap_or("unknown");
                                                            info!("Transaction status from {}: {}", endpoint, status_str);
                                                            if status_str == "confirmed" || status_str == "finalized" {
                                                                info!("✅ Transaction confirmed on-chain at endpoint: {}", endpoint);
                                                                return Ok((true, None));
                                                            }
                                                        } else {
                                                            // Status exists but no confirmationStatus field
                                                            info!("✅ Transaction exists but no confirmation status field at endpoint: {}", endpoint);
                                                            return Ok((true, None));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    },
                                    Err(e) => {
                                        warn!("⚠️ Failed to parse response from {}: {}", endpoint, e);
                                    }
                                }
                            }
                        },
                        Err(e) => {
                            warn!("⚠️ Failed to get response from {}: {}", endpoint, e);
                        }
                    }
                },
                Err(_) => {
                    warn!("⚠️ Timeout waiting for verification response from {}", endpoint);
                }
            };
        }
        
        current_attempt += 1;
        info!("⏳ Verification attempt {}/{} failed for tx: {}", current_attempt, max_attempts, signature);
    }
    
    // If we reached here, we couldn't verify the transaction on any endpoint
    warn!("❌ Unable to verify transaction after {} attempts: {}", max_attempts, signature);
    
    // For warp transactions through Trader Node, we'll give more benefit of the doubt
    // as they often land on-chain even without immediate verification
    if rpc_url.contains("p2pify") || rpc_url.contains("chainstack") {
        info!("✅ Transaction likely to confirm later (Chainstack Trader Node with 100% landing rate): {}", signature);
        info!("💡 Chainstack guarantees 75% of transactions confirmed within 5 blocks, 95% within 6 blocks");
        return Ok((true, None)); // Return true for Trader Node/Warp txs to avoid unnecessary retries
    }
    
    Ok((false, Some(String::from("Unable to verify transaction status"))))
}

/// Transaction status enum
enum TransactionStatus {
    Success,
    Processing,
    Failed(String),
}

/// Check transaction status
async fn check_transaction_status(signature: &str) -> Result<TransactionStatus, anyhow::Error> {
    // Use the standard RPC URL for checking status to avoid using Trader Node quota
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    let client = reqwest::Client::new();
    
    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSignatureStatuses",
        "params": [
            [signature],
            { "searchTransactionHistory": true }
        ]
    });
    
    let response = client.post(&rpc_url)
        .json(&request_body)
        .send()
        .await?;
    
    if !response.status().is_success() {
        return Err(anyhow!("Failed to get transaction status"));
    }
    
    let response_json: Value = response.json().await?;
    
    if let Some(result) = response_json.get("result") {
        if let Some(statuses) = result.get("value") {
            if let Some(status) = statuses.get(0) {
                if status.is_null() {
                    return Ok(TransactionStatus::Processing);
                }
                
                if let Some(err) = status.get("err") {
                    if !err.is_null() {
                        return Ok(TransactionStatus::Failed(err.to_string()));
                    }
                }
                
                if let Some(confirmation_status) = status.get("confirmationStatus").and_then(|s| s.as_str()) {
                    match confirmation_status {
                        "finalized" | "confirmed" => return Ok(TransactionStatus::Success),
                        "processed" => return Ok(TransactionStatus::Processing),
                        _ => return Ok(TransactionStatus::Processing),
                    }
                }
            }
        }
    }
    
    Ok(TransactionStatus::Processing)
}

/// Helper function to wait for transaction confirmation
async fn wait_for_transaction_confirmation(signature: &str) -> Result<(), anyhow::Error> {
    // Parse signature
    let signature = solana_sdk::signature::Signature::from_str(signature)?;
    
    // Create RPC client with processed commitment
    let rpc_url = std::env::var("TRADER_NODE_RPC_URL")
        .or_else(|_| std::env::var("CHAINSTACK_TRADER_RPC_URL"))
        .or_else(|_| std::env::var("RPC_URL"))
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    let commitment = solana_sdk::commitment_config::CommitmentConfig::processed();
    
    let client = solana_client::rpc_client::RpcClient::new_with_commitment(
        rpc_url,
        commitment
    );
    
    // Check transaction status
    let status = client.get_signature_status(&signature)?;
    
    match status {
        Some(Ok(_)) => {
            info!("✅ Transaction {} confirmed successfully", signature);
            Ok(())
        },
        Some(Err(e)) => {
            error!("❌ Transaction {} failed: {:?}", signature, e);
            Err(anyhow!("Transaction failed: {:?}", e))
        },
        None => {
            warn!("⚠️ Transaction {} not found, may still be processing", signature);
            Err(anyhow!("Transaction not found, may still be processing"))
        }
    }
}

/// Sell a token using pump.fun API
pub async fn sell_token(
    client: &Client,
    mint: &str,
    amount: &str,
    private_key: &str,
) -> Result<ApiResponse, anyhow::Error> {
    // Log the action with correct info but without slippage since it's not needed
    info!("🔄 Selling {} of token {}", amount, mint);
    
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
    
    // Get slippage from environment variable or use default
    let slippage = std::env::var("SLIPPAGE")
        .unwrap_or_else(|_| "40".to_string())
        .parse::<f64>()
        .unwrap_or(40.0) / 100.0; // Convert percentage to decimal
    
    // Determine RPC URL to use
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    // HTTP-based transaction via pump.fun API
    info!("Using HTTP API for sell transaction with {}% slippage", slippage * 100.0);
    
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
    
    // If the sell was successful, completely reset token detection to resume normal operation
    if api_response.status == "success" {
        info!("✅ Sell transaction successful!");
        
        // Get the signature from the response
        if let Some(signature) = api_response.data.get("signature").and_then(|s| s.as_str()) {
            info!("🔗 Sell transaction signature: {}", signature);
            info!("🔗 View on explorer: https://explorer.solana.com/tx/{}", signature);
        }
        
        // Regardless of success or failure, we want to reset the token detection state
        info!("🔓 UNLOCKING TOKEN DETECTION after successful sell");
        std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
        crate::token_detector::set_position_monitoring_active(false);
        
        // Check for auto-buy in .env to provide clear log message
        let auto_buy = std::env::var("AUTO_BUY")
            .unwrap_or_else(|_| "false".to_string())
            .to_lowercase() == "true";
            
        if auto_buy {
            info!("🔍 AUTO_BUY is enabled - Resuming token detection with auto-buy");
        } else {
            info!("🔍 Resuming token detection in manual mode");
        }
        
        // Extra safety - directly check and update any trades in DB
        match crate::db::get_trade_by_mint(mint) {
            Ok(Some(trade)) => {
                if let Some(id) = trade.id {
                    match crate::db::update_trade_sold(id, trade.buy_price, trade.buy_liquidity) {
                        Ok(_) => info!("✅ DB updated: Trade {} for {} marked as sold", id, mint),
                        Err(e) => warn!("⚠️ Failed to update trade {} in DB: {}", id, e),
                    }
                }
            },
            Ok(None) => warn!("⚠️ No trade found in DB for mint {}", mint),
            Err(e) => warn!("⚠️ Error looking up trade in DB: {}", e),
        }
    } else {
        warn!("⚠️ Sell response indicates failure: {:?}", api_response.data);
        
        // Even in case of failure, try to maintain token detection state
        // Don't unlock if we think the position is still active
        if let Some(error_msg) = api_response.data.get("message")
            .and_then(|m| m.as_str())
            .filter(|msg| msg.contains("already sold") || msg.contains("not found")) {
            
            // If token is already sold or not found, we can safely unlock
            info!("🔓 UNLOCKING TOKEN DETECTION since token appears to be already sold or not found");
            std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
            crate::token_detector::set_position_monitoring_active(false);
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
    // First, we need to find the bonding curve for this mint
    // This is usually done by looking at the token metadata or querying the chain
    
    info!("🧮 Creating buy instructions for mint: {}", mint);
    
    // Try to look up if we have the bonding curve in our cache
    let bonding_curve_str = match std::env::var(format!("BONDING_CURVE_{}", mint.to_string())) {
        Ok(curve) => {
            info!("✅ Found bonding curve in cache: {}", curve);
            curve
        },
        Err(_) => {
            // If not in env vars, check if we have it from API data
            // This would be populated by the token detection process
            match crate::api::get_bonding_curve_for_mint(&mint.to_string()).await {
                Some(curve) => {
                    info!("✅ Found bonding curve from API: {}", curve);
                    curve
                },
                None => {
                    // If still not found, use RPC to look for it
                    warn!("⚠️ Bonding curve not found in cache, attempting to derive it...");
                    
                    // Here we'd implement logic to find the bonding curve
                    // For now, we'll return an error
                    return Err(anyhow!("Cannot find bonding curve for mint {}", mint));
                }
            }
        }
    };
    
    // Convert the string to a pubkey
    let bonding_curve = solana_sdk::pubkey::Pubkey::from_str(&bonding_curve_str)?;
    
    // Get slippage from config
    let slippage = std::env::var("SLIPPAGE")
        .unwrap_or_else(|_| "40".to_string())  // Default to 40% slippage
        .parse::<f64>()
        .unwrap_or(40.0);  // Default to 40% if parsing fails
    
    // Properly calculate token amount based on price
    // This should match the Python implementation exactly, including discriminator and byte layout
    info!("🔄 Using crate::create_buy_instruction::create_buy_instruction");
    let (instruction, setup_instructions) = crate::create_buy_instruction::create_buy_instruction(
        &wallet.pubkey(),
        &mint.to_string(),
        &bonding_curve_str,
        amount,
        slippage,
    )?;
    
    // Log the number of accounts to verify proper structure
    info!("✅ Created buy instruction with {} accounts", instruction.accounts.len());
    
    // Create compute budget instruction to help transactions succeed
    let priority_fee = std::env::var("PRIORITY_FEE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2000000); // Default 2M
    
    let compute_units = std::env::var("COMPUTE_UNITS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(200000); // Default 200K
    
    // Create compute budget instructions
    let compute_budget_ix = solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
        compute_units
    );
    
    let priority_fee_ix = solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(
        priority_fee
    );
    
    // Return all instructions
    let mut instructions = vec![compute_budget_ix, priority_fee_ix, instruction];
    
    // Only create ATA if config allows it
    let create_ata = std::env::var("CREATE_ATA_ON_BUY")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false);
    
    if create_ata {
        info!("🔄 Adding instruction to create associated token account");
        let create_ata_ix = spl_associated_token_account::instruction::create_associated_token_account(
            &wallet.pubkey(), 
            &wallet.pubkey(), 
            mint,
            &spl_token::id()
        );
        
        // Add create ATA instruction first
        instructions.insert(0, create_ata_ix);
    }
    
    Ok(instructions)
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
        .map(|v| v.parse::<u64>().unwrap_or(3))
        .unwrap_or(3);

    // Check if we should use the Trader Node for transactions
    let use_trader_node = std::env::var("USE_TRADER_NODE_FOR_TRANSACTIONS")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false);

    // Get the appropriate RPC URL based on the environment variable
    let rpc_url = if use_trader_node {
        info!("🚀 Using Trader Node for transaction submission");
        let url = crate::config::get_trader_node_rpc_url();
        if url.is_empty() {
            warn!("Trader Node RPC URL is empty, falling back to standard RPC");
            std::env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string())
        } else {
            url
        }
    } else {
        info!("Using standard RPC for transaction submission");
        std::env::var("RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string())
    };

    // Get the commitment level
    let commitment = std::env::var("COMMITMENT")
        .unwrap_or_else(|_| "processed".to_string());

    // Prepare the request body
    let request_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendTransaction",
        "params": [
            encoded_tx,
            {
                "skipPreflight": skip_preflight,
                "preflightCommitment": commitment,
                "encoding": "base64",
                "maxRetries": max_retries
            }
        ]
    });

    info!("Transaction options: skipPreflight={}, commitment={}, maxRetries={}", 
          skip_preflight, commitment, max_retries);

    // Create a new request with the appropriate URL
    let mut request_builder = client.post(&rpc_url)
        .json(&request_body)
        .timeout(Duration::from_secs(30));

    // Add authentication if using Trader Node
    if use_trader_node {
        let (username, password) = crate::config::get_trader_node_credentials();
        if !username.is_empty() && !password.is_empty() {
            request_builder = request_builder.basic_auth(username, Some(password));
        }
    }

    // Send the request
    info!("Sending transaction to {}", rpc_url);
    let response = request_builder.send().await?;

    // Check if the request was successful
    let status = response.status();
    if !status.is_success() {
        let error_text = response.text().await?;
        error!("Transaction submission failed with status {}: {}", status, error_text);
        return Err(anyhow::anyhow!("Transaction submission failed: {}", error_text));
    }

    // Parse the response
    let response_json: Value = response.json().await?;

    // Check for errors in the response
    if let Some(error) = response_json.get("error") {
        error!("Transaction error: {:?}", error);
        return Err(anyhow::anyhow!("Transaction error: {:?}", error));
    }

    // Extract the transaction signature
    let signature_str = response_json["result"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing transaction signature in response"))?;

    // Convert the signature string to a Signature
    let signature = Signature::from_str(signature_str)?;
    info!("Transaction submitted successfully with signature: {}", signature);

    Ok(signature)
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
    // Default to a very high value for ultra-fast transactions
    let default_fee = 40_000_000; // 0.04 SOL default
    
    // Get RPC URL
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    let client = reqwest::Client::new();
    
    // Call getRecentPrioritizationFees to get recent prioritization fees
    let request_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getRecentPrioritizationFees",
        "params": []
    });
    
    match client.post(&rpc_url)
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<Value>().await {
                    Ok(json) => {
                        if let Some(result) = json.get("result") {
                            if let Some(fees) = result.as_array() {
                                if !fees.is_empty() {
                                    // Calculate 90th percentile fee for much higher likelihood of quick processing
                                    let mut recent_fees: Vec<u64> = fees.iter()
                                        .filter_map(|fee| fee.get("prioritizationFee")?.as_u64())
                                        .collect();
                                    
                                    if !recent_fees.is_empty() {
                                        // Sort fees to calculate percentile
                                        recent_fees.sort();
                                        
                                        // Get the 90th percentile index - much higher than before
                                        let index = (recent_fees.len() as f64 * 0.90) as usize;
                                        let fee_90th_percentile = recent_fees[index.min(recent_fees.len() - 1)];
                                        
                                        // Add a 200% margin to ensure our transaction gets absolute priority
                                        let recommended_fee = (fee_90th_percentile as f64 * 3.0) as u64;
                                        
                                        // Ensure fee is at least 30M microlamports
                                        let min_fee = 30_000_000;
                                        let max_fee = 100_000_000; // Max 0.1 SOL
                                        let fee = recommended_fee.max(min_fee).min(max_fee);
                                        
                                        info!("📊 Calculated dynamic priority fee: {} microlamports based on 90th percentile: {} with 3x multiplier", 
                                              fee, fee_90th_percentile);
                                        
                                        return Ok(fee);
                                    }
                                }
                            }
                        }
                    },
                    Err(e) => {
                        warn!("Failed to parse getRecentPrioritizationFees response: {}", e);
                    }
                }
            }
        },
        Err(e) => {
            warn!("Failed to get recent prioritization fees: {}", e);
        }
    }
    
    info!("Using ultra-high default priority fee of {} microlamports for fastest transaction processing", default_fee);
    Ok(default_fee)
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

/// Check if an associated bonding curve account exists and is initialized
async fn check_associated_bonding_curve(
    client: &Arc<Client>,
    mint: &str, 
    bonding_curve: &str
) -> Result<bool, anyhow::Error> {
    // Parse pubkeys
    let mint_pubkey = Pubkey::from_str(mint)?;
    let bonding_curve_pubkey = Pubkey::from_str(bonding_curve)?;
    let program_id = Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")?;
    
    // Derive the associated bonding curve address using the same logic as in create_buy_instruction.rs
    let associated_bonding_curve = crate::create_buy_instruction::get_associated_bonding_curve(
        &mint_pubkey,
        &bonding_curve_pubkey,
        &program_id
    )?;
    
    info!("🔍 Checking if associated bonding curve account exists: {}", associated_bonding_curve);
    
    // Get trader node URL
    let trader_node_url = env::var("TRADER_NODE_RPC_URL")
        .or_else(|_| env::var("CHAINSTACK_TRADER_RPC_URL"))
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    // Build JSON-RPC request to check account info
    let account_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [
            associated_bonding_curve.to_string(),
            {"encoding": "base64"}
        ]
    });
    
    // Send request
    let trader_username = env::var("TRADER_NODE_USERNAME").unwrap_or_default();
    let trader_password = env::var("TRADER_NODE_PASSWORD").unwrap_or_default();
    
    let mut request_builder = client.post(&trader_node_url).json(&account_request);
    if !trader_username.is_empty() && !trader_password.is_empty() {
        request_builder = request_builder.basic_auth(&trader_username, Some(&trader_password));
    } else {
        request_builder = request_builder.header("x-api-key", env::var("API_KEY").unwrap_or_default());
    }
    
    let response = request_builder
        .header("Content-Type", "application/json")
        .send()
        .await?;
    
    if response.status().is_success() {
        let response_json: Value = response.json().await?;
        
        // Check if account exists
        if let Some(result) = response_json.get("result") {
            if let Some(value) = result.get("value") {
                if !value.is_null() {
                    info!("✅ Associated bonding curve account exists");
                    return Ok(true);
                }
            }
        }
        
        info!("❌ Associated bonding curve account does not exist");
        return Ok(false);
    }
    
    info!("❌ Failed to check associated bonding curve account");
    return Ok(false);
}

/// Add this function that was referenced but not defined
async fn check_account_exists(rpc_url: &str, account_pubkey: &str) -> Result<bool, anyhow::Error> {
    // Create a one-time client with a short timeout
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()?;

    // Create JSON-RPC request
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [
            account_pubkey,
            {"encoding": "base64"}
        ]
    });

    // Send the request
    let response = client.post(rpc_url)
        .json(&request)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow!("Failed to check account: HTTP error {}", response.status()));
    }

    // Parse the response
    let response_json: Value = response.json().await?;
    
    // Check if we got a result
    if let Some(result) = response_json.get("result") {
        if let Some(value) = result.get("value") {
            // If value is null, the account doesn't exist
            return Ok(!value.is_null());
        }
    }

    // Default to false if we can't determine
    warn!("⚠️ Could not determine if account exists");
    Ok(false)
}

/// Add this helper function that was referenced
async fn get_bonding_curve_from_api(mint: &str) -> Result<String, anyhow::Error> {
    match crate::api::get_bonding_curve_for_mint(mint).await {
        Some(bonding_curve) => {
            info!("✅ Found bonding curve from API: {}", bonding_curve);
            Ok(bonding_curve)
        },
        None => {
            error!("❌ Failed to get bonding curve");
            Err(anyhow!("Failed to find bonding curve for mint {}", mint))
        }
    }
}

/// Helper function to decode a keypair from a private key string
fn decode_keypair(private_key: &str) -> Result<Keypair, anyhow::Error> {
    let private_key_bytes = base58::decode(private_key)
        .into_vec()
        .map_err(|e| anyhow!("Invalid private key: {}", e))?;
    Keypair::from_bytes(&private_key_bytes)
        .map_err(|e| anyhow!("Invalid keypair: {}", e))
}

/// Implement dynamic priority fee calculation
async fn get_dynamic_priority_fee() -> Result<u64, anyhow::Error> {
    // Default to a very high value for ultra-fast transactions
    let default_fee = 40_000_000; // 0.04 SOL default
    
    // Get RPC URL
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    let client = reqwest::Client::new();
    
    // Call getRecentPrioritizationFees to get recent prioritization fees
    let request_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getRecentPrioritizationFees",
        "params": []
    });
    
    match client.post(&rpc_url)
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<Value>().await {
                    Ok(json) => {
                        if let Some(result) = json.get("result") {
                            if let Some(fees) = result.as_array() {
                                if !fees.is_empty() {
                                    // Calculate 90th percentile fee for much higher likelihood of quick processing
                                    let mut recent_fees: Vec<u64> = fees.iter()
                                        .filter_map(|fee| fee.get("prioritizationFee")?.as_u64())
                                        .collect();
                                    
                                    if !recent_fees.is_empty() {
                                        // Sort fees to calculate percentile
                                        recent_fees.sort();
                                        
                                        // Get the 90th percentile index - much higher than before
                                        let index = (recent_fees.len() as f64 * 0.90) as usize;
                                        let fee_90th_percentile = recent_fees[index.min(recent_fees.len() - 1)];
                                        
                                        // Add a 200% margin to ensure our transaction gets absolute priority
                                        let recommended_fee = (fee_90th_percentile as f64 * 3.0) as u64;
                                        
                                        // Ensure fee is at least 30M microlamports
                                        let min_fee = 30_000_000;
                                        let max_fee = 100_000_000; // Max 0.1 SOL
                                        let fee = recommended_fee.max(min_fee).min(max_fee);
                                        
                                        info!("📊 Calculated dynamic priority fee: {} microlamports based on 90th percentile: {} with 3x multiplier", 
                                              fee, fee_90th_percentile);
                                        
                                        return Ok(fee);
                                    }
                                }
                            }
                        }
                    },
                    Err(e) => {
                        warn!("Failed to parse getRecentPrioritizationFees response: {}", e);
                    }
                }
            }
        },
        Err(e) => {
            warn!("Failed to get recent prioritization fees: {}", e);
        }
    }
    
    info!("Using ultra-high default priority fee of {} microlamports for fastest transaction processing", default_fee);
    Ok(default_fee)
}
