use std::str::FromStr;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use tokio::time::Duration;
use anyhow::anyhow;
use log::{debug, error, info, warn};
use crate::api;
use crate::database;
use std::sync::Arc;

pub async fn handle_buy_token(token: &crate::api::models::TokenData, force: bool) -> Result<(), anyhow::Error> {
    let start_time = std::time::Instant::now();
    
    info!("🔵 Starting buy token process for: {} ({})", 
          token.name.as_deref().unwrap_or("Unknown"), token.mint);
    
    // Check auto buy setting first
    let auto_buy = std::env::var("AUTO_BUY")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
    
    if !auto_buy && !force {
        info!("❌ AUTO_BUY is disabled in config. Skipping purchase.");
        return Err(anyhow!("AUTO_BUY is disabled. Enable it in config.env or use force parameter."));
    }
    
    info!("✅ Auto-buy is enabled in config");
    
    // Get RPC URL for transactions
    let rpc_url = std::env::var("TRADER_NODE_RPC_URL")
        .or_else(|_| std::env::var("CHAINSTACK_TRADER_RPC_URL"))
        .unwrap_or_else(|_| std::env::var("RPC_URL")
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()));
    
    info!("🌐 Using RPC URL: {}", rpc_url);
    
    // Fetch slippage from config
    let slippage = std::env::var("SLIPPAGE")
        .unwrap_or_else(|_| "30".to_string())  // Default 30% if not specified
        .parse::<f64>()
        .unwrap_or(30.0);  // Already in percentage format for API
        
    info!("⚙️ Using slippage: {}%", slippage);
    
    // Fetch amount from config
    let amount = std::env::var("AMOUNT")
        .unwrap_or_else(|_| "0.01".to_string())
        .parse::<f64>()
        .unwrap_or(0.01);  // Default to 0.01 SOL if not specified
        
    info!("💰 Using amount: {} SOL", amount);
    
    // Get HTTP client
    info!("🔄 Getting HTTP client from global state");
    let http_client = match crate::api::HTTP_CLIENT.get_client() {
        Some(client) => {
            info!("✅ Successfully got HTTP client from global state");
            Arc::clone(client)
        },
        None => {
            error!("❌ Failed to get HTTP client. Creating new client.");
            let client = Arc::new(crate::api::utils::create_speed_optimized_client());
            Arc::clone(&client)
        }
    };
    
    // Get private key for transactions
    info!("🔑 Getting private key from environment");
    let private_key = match std::env::var("PRIVATE_KEY") {
        Ok(key) => {
            info!("✅ Private key found in environment (length: {})", key.len());
            key
        },
        Err(e) => {
            error!("❌ Failed to get private key: {}", e);
            return Err(anyhow!("Private key not found in environment: {}", e));
        }
    };
    
    // Log that we're about to start the buy process
    info!("🚀 Starting buy process for token: {}", token.mint);
    let buy_start = std::time::Instant::now();
    
    // Call buy_token function from the API module
    info!("🔵 DEBUG: About to call api::transactions::buy_token for {}", token.mint);
    info!("🔵 DEBUG: HTTP_CLIENT is initialized and ready to use");
    info!("🔵 DEBUG: Amount: {}, Slippage: {}", amount, slippage);
    
    // Execute the buy_token function and handle response
    match api::transactions::buy_token(&http_client, &private_key, &token.mint, amount, slippage).await {
        Ok(response) => {
            // Store the status - this could be "success" or "pending"
            let status = response.status.clone();
            info!("✅ Buy request completed with status: {}", status);
            
            let signature = match response.data.get("signature") {
                Some(sig) => {
                    let sig_str = sig.as_str().unwrap_or("unknown").to_string();
                    info!("✅ Transaction signature: {}", sig_str);
                    sig_str
                },
                None => {
                    warn!("⚠️ No transaction signature in response");
                    "unknown".to_string()
                }
            };

            // Create buy duration and total duration calculations
            let buy_duration = buy_start.elapsed();
            let total_duration = start_time.elapsed();
            let detection_to_buy_ms = total_duration.as_millis() as i64;

            // Log basic success info
            info!("✅ TRANSACTION SENT FOR TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s", 
                  token.mint, buy_duration.as_secs_f64(), total_duration.as_secs_f64());
            
            // If the transaction was actually sent (we have a signature)
            if signature != "unknown" {
                // Record in database
                info!("💾 Recording transaction in database");
                let buy_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;

                match crate::db::insert_trade(
                    &token.mint,
                    &token.name.clone().unwrap_or_else(|| "Unknown".to_string()),
                    amount, // Use amount as the buy_price
                    0.0, // buy_liquidity is not known at this point
                    total_duration.as_millis() as i64, // detection_time
                    buy_time
                ) {
                    Ok(_) => {
                        info!("✅ Successfully recorded transaction in database using db::insert_trade");
                    },
                    Err(e) => {
                        warn!("⚠️ Failed to record trade in database: {}", e);
                    }
                }
                
                // Set up monitoring flags
                info!("🔒 Setting up position monitoring");
                let max_positions = std::env::var("MAX_POSITIONS")
                    .unwrap_or_else(|_| "1".to_string())
                    .parse::<i64>()
                    .unwrap_or(1);
                    
                info!("MAX_POSITIONS ({}) reached after buying token {}", max_positions, token.mint);
                std::env::set_var("_STOP_WEBSOCKET_LISTENER", "true");
                std::env::set_var("_MONITORING_ACTIVE_TRANSACTION", "true");
                info!("🔒 Focusing all resources on monitoring this position");
                
                // Return success
                info!("✅ Buy process completed successfully for token: {}", token.mint);
                return Ok(());
            } else {
                // No signature means we couldn't even send the transaction
                warn!("⚠️ No transaction signature received, cannot verify token purchase");
                return Err(anyhow!("Failed to get transaction signature"));
            }
        },
        Err(e) => {
            // Log the error with details
            error!("❌ Failed to buy token: {}", e);
            return Err(anyhow!("Failed to buy token: {}", e));
        }
    }
}

/// Comprehensive token purchase verification process
/// This checks both transaction status and wallet balance
async fn verify_transaction_and_update_status(
    mint: String, 
    signature: String, 
    private_key: String,
    amount: f64,
) {
    // Spawn a new task that won't block the main execution
    tokio::spawn(async move {
        info!("🔍 Starting verification process for transaction: {}", signature);
        
        // Wait for initial network propagation - reduced to improve speed
        tokio::time::sleep(Duration::from_secs(1)).await;
        
        // Create an RPC client with the best available endpoint
        let rpc_url = std::env::var("CHAINSTACK_TRADER_RPC_URL")
            .or_else(|_| std::env::var("RPC_URL"))
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
        
        // Create a client with confirmed commitment - higher chance of success
        let rpc_client = match solana_client::rpc_client::RpcClient::new_with_commitment(
            rpc_url.clone(),
            solana_sdk::commitment_config::CommitmentConfig::confirmed()
        ) {
            client => client,
        };
        
        // Parse the signature
        let signature_parsed = match Signature::from_str(&signature) {
            Ok(sig) => sig,
            Err(e) => {
                error!("❌ Invalid signature format '{}': {}", signature, e);
                // Update database to reflect failed transaction
                let _ = crate::db::update_trade_status(&mint, "failed").await;
                cleanup_monitoring_flags(&mint, false).await;
                return;
            }
        };
        
        // Check transaction status with improved retry logic
        let mut transaction_confirmed = false;
        let max_tx_check_attempts = 10; // Increased to allow more time
        
        for attempt in 0..max_tx_check_attempts {
            // Using Python-like exponential backoff
            let delay = if attempt == 0 { 1 } else { 2u64.pow(attempt.min(6)) }; // Cap at 64 seconds
            
            info!("Checking transaction status (attempt {}/{}), waiting {} seconds...", 
                 attempt + 1, max_tx_check_attempts, delay);
            tokio::time::sleep(Duration::from_secs(delay)).await;
            
            match rpc_client.get_signature_status(&signature_parsed) {
                Ok(Some(Ok(_))) => {
                    info!("✅ TRANSACTION CONFIRMED: {}", signature);
                    transaction_confirmed = true;
                    break;
                },
                Ok(Some(Err(e))) => {
                    error!("❌ TRANSACTION FAILED: {}, Error: {:?}", signature, e);
                    // Transaction was rejected by the network
                    let _ = crate::db::update_trade_status(&mint, "failed").await;
                    cleanup_monitoring_flags(&mint, false).await;
                    return;
                },
                Ok(None) => {
                    info!("⏳ Transaction still pending: {}", signature);
                    // Continue to next attempt
                },
                Err(e) => {
                    warn!("⚠️ Failed to check transaction status: {}", e);
                    // Try using a different API endpoint on error
                    if attempt > 1 {
                        let backup_url = match attempt % 3 {
                            0 => "https://api.mainnet-beta.solana.com",
                            1 => "https://solana-api.projectserum.com",
                            _ => "https://rpc.ankr.com/solana",
                        };
                        
                        let backup_client = solana_client::rpc_client::RpcClient::new_with_commitment(
                            backup_url.to_string(),
                            solana_sdk::commitment_config::CommitmentConfig::confirmed()
                        );
                        
                        // Try the backup client
                        match backup_client.get_signature_status(&signature_parsed) {
                            Ok(Some(Ok(_))) => {
                                info!("✅ TRANSACTION CONFIRMED (via backup RPC): {}", signature);
                                transaction_confirmed = true;
                                break;
                            },
                            Ok(Some(Err(e))) => {
                                error!("❌ TRANSACTION FAILED (via backup RPC): {}, Error: {:?}", signature, e);
                                let _ = crate::db::update_trade_status(&mint, "failed").await;
                                cleanup_monitoring_flags(&mint, false).await;
                                return;
                            },
                            _ => {
                                // Continue with next attempt
                            }
                        }
                    }
                }
            }
        }
        
        if transaction_confirmed {
            // Update database to reflect confirmed purchase if transaction is confirmed
            let _ = crate::db::update_trade_status(&mint, "confirmed").await;
            info!("✅ Transaction {} confirmed on-chain", signature);
        } else {
            warn!("⚠️ Transaction was not confirmed after {} attempts - checking wallet balance", 
                 max_tx_check_attempts);
        }
        
        // Even if transaction status checking failed, check wallet balance as ultimate verification
        let mut token_in_wallet = false;
        let max_wallet_check_attempts = 10; // Increased to allow more time
        
        for attempt in 0..max_wallet_check_attempts {
            // Using Python-like exponential backoff
            let delay = if attempt == 0 { 2 } else { 2u64.pow(attempt.min(6)) }; // Cap at 64 seconds
            
            info!("Verifying token in wallet (attempt {}/{}), waiting {} seconds...", 
                 attempt + 1, max_wallet_check_attempts, delay);
            tokio::time::sleep(Duration::from_secs(delay)).await;
            
            // Check wallet balance
            match verify_token_in_wallet(&mint, &private_key).await {
                Ok(true) => {
                    info!("✅ TOKEN VERIFIED IN WALLET: {}", mint);
                    token_in_wallet = true;
                    
                    // Update database to reflect confirmed purchase
                    match crate::db::update_trade_status(&mint, "confirmed").await {
                        Ok(_) => info!("✅ Updated token status to 'confirmed' in database"),
                        Err(e) => warn!("Failed to update token status: {}", e),
                    }
                    
                    // Set a flag to indicate we have an actual verified position
                    std::env::set_var("_HAS_VERIFIED_POSITION", "true");
                    
                    // Token found - exit verification process
                    info!("Token {} successfully purchased for {} SOL", mint, amount);
                    return;
                },
                Ok(false) => {
                    warn!("⚠️ Token not found in wallet: {}", mint);
                    // Continue to next attempt
                },
                Err(e) => {
                    error!("❌ Error checking wallet: {}", e);
                    // Continue to next attempt
                }
            }
        }
        
        // If we reach here, the token could not be verified in the wallet
        if !token_in_wallet {
            error!("❌ TOKEN NOT FOUND IN WALLET: {} after {} verification attempts", 
                  mint, max_wallet_check_attempts);
            
            // Update database to reflect failed purchase
            match crate::db::update_trade_status(&mint, "failed").await {
                Ok(_) => info!("Updated token status to 'failed' in database"),
                Err(e) => warn!("Failed to update token status: {}", e),
            }
            
            // Clean up monitoring flags since we failed to verify the token
            cleanup_monitoring_flags(&mint, false).await;
            
            // Also log the transaction signature for investigation
            error!("Transaction {} could not be verified as successful - position marked as failed", signature);
        }
    });
}

/// Clean up monitoring flags and optionally restart websocket listener
async fn cleanup_monitoring_flags(mint: &str, restart_websocket: bool) {
    info!("🧹 Cleaning up monitoring flags for mint: {}", mint);
    
    // Clear transaction monitoring flag
    std::env::remove_var("_MONITORING_ACTIVE_TRANSACTION");
    
    // Optionally restart websocket listener if needed
    if restart_websocket {
        info!("🔄 Restarting websocket listener for token detection");
        std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
    } else {
        info!("🛑 Keeping websocket listener paused");
    }
    
    // Clear verified position flag if it exists
    std::env::remove_var("_HAS_VERIFIED_POSITION");
}

/// Verify that a token exists in the user's wallet
async fn verify_token_in_wallet(mint: &str, private_key: &str) -> Result<bool, anyhow::Error> {
    // Create keypair from private key
    let keypair = solana_sdk::signature::Keypair::from_base58_string(private_key);
    let pubkey = keypair.pubkey();
    
    // Parse mint pubkey
    let mint_pubkey = solana_sdk::pubkey::Pubkey::from_str(mint)?;
    
    // Calculate the associated token account
    let associated_token_address = spl_associated_token_account::get_associated_token_address(
        &pubkey,
        &mint_pubkey
    );
    
    // Create RPC client - Try to use highest quality RPC endpoint available
    let rpc_url = std::env::var("CHAINSTACK_TRADER_RPC_URL")
        .or_else(|_| std::env::var("RPC_URL"))
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    let rpc_client = solana_client::rpc_client::RpcClient::new_with_commitment(
        rpc_url,
        solana_sdk::commitment_config::CommitmentConfig::confirmed() // Use confirmed commitment
    );
    
    // Enhanced: First check token account existence explicitly
    match rpc_client.get_account(&associated_token_address) {
        Ok(account) => {
            // Account exists, now check balance
            match rpc_client.get_token_account_balance(&associated_token_address) {
                Ok(balance) => {
                    let amount = balance.amount.parse::<u64>().unwrap_or(0);
                    if amount > 0 {
                        info!("✅ Token balance confirmed: {} tokens of mint {}", amount, mint);
                        return Ok(true);
                    } else {
                        info!("⚠️ Token account exists but has zero balance for mint {}", mint);
                        return Ok(false);
                    }
                },
                Err(e) => {
                    info!("⚠️ Failed to get token balance: {}", e);
                    return Ok(false); // Account exists but we couldn't read balance
                }
            }
        },
        Err(e) => {
            // Detect if error is "account not found" vs other errors
            if e.to_string().contains("not found") || e.to_string().contains("does not exist") {
                info!("⚠️ Token account does not exist for mint {}", mint);
                return Ok(false);
            }
            
            // Other errors should be returned
            warn!("⚠️ Error checking token account: {}", e);
            return Err(anyhow!("Failed to check token account: {}", e));
        }
    }
}

// ... rest of the file ... 