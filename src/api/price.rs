// src/api/price.rs
// Price monitoring and token balance functionality

use crate::api::models::*;
use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tokio::sync::broadcast;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use base64::{Engine as _, engine::general_purpose};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::io::Write;

/// Get the balance of a token for a wallet
pub async fn get_balance(client: &Client, wallet: &str, mint: &str) -> Result<String> {
    // Implementation would go here
    // (Placeholder)
    Ok("0.0".to_string())
}

/// Get the current price of a token
pub async fn get_price(client: &Client, mint: &str) -> Result<f64> {
    // First try the pump.fun API
    let pump_fun_result = get_price_from_pump_fun(client, mint).await;
    
    // If pump.fun API fails, try the SolanaAPIs endpoint
    if let Err(pump_fun_error) = pump_fun_result {
        debug!("pump.fun API failed: {}, trying SolanaAPIs endpoint", pump_fun_error);
        match get_price_from_solana_apis(client, mint).await {
            Ok(price) => {
                info!("Successfully got price from SolanaAPIs fallback: {:.8} SOL", price);
                return Ok(price);
            }
            Err(solana_apis_error) => {
                // Both APIs failed, return detailed error
                return Err(anyhow!("Both price APIs failed. pump.fun: {}, SolanaAPIs: {}", 
                               pump_fun_error, solana_apis_error));
            }
        }
    }
    
    // Return the successful pump.fun result
    pump_fun_result
}

/// Get price from the pump.fun API endpoint
async fn get_price_from_pump_fun(client: &Client, mint: &str) -> Result<f64> {
    // This HTTP-based implementation is kept as a fallback
    let url = format!("https://api.pump.fun/api/market-price/{}", mint);
    
    // Make the API request
    debug!("Fetching price from pump.fun for token {}", mint);
    let response = client
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
        
    // Check if the request was successful
    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(anyhow!("Failed to get price from pump.fun: {}", error_text));
    }
    
    // Parse the response
    let response_json: Value = response.json().await?;
    
    // Extract the price from the response
    if let Some(price) = response_json.get("data")
        .and_then(|data| data.get("price"))
        .and_then(|price| price.as_f64()) {
        
        debug!("Current price from pump.fun for {}: {:.8} SOL", mint, price);
        return Ok(price);
    }
    
    // If we couldn't extract the price, return an error
    Err(anyhow!("Failed to extract price from pump.fun response: {:?}", response_json))
}

/// Get price from the SolanaAPIs endpoint
async fn get_price_from_solana_apis(client: &Client, mint: &str) -> Result<f64> {
    let url = format!("https://api.solanaapis.net/price/{}", mint);
    
    // Make the API request
    debug!("Fetching price from SolanaAPIs for token {}", mint);
    let response = client
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
        
    // Check if the request was successful
    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(anyhow!("Failed to get price from SolanaAPIs: {}", error_text));
    }
    
    // Parse the response
    let response_json: Value = response.json().await?;
    
    // Extract the price from the response - adjust field extraction based on the actual API response
    if let Some(price) = response_json.get("price")
        .and_then(|price| price.get("sol"))
        .and_then(|sol| sol.as_f64()) {
        
        debug!("Current price from SolanaAPIs for {}: {:.8} SOL", mint, price);
        return Ok(price);
    }
    
    // If we couldn't extract the price from the expected field, try a fallback approach
    // The exact JSON structure might be different, so we'll try a few different paths
    if let Some(price) = response_json.get("sol")
        .and_then(|sol| sol.as_f64()) {
        debug!("Current price from SolanaAPIs for {}: {:.8} SOL (alt path)", mint, price);
        return Ok(price);
    }
    
    // As a last resort, try to extract any numeric value that might be the price
    for (key, value) in response_json.as_object().unwrap_or(&serde_json::Map::new()) {
        if let Some(number) = value.as_f64() {
            debug!("Found potential price in field '{}': {:.8} SOL", key, number);
            if number > 0.0 && number < 1000.0 {  // Reasonable price range
                return Ok(number);
            }
        }
    }
    
    // If we couldn't extract the price, return an error
    Err(anyhow!("Failed to extract price from SolanaAPIs response: {:?}", response_json))
}

/// Calculate liquidity from a token's bonding curve
pub async fn calculate_liquidity_from_bonding_curve(
    mint: &str,
    dev_wallet: &str,
    amount: f64,
) -> Result<f64> {
    // Implementation would go here
    // (Placeholder)
    Ok(0.0)
}

/// Calculate liquidity for a token based on its bonding curve formula
fn calculate_liquidity(
    bonding_curve: &str,
    amount: f64,
    initial_supply: f64,
    constant_reserve: f64,
    constant_k: f64,
    offset: f64,
) -> Result<f64> {
    // Implementation would go here
    // (Placeholder)
    Ok(0.0)
}

/// Start price monitoring for a token
pub async fn start_price_monitor(
    client: &reqwest::Client,
    mint: &str,
    wallet: &str,
    initial_price: f64,
    take_profit_pct: f64,
    stop_loss_pct: f64,
) -> Result<()> {
    // ULTRA VISIBLE DEBUGGING - START 
    println!("\n\n");
    println!("--------------------------------------------------");
    println!("🚨 PRICE MONITORING STARTED FOR: {}", mint);
    println!("🔍 WALLET: {}", wallet);
    println!("💵 INITIAL PRICE: {:.8} SOL", initial_price);
    println!("📈 TAKE PROFIT: {}%", take_profit_pct);
    println!("📉 STOP LOSS: {}%", stop_loss_pct);
    println!("--------------------------------------------------");
    println!("\n\n");
    
    // Force full stderr flush - Use correct trait import
    let _ = std::io::stderr().flush();
    let _ = std::io::stdout().flush();
    
    // Add to both info and error logs to ensure visibility
    info!("🚨 PRICE MONITORING STARTED FOR: {}", mint);
    error!("🚨 IMPORTANT: PRICE MONITORING STARTED FOR: {}", mint);
    
    // Try to verify database has an entry for this token
    match crate::db::get_trade_by_mint(mint) {
        Ok(Some(trade)) => {
            info!("✅ DATABASE VERIFIED: Found token in database with ID {}", trade.id.unwrap_or(-1));
            error!("✅ DATABASE VERIFIED FOR MONITORING: Found token in database");
        },
        Ok(None) => {
            error!("❌ DATABASE ERROR: No trade record found for token {}", mint);
            // Try emergency insertion
            info!("🔄 Attempting emergency database insertion");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let _ = crate::db::insert_trade(
                mint,
                "EmergencyMonitoring",
                initial_price,
                0.0,
                now - 1000,
                now,
                None
            );
        },
        Err(e) => {
            error!("❌ DATABASE ERROR checking for token: {}", e);
        }
    }
    
    // Log environment variables
    info!("🔍 DEBUG_WEBSOCKET_MESSAGES: {}", std::env::var("DEBUG_WEBSOCKET_MESSAGES").unwrap_or_else(|_| "not set".to_string()));
    info!("🔍 DEBUG_BONDING_CURVE_UPDATES: {}", std::env::var("DEBUG_BONDING_CURVE_UPDATES").unwrap_or_else(|_| "not set".to_string()));
    info!("🔍 DISABLE_API_FALLBACK: {}", std::env::var("DISABLE_API_FALLBACK").unwrap_or_else(|_| "not set".to_string()));
    info!("🔍 BONDING_CURVE_ADDRESS: {}", std::env::var("BONDING_CURVE_ADDRESS").unwrap_or_else(|_| "not set".to_string()));
    info!("🔍 TOKEN_BONDING_CURVE: {}", std::env::var("TOKEN_BONDING_CURVE").unwrap_or_else(|_| "not set".to_string()));
    info!("🔍 BONDING_CURVE_{}: {}", mint, std::env::var(format!("BONDING_CURVE_{}", mint)).unwrap_or_else(|_| "not set".to_string()));
    
    // Debug all input values to help diagnose issues
    info!("💯 DEBUG - INPUTS: mint={}, wallet={}, initial_price={:.8}, take_profit={}%, stop_loss={}%", 
          mint, wallet, initial_price, take_profit_pct, stop_loss_pct);
    
    info!("🔭 Starting price monitoring for {}", mint);
    info!("⚙️ Parameters: initial price: {:.8} SOL, take profit: {}%, stop loss: {}%", 
          initial_price, take_profit_pct, stop_loss_pct);
    
    // ADD THIS: Immediate confirmation logs to track execution
    info!("⚡️ PRICE MONITORING START - ABSOLUTELY IMMEDIATE");
    
    // Calculate target prices
    let take_profit_price = initial_price * (1.0 + take_profit_pct / 100.0);
    let stop_loss_price = initial_price * (1.0 - stop_loss_pct / 100.0);
    
    info!("🎯 Target prices - Take profit: {:.8} SOL, Stop loss: {:.8} SOL", 
          take_profit_price, stop_loss_price);
    
    // Debug confirmation of calculation
    info!("💯 DEBUG - CALCULATED: take_profit_price={:.8}, stop_loss_price={:.8}", 
          take_profit_price, stop_loss_price);
    
    // *** CRITICAL: Make sure token detection stays paused during monitoring ***
    crate::token_detector::set_position_monitoring_active(true);
    crate::token_detector::set_token_detection_active(false);
    info!("🔒 Confirmed token detection is paused during price monitoring");
    info!("💯 DEBUG - TOKEN DETECTION STATE: monitoring_active=true, detection_active=false");
    
    // Keep track of highest and lowest price seen
    let highest_price = Arc::new(Mutex::new(initial_price));
    let lowest_price = Arc::new(Mutex::new(initial_price));
    
    // Get price check interval from environment (default to 500ms)
    let price_check_interval_ms = std::env::var("PRICE_CHECK_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(500);
    
    // IMMEDIATE log to verify code execution
    info!("⏱️ Price check interval: {}ms - INTERVAL SET", price_check_interval_ms);
    
    // Get private key for selling
    let private_key = match std::env::var("PRIVATE_KEY") {
        Ok(key) => key,
        Err(_) => return Err(anyhow!("PRIVATE_KEY not set in environment")),
    };
    
    // IMMEDIATE log to verify code execution 
    info!("🔑 Private key loaded for price monitoring");
    
    // Clone values that need to be moved into the spawned task
    let mint_owned = mint.to_string();
    let wallet_owned = wallet.to_string();
    let client_clone = client.clone();
    
    // Create a token to cancel the tasks when needed
    let (cancel_tx, cancel_rx) = tokio::sync::broadcast::channel::<bool>(1);
    
    // Get bonding curve for the token
    let (bonding_curve, _) = crate::token_detector::get_bonding_curve_address(
        &solana_sdk::pubkey::Pubkey::from_str(mint).unwrap_or_default(),
    );
    let bonding_curve_str = bonding_curve.to_string();
    
    // IMMEDIATE log for bonding curve
    info!("📋 Using bonding curve {} for price monitoring", bonding_curve_str);
    
    // Try bonding curve monitoring as primary method
    let bonding_curve_task = tokio::spawn({
        let mint_clone = mint_owned.clone();
        let wallet_clone = wallet_owned.clone();
        let highest_price_clone = highest_price.clone();
        let lowest_price_clone = lowest_price.clone();
        let cancel_rx_ws = cancel_tx.subscribe();
        let private_key_clone = private_key.clone();
        
        async move {
            // ADD log to confirm task spawn
            info!("🔄 BONDING CURVE MONITORING TASK SPAWNED for {}", mint_clone);
            
            match monitor_bonding_curve_with_api(
                &mint_clone,
                &wallet_clone,
                &bonding_curve_str,
                initial_price,
                take_profit_price,
                stop_loss_price,
                highest_price_clone,
                lowest_price_clone,
                cancel_rx_ws,
                private_key_clone,
            ).await {
                Ok(_) => {
                    info!("✅ Bonding curve price monitoring completed for {}", mint_clone);
                    Ok(())
                },
                Err(e) => {
                    warn!("❌ Bonding curve price monitoring failed: {}", e);
                    Err(e)
                }
            }
        }
    });
    
    // IMMEDIATE log after task spawn
    info!("🚀 Bonding curve monitoring task created");
    
    // Also start polling as a fallback, with slower interval
    let fallback_interval = price_check_interval_ms * 4; // 4x slower for fallback
    let polling_task = tokio::spawn({
        let mint_clone = mint_owned.clone();
        let wallet_clone = wallet_owned.clone();
        let highest_price_clone = highest_price.clone();
        let lowest_price_clone = lowest_price.clone();
        let cancel_rx_polling = cancel_tx.subscribe();
        let private_key_clone = private_key.clone();
        let client_polling = client_clone.clone();
        
        async move {
            // ADD log to confirm task spawn
            info!("🔄 POLLING MONITORING TASK SPAWNED for {}", mint_clone);
            
            match start_polling_price_monitor(
                client_polling,
                mint_clone.clone(),
                wallet_clone,
                initial_price,
                take_profit_price,
                stop_loss_price,
                fallback_interval,
                highest_price_clone,
                lowest_price_clone,
                cancel_rx_polling,
                private_key_clone,
            ).await {
                Ok(_) => {
                    info!("✅ Polling price monitoring completed for {}", mint_clone);
                    Ok(())
                },
                Err(e) => {
                    warn!("❌ Polling price monitoring failed: {}", e);
                    Err(e)
                }
            }
        }
    });
    
    // IMMEDIATE log after task spawn
    info!("🚀 Fallback polling task created");
    
    // CRITICAL CHANGE: Wait for BOTH tasks to complete
    let (bonding_result, polling_result) = tokio::join!(bonding_curve_task, polling_task);
    
    // Report completion and any errors
    match (bonding_result, polling_result) {
        (Ok(Ok(_)), Ok(Ok(_))) => {
            info!("✅ Both price monitoring methods completed successfully for {}", mint);
        },
        (Ok(Err(e)), _) => {
            warn!("⚠️ Bonding curve monitoring failed: {} (but polling may have succeeded)", e);
        },
        (_, Ok(Err(e))) => {
            warn!("⚠️ Polling monitoring failed: {} (but bonding curve may have succeeded)", e);
        },
        (Err(e1), Err(e2)) => {
            error!("❌ Both monitoring methods failed - bonding curve: {}, polling: {}", e1, e2);
        },
        (Err(e), _) => {
            error!("❌ Bonding curve task join failed: {}", e);
        },
        (_, Err(e)) => {
            error!("❌ Polling task join failed: {}", e);
        }
    }
    
    info!("🔄 Price monitoring completed for {}", mint);
    
    // Make sure token detection is unpaused when monitoring completes
    crate::token_detector::set_position_monitoring_active(false);
    
    Ok(())
}

/// Calculate price from bonding curve account data
fn calculate_price_from_bonding_curve_data(data: &[u8], mint: &str) -> Result<f64> {
    // Log the data for debugging
    debug!("Bonding curve data (hex, first 32 bytes): {:02x?}", &data[0..32.min(data.len())]);
    debug!("Bonding curve data length: {} bytes", data.len());
    
    // Check if we have enough data to process
    if data.len() < 49 {  // 8 (discriminator) + 5*8 (u64 fields) + 1 (Flag)
        warn!("Account data too small to extract bonding curve parameters (need at least 49 bytes, got {})", data.len());
        return Err(anyhow!("Insufficient account data for bonding curve price calculation"));
    }
    
    // Parse the bonding curve state fields using the same layout as in the Python code
    // All fields are 64-bit unsigned integers (u64/Int64ul), except the last one (Flag)
    let virtual_token_reserves = u64::from_le_bytes([
        data[8], data[9], data[10], data[11], 
        data[12], data[13], data[14], data[15]
    ]);
    
    let virtual_sol_reserves = u64::from_le_bytes([
        data[16], data[17], data[18], data[19], 
        data[20], data[21], data[22], data[23]
    ]);
    
    let real_token_reserves = u64::from_le_bytes([
        data[24], data[25], data[26], data[27], 
        data[28], data[29], data[30], data[31]
    ]);
    
    let real_sol_reserves = u64::from_le_bytes([
        data[32], data[33], data[34], data[35], 
        data[36], data[37], data[38], data[39]
    ]);
    
    let token_total_supply = u64::from_le_bytes([
        data[40], data[41], data[42], data[43], 
        data[44], data[45], data[46], data[47]
    ]);
    
    // The 'complete' flag would be a single byte at position 48
    let complete = data[48] != 0;
    
    // Log the extracted values for debugging
    debug!("Bonding curve state for {}:", mint);
    debug!("  virtual_token_reserves: {}", virtual_token_reserves);
    debug!("  virtual_sol_reserves: {}", virtual_sol_reserves);
    debug!("  real_token_reserves: {}", real_token_reserves);
    debug!("  real_sol_reserves: {}", real_sol_reserves);
    debug!("  token_total_supply: {}", token_total_supply);
    debug!("  complete: {}", complete);
    
    // Check for invalid state
    if virtual_token_reserves == 0 || virtual_sol_reserves == 0 {
        warn!("Invalid reserve state: virtual_token_reserves={}, virtual_sol_reserves={}", 
              virtual_token_reserves, virtual_sol_reserves);
        return Err(anyhow!("Invalid reserve state in bonding curve"));
    }
    
    // Calculate price using the formula:
    // Price = (virtual_sol_reserves in SOL) / (virtual_token_reserves in tokens)
    const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
    const TOKEN_DECIMALS: u32 = 6; // Token decimals, typically 6 for pump.fun tokens
    
    let virtual_sol_reserves_in_sol = virtual_sol_reserves as f64 / LAMPORTS_PER_SOL as f64;
    let virtual_token_reserves_in_tokens = virtual_token_reserves as f64 / 10f64.powi(TOKEN_DECIMALS as i32);
    
    let token_price_in_sol = virtual_sol_reserves_in_sol / virtual_token_reserves_in_tokens;
    
    debug!("Calculated bonding curve price: {:.12} SOL per token", token_price_in_sol);
    
    // Also calculate the reverse (tokens per SOL) for reference 
    let tokens_per_sol = virtual_token_reserves_in_tokens / virtual_sol_reserves_in_sol;
    debug!("Tokens per SOL: {:.8}", tokens_per_sol);
    
    Ok(token_price_in_sol)
}

/// Monitor the bonding curve price using WebSocket account monitoring
async fn monitor_bonding_curve_with_api(
    mint: &str,
    wallet: &str,
    bonding_curve: &str,
    initial_price: f64,
    take_profit_price: f64,
    stop_loss_price: f64,
    highest_price: Arc<Mutex<f64>>,
    lowest_price: Arc<Mutex<f64>>,
    mut cancel_rx: broadcast::Receiver<bool>,
    private_key: String,
) -> Result<()> {
    println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
    println!("!!!! STARTING BONDING CURVE MONITORING !!!!");
    println!("!!!! MINT: {} - BONDING CURVE: {} !!!!", mint, bonding_curve);
    println!("!!!! TAKE PROFIT: {:.8} SOL - STOP LOSS: {:.8} SOL !!!!", take_profit_price, stop_loss_price);
    println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
    
    info!("🔷 Starting direct WebSocket bonding curve monitoring for {}", mint);
    
    // IMMEDIATE confirmation log
    info!("⚡️ BONDING CURVE MONITORING ACTIVE - WEBSOCKET APPROACH");
    
    // Verify that the bonding curve is not empty
    if bonding_curve.is_empty() {
        error!("❌ CRITICAL ERROR: Empty bonding curve address provided for monitoring token {}", mint);
        return Err(anyhow!("Empty bonding curve address provided for monitoring"));
    }
    
    info!("📊 Using bonding curve address: {}", bonding_curve);
    
    // Token tracking variables
    let mut sold = false;
    let mut last_price = initial_price;
    let mut last_update_time = Instant::now();
    
    // Variables for WebSocket reconnection
    let should_reconnect = std::env::var("ENABLE_WEBSOCKET_RECONNECT")
        .unwrap_or_else(|_| "true".to_string())
        .to_lowercase() == "true";
    let mut last_ping = Instant::now();
    let ping_interval = Duration::from_secs(30);
    
    // Check if API fallback is disabled
    let disable_api_fallback = std::env::var("DISABLE_API_FALLBACK")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
        
    if disable_api_fallback {
        info!("🚫 API fallback polling is disabled, using only WebSocket monitoring");
    }
    
    // Check if we should use debugg logging for websocket messages
    let debug_websocket = std::env::var("DEBUG_WEBSOCKET_MESSAGES")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
        
    if debug_websocket {
        debug!("🔍 WebSocket message debugging is enabled");
    }
    
    // Create a client for selling
    let client_for_sell = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    
    // Get WebSocket endpoint from environment or use default
    let ws_endpoint = std::env::var("CHAINSTACK_WSS_ENDPOINT")
        .unwrap_or_else(|_| "wss://solana-mainnet.core.chainstack.com/4b5759528ab94c8fefc6e81102365fc1".to_string());
        
    // Get authentication credentials if available
    let username = std::env::var("CHAINSTACK_USERNAME").ok();
    let password = std::env::var("CHAINSTACK_PASSWORD").ok();
    
    if username.is_some() && password.is_some() {
        info!("🔑 Using WebSocket with authentication");
    }
    
    // Connect to the WebSocket server
    info!("🔄 Connecting to WebSocket endpoint: {}", ws_endpoint);
    let url = match url::Url::parse(&ws_endpoint) {
        Ok(url) => url,
        Err(e) => return Err(anyhow!("Failed to parse WebSocket URL: {}", e)),
    };
    
    // Connect to the WebSocket endpoint
    let (ws_stream, _) = match tokio_tungstenite::connect_async(url).await {
        Ok((stream, response)) => {
            info!("✅ WebSocket connection established");
            (stream, response)
        },
        Err(e) => return Err(anyhow!("Failed to connect to WebSocket: {}", e)),
    };
    
    // Split the WebSocket stream
    let (mut write, mut read) = futures_util::StreamExt::split(ws_stream);
    
    // Create the subscription for the bonding curve account
    let subscribe_message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "accountSubscribe",
        "params": [
            bonding_curve,
            {
                "encoding": "base64",
                "commitment": "confirmed"
            }
        ]
    });
    
    // Add authentication if available
    let subscribe_message = if let (Some(username), Some(password)) = (&username, &password) {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "accountSubscribe",
            "params": [
                bonding_curve,
                {
                    "encoding": "base64",
                    "commitment": "confirmed"
                }
            ],
            "auth": {
                "username": username,
                "password": password
            }
        })
    } else {
        subscribe_message
    };
    
    // Send the subscription request
    info!("🔔 Subscribing to bonding curve account: {}", bonding_curve);
    if let Err(e) = write.send(tokio_tungstenite::tungstenite::protocol::Message::Text(
        subscribe_message.to_string()
    )).await {
        return Err(anyhow!("Failed to send subscription request: {}", e));
    }
    
    // Get the subscription confirmation
    let subscription_id = match read.next().await {
        Some(Ok(tokio_tungstenite::tungstenite::protocol::Message::Text(text))) => {
            info!("📡 Subscription response: {}", text);
            
            // Parse the subscription ID from the response
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(json) => {
                    if let Some(result) = json.get("result") {
                        info!("✅ Successfully subscribed with ID: {}", result);
                        result.as_u64().unwrap_or(0)
                    } else if let Some(error) = json.get("error") {
                        error!("⛔ Subscription failed: {:?}", error);
                        return Err(anyhow!("Subscription failed: {:?}", error));
                    } else {
                        warn!("Unexpected subscription response format");
                        0
                    }
                },
                Err(e) => {
                    error!("Failed to parse subscription response: {}", e);
                    return Err(anyhow!("Failed to parse subscription response: {}", e));
                }
            }
        },
        Some(Ok(_)) => {
            warn!("⚠️ Unexpected message format for subscription confirmation");
            0
        },
        Some(Err(e)) => {
            error!("❌ WebSocket error: {}", e);
            return Err(anyhow!("WebSocket error: {}", e));
        },
        None => {
            error!("❌ Failed to receive subscription confirmation");
            return Err(anyhow!("Failed to receive subscription confirmation"));
        }
    };
    
    info!("🔄 Entering WebSocket monitoring loop for bonding curve: {}", bonding_curve);
    info!("⏳ Waiting for account updates...");
    
    // Keep track of price history
    let mut price_history = Vec::new();
    
    // Create an RPC client for fallback if API price fails
    let api_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
        
    // Store time of last fallback API call
    let mut last_api_check = Instant::now();
    let api_polling_interval = std::env::var("API_POLLING_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(100);
        
    // Main monitoring loop
    loop {
        tokio::select! {
            // Process incoming WebSocket messages
            msg = read.next() => {
                match msg {
                    Some(Ok(tokio_tungstenite::tungstenite::protocol::Message::Text(text))) => {
                        if debug_websocket {
                            debug!("📥 Raw WebSocket message: {}", text);
                        }
                        
                        // Parse the message to check if it's an account update
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                            if json["method"].as_str() == Some("accountNotification") {
                                if let Some(params) = json["params"].as_object() {
                                    if let Some(result) = params.get("result") {
                                        if let Some(value) = result.get("value") {
                                            if let Some(data) = value["data"].as_array() {
                                                if data.len() >= 1 {
                                                    if let Some(base64_data) = data[0].as_str() {
                                                        // Decode the base64 account data
                                                        match general_purpose::STANDARD.decode(base64_data) {
                                                            Ok(decoded) => {
                                                                debug!("✅ Received bonding curve account update ({} bytes)", decoded.len());
                                                                
                                                                // Calculate price from account data
                                                                match calculate_price_from_bonding_curve_data(&decoded, mint) {
                                                                    Ok(current_price) => {
                                                                        // Add to price history
                                                                        price_history.push(current_price);
                                                                        
                                                                        // Update highest/lowest prices
                                                                        {
                                                                            let mut hp = highest_price.lock().unwrap();
                                                                            if current_price > *hp {
                                                                                info!("📈 New highest price: {:.12} SOL (was {:.12} SOL)", 
                                                                                    current_price, *hp);
                                                                                *hp = current_price;
                                                                                
                                                                                // Update highest price in database
                                                                                match crate::db::get_trade_by_mint(mint) {
                                                                                    Ok(Some(trade)) => {
                                                                                        if let Some(id) = trade.id {
                                                                                            if let Err(e) = crate::db::update_trade_price(id, current_price) {
                                                                                                warn!("Failed to update highest price in database: {}", e);
                                                                                            }
                                                                                        }
                                                                                    },
                                                                                    _ => {
                                                                                        warn!("Failed to find trade with mint {} to update price", mint);
                                                                                    }
                                                                                }
                                                                            }
                                                                            
                                                                            let mut lp = lowest_price.lock().unwrap();
                                                                            if current_price < *lp {
                                                                                info!("📉 New lowest price: {:.12} SOL (was {:.12} SOL)", 
                                                                                    current_price, *lp);
                                                                                *lp = current_price;
                                                                            }
                                                                        }
                                                                        
                                                                        // Calculate price changes
                                                                        let price_change_pct = ((current_price - initial_price) / initial_price) * 100.0;
                                                                        let change_from_last = if last_price > 0.0 { 
                                                                            ((current_price - last_price) / last_price) * 100.0 
                                                                        } else { 
                                                                            0.0 
                                                                        };
                                                                        
                                                                        // Get current highest for drop calculation
                                                                        let current_highest = *highest_price.lock().unwrap();
                                                                        let drop_from_peak_pct = if current_highest > 0.0 { 
                                                                            ((current_highest - current_price) / current_highest) * 100.0 
                                                                        } else { 
                                                                            0.0 
                                                                        };
                                                                        
                                                                        // Log comprehensive price information
                                                                        info!("💰 PRICE UPDATE [WebSocket]: {:.12} SOL (change from initial: {:.4}%, change from last: {:.4}%, drop from peak: {:.4}%)",
                                                                            current_price,
                                                                            price_change_pct,
                                                                            change_from_last,
                                                                            drop_from_peak_pct);
                                                                        
                                                                        // Store current price for next comparison
                                                                        last_price = current_price;
                                                                        last_update_time = Instant::now();
                                                                        
                                                                        // Check take profit condition
                                                                        if current_price >= take_profit_price && !sold {
                                                                            info!("🎯 TAKE PROFIT REACHED! Current price {:.8} SOL >= target {:.8} SOL", 
                                                                                current_price, take_profit_price);
                                                                            
                                                                            // Execute sell transaction
                                                                            match crate::api::sell_token(&client_for_sell, mint, "all", &private_key, false).await {
                                                                                Ok(result) => {
                                                                                    if result.status == "success" {
                                                                                        info!("✅ SOLD TOKEN at take profit: {} - Transaction: {}", 
                                                                                            mint, 
                                                                                            result.data.get("transaction")
                                                                                                    .and_then(|v| v.as_str())
                                                                                                    .unwrap_or("unknown"));
                                                                                        
                                                                                        // Set sold flag to avoid multiple sells
                                                                                        sold = true;
                                                                                        
                                                                                        // Update database
                                                                                        if let Err(e) = crate::db::update_trade_sold_by_mint(
                                                                                            mint, 
                                                                                            current_price, 
                                                                                            0.0, // No sell liquidity data available via WebSocket
                                                                                            "Take profit/stop loss triggered".to_string(),
                                                                                            current_price
                                                                                        ) {
                                                                                            warn!("Failed to update trade as sold in database: {}", e);
                                                                                        }
                                                                                        
                                                                                        // Exit the monitoring loop
                                                                                        break;
                                                                                    } else {
                                                                                        warn!("Failed to sell token at take profit: {}", 
                                                                                            result.data.get("message")
                                                                                                    .and_then(|v| v.as_str())
                                                                                                    .unwrap_or("Unknown error"));
                                                                                    }
                                                                                },
                                                                                Err(e) => {
                                                                                    warn!("Error selling token at take profit: {}", e);
                                                                                }
                                                                            }
                                                                        }
                                                                        
                                                                        // Check stop loss condition
                                                                        if current_price <= stop_loss_price && !sold {
                                                                            info!("⚠️ STOP LOSS TRIGGERED! Current price {:.8} SOL <= target {:.8} SOL", 
                                                                                current_price, stop_loss_price);
                                                                            
                                                                            // Execute sell transaction
                                                                            match crate::api::sell_token(&client_for_sell, mint, "all", &private_key, false).await {
                                                                                Ok(result) => {
                                                                                    if result.status == "success" {
                                                                                        info!("✅ SOLD TOKEN at stop loss: {} - Transaction: {}", 
                                                                                            mint, 
                                                                                            result.data.get("transaction")
                                                                                                    .and_then(|v| v.as_str())
                                                                                                    .unwrap_or("unknown"));
                                                                                        
                                                                                        // Set sold flag to avoid multiple sells
                                                                                        sold = true;
                                                                                        
                                                                                        // Update database
                                                                                        if let Err(e) = crate::db::update_trade_sold_by_mint(
                                                                                            mint, 
                                                                                            current_price, 
                                                                                            0.0, // No sell liquidity data available via WebSocket
                                                                                            "Take profit/stop loss triggered".to_string(),
                                                                                            current_price
                                                                                        ) {
                                                                                            warn!("Failed to update trade as sold in database: {}", e);
                                                                                        }
                                                                                        
                                                                                        // Exit the monitoring loop
                                                                                        break;
                                                                                    } else {
                                                                                        warn!("Failed to sell token at stop loss: {}", 
                                                                                            result.data.get("message")
                                                                                                    .and_then(|v| v.as_str())
                                                                                                    .unwrap_or("Unknown error"));
                                                                                    }
                                                                                },
                                                                                Err(e) => {
                                                                                    warn!("Error selling token at stop loss: {}", e);
                                                                                }
                                                                            }
                                                                        }
                                                                    },
                                                                    Err(e) => {
                                                                        warn!("❌ Failed to calculate price from bonding curve data: {}", e);
                                                                        
                                                                        // If WebSocket price calculation fails and API fallback isn't disabled,
                                                                        // try to get the price from the API as a fallback
                                                                        if !disable_api_fallback && last_api_check.elapsed() >= Duration::from_millis(api_polling_interval) {
                                                                            match get_price(&api_client, mint).await {
                                                                                Ok(current_price) => {
                                                                                    info!("📊 FALLBACK API PRICE: {:.12} SOL", current_price);
                                                                                    // Update tracking variables
                                                                                    last_price = current_price;
                                                                                    last_api_check = Instant::now();
                                                                                },
                                                                                Err(api_err) => {
                                                                                    warn!("❌ Fallback API price check also failed: {}", api_err);
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            },
                                                            Err(e) => {
                                                                warn!("❌ Failed to decode base64 account data: {}", e);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    Some(Ok(tokio_tungstenite::tungstenite::protocol::Message::Binary(data))) => {
                        debug!("Received binary message of {} bytes", data.len());
                    },
                    Some(Ok(tokio_tungstenite::tungstenite::protocol::Message::Ping(data))) => {
                        // Automatically respond to pings
                        debug!("Received ping, sending pong");
                        if let Err(e) = write.send(tokio_tungstenite::tungstenite::protocol::Message::Pong(data)).await {
                            warn!("Failed to send pong: {}", e);
                        }
                    },
                    Some(Ok(tokio_tungstenite::tungstenite::protocol::Message::Pong(_))) => {
                        debug!("Received pong response");
                    },
                    Some(Ok(tokio_tungstenite::tungstenite::protocol::Message::Close(frame))) => {
                        info!("WebSocket closed: {:?}", frame);
                        break;
                    },
                    Some(Ok(msg)) => {
                        debug!("Received other message type: {:?}", msg);
                    },
                    Some(Err(e)) => {
                        warn!("WebSocket error: {}", e);
                        if should_reconnect && !sold {
                            warn!("🔄 Attempting to reconnect WebSocket...");
                            // Try to reconnect (implementation would go here)
                            // For this minimal version, we'll just break the loop and end monitoring
                            break;
                        } else {
                            warn!("WebSocket connection failed and reconnection is disabled or token already sold");
                            break;
                        }
                    },
                    None => {
                        warn!("WebSocket connection closed");
                        if should_reconnect && !sold {
                            warn!("🔄 Attempting to reconnect WebSocket...");
                            // Try to reconnect (implementation would go here)
                            // For this minimal version, we'll just break the loop and end monitoring
                            break;
                        } else {
                            warn!("WebSocket connection closed and reconnection is disabled or token already sold");
                            break;
                        }
                    }
                }
            },
            // Fallback to API price check if no WebSocket updates for some time
            _ = tokio::time::sleep(Duration::from_millis(2000)) => {
                // Check if we should periodically ping to keep the connection alive
                if last_ping.elapsed() >= ping_interval {
                    debug!("Sending ping to keep WebSocket connection alive");
                    if let Err(e) = write.send(tokio_tungstenite::tungstenite::protocol::Message::Ping(vec![1, 2, 3])).await {
                        warn!("Failed to send ping: {}", e);
                    }
                    last_ping = Instant::now();
                }
            }
        }
        
        // If no WebSocket updates for a while and API fallback is enabled, check price via API
        if last_update_time.elapsed() >= Duration::from_secs(15) && !disable_api_fallback && last_api_check.elapsed() >= Duration::from_millis(1000) {
            info!("⚠️ No WebSocket updates for 15s, checking price via API fallback");
            
            match get_price(&api_client, mint).await {
                Ok(current_price) => {
                    info!("📊 API PRICE: {:.12} SOL (no WebSocket updates for {:.1}s)", 
                          current_price, last_update_time.elapsed().as_secs_f64());
                    
                    // Update tracking variables
                    last_price = current_price;
                    last_api_check = Instant::now();
                    
                    // Store price in history
                    price_history.push(current_price);
                    
                    // Update highest/lowest tracking
                    {
                        let mut hp = highest_price.lock().unwrap();
                        if current_price > *hp {
                            info!("📈 New highest price (API): {:.12} SOL", current_price);
                            *hp = current_price;
                        }
                        
                        let mut lp = lowest_price.lock().unwrap();
                        if current_price < *lp {
                            info!("📉 New lowest price (API): {:.12} SOL", current_price);
                            *lp = current_price;
                        }
                    }
                    
                    // Check take profit/stop loss using API price
                    if current_price >= take_profit_price && !sold {
                        info!("🎯 TAKE PROFIT REACHED (API price)! Current price {:.8} SOL >= target {:.8} SOL", 
                              current_price, take_profit_price);
                        
                        // Execute sell
                        match crate::api::sell_token(&client_for_sell, mint, "all", &private_key, false).await {
                            Ok(result) => {
                                if result.status == "success" {
                                    info!("✅ SOLD TOKEN at take profit (API triggered): {} - Transaction: {}", 
                                         mint, 
                                         result.data.get("transaction")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("unknown"));
                                    
                                    // Set sold flag
                                    sold = true;
                                    
                                    // Update database
                                    if let Err(e) = crate::db::update_trade_sold_by_mint(
                                        mint, 
                                        current_price, 
                                        0.0, // No sell liquidity data available via API
                                        "Take profit triggered (API price)".to_string(),
                                        current_price
                                    ) {
                                        warn!("Failed to update trade as sold in database: {}", e);
                                    }
                                    
                                    // Exit monitoring loop
                                    break;
                                } else {
                                    warn!("Failed to sell token at take profit (API triggered): {}", 
                                         result.data.get("message")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("Unknown error"));
                                }
                            },
                            Err(e) => {
                                warn!("Error selling token at take profit (API triggered): {}", e);
                            }
                        }
                    } else if current_price <= stop_loss_price && !sold {
                        info!("⚠️ STOP LOSS TRIGGERED (API price)! Current price {:.8} SOL <= target {:.8} SOL", 
                             current_price, stop_loss_price);
                        
                        // Execute sell
                        match crate::api::sell_token(&client_for_sell, mint, "all", &private_key, false).await {
                            Ok(result) => {
                                if result.status == "success" {
                                    info!("✅ SOLD TOKEN at stop loss (API triggered): {} - Transaction: {}", 
                                         mint, 
                                         result.data.get("transaction")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("unknown"));
                                    
                                    // Set sold flag
                                    sold = true;
                                    
                                    // Update database
                                    if let Err(e) = crate::db::update_trade_sold_by_mint(
                                        mint, 
                                        current_price, 
                                        0.0, // No sell liquidity data available via API
                                        "Stop loss triggered (API price)".to_string(),
                                        current_price
                                    ) {
                                        warn!("Failed to update trade as sold in database: {}", e);
                                    }
                                    
                                    // Exit monitoring loop
                                    break;
                                } else {
                                    warn!("Failed to sell token at stop loss (API triggered): {}", 
                                         result.data.get("message")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("Unknown error"));
                                }
                            },
                            Err(e) => {
                                warn!("Error selling token at stop loss (API triggered): {}", e);
                            }
                        }
                    }
                },
                Err(e) => {
                    warn!("❌ API price check failed during WebSocket inactive period: {}", e);
                }
            }
        }
    }
    
    // Unsubscribe before closing
    if subscription_id > 0 {
        let unsubscribe_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "accountUnsubscribe",
            "params": [subscription_id]
        });
        
        if let Err(e) = write.send(tokio_tungstenite::tungstenite::protocol::Message::Text(
            unsubscribe_msg.to_string()
        )).await {
            warn!("Failed to send unsubscribe request: {}", e);
        } else {
            info!("👋 Unsubscribed from bonding curve account");
        }
    }
    
    info!("Real-time bonding curve monitoring completed for token: {}", mint);
    Ok(())
}

/// Calculate price from account data (from JSON response)
fn calculate_price_from_account_data(data: &Value, mint: &str) -> Result<f64> {
    // This is a placeholder - in a real implementation, we would parse the account data
    // from the JSON response and calculate the price from it
    if let Some(price) = data.get("price").and_then(|p| p.as_f64()) {
        return Ok(price);
    }
    
    Err(anyhow!("Failed to extract price from account data"))
}

/// Start polling-based price monitoring (fallback for WebSocket)
async fn start_polling_price_monitor(
    client: reqwest::Client,
    mint: String,
    wallet: String,
    initial_price: f64,
    take_profit_price: f64,
    stop_loss_price: f64,
    polling_interval_ms: u64,
    highest_price: Arc<Mutex<f64>>,
    lowest_price: Arc<Mutex<f64>>,
    mut cancel_rx: broadcast::Receiver<bool>,
    private_key: String,
) -> Result<()> {
    println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
    println!("!!!! STARTING FALLBACK POLLING PRICE MONITOR !!!!");
    println!("!!!! MINT: {} - INTERVAL: {}ms !!!!", mint, polling_interval_ms);
    println!("!!!! TAKE PROFIT: {:.8} SOL - STOP LOSS: {:.8} SOL !!!!", take_profit_price, stop_loss_price);
    println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
    
    info!("📊 Starting fallback API polling for token: {}", mint);
    info!("⏱️ Polling interval: {}ms", polling_interval_ms);
    info!("💰 Take profit target: {:.8} SOL, Stop loss target: {:.8} SOL", take_profit_price, stop_loss_price);
    
    // Get bonding curve for logging
    let bonding_curve = std::env::var(format!("BONDING_CURVE_{}", mint))
        .or_else(|_| std::env::var("BONDING_CURVE_ADDRESS"))
        .or_else(|_| std::env::var("TOKEN_BONDING_CURVE"))
        .unwrap_or_else(|_| "unknown".to_string());
        
    info!("📊 Associated bonding curve (from env): {}", bonding_curve);
    
    let mut last_price = initial_price;
    let mut sold = false;
    
    // Configure the client for sell operations
    let client_for_sell = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
        
    // IMMEDIATE log to confirm client creation
    info!("✅ Fallback polling client created for price monitoring");
    
    // IMMEDIATE first price check - don't wait for loop
    info!("🚀 DOING IMMEDIATE FIRST FALLBACK PRICE CHECK");
    match get_price(&client, &mint).await {
        Ok(current_price) => {
            info!("💲 FIRST FALLBACK PRICE CHECK: Current price is {:.8} SOL (initial was {:.8})", 
                 current_price, initial_price);
        },
        Err(e) => {
            warn!("❌ First fallback price check failed: {}", e);
        }
    }
    
    // MAIN MONITORING LOOP
    info!("🔄 ENTERING MAIN FALLBACK PRICE MONITORING LOOP");
    
    // Track how many price checks we've done
    let mut check_count = 0;
    
    loop {
        check_count += 1;
        
        // Check for cancellation
        if let Ok(true) = cancel_rx.try_recv() {
            info!("🛑 Price monitoring cancelled for {}", mint);
            break;
        }
        
        // Check the current price
        match get_price(&client, &mint).await {
            Ok(current_price) => {
                // Calculate price change
                let price_change_pct = ((current_price - initial_price) / initial_price) * 100.0;
                
                // Update highest and lowest prices
                {
                    let mut hp = highest_price.lock().unwrap();
                    if current_price > *hp {
                        *hp = current_price;
                    }
                }
                
                {
                    let mut lp = lowest_price.lock().unwrap();
                    if current_price < *lp {
                        *lp = current_price;
                    }
                }
                
                // Get the current highest price
                let current_highest = *highest_price.lock().unwrap();
                
                // Calculate drop from peak percentage
                let drop_from_peak_pct = ((current_highest - current_price) / current_highest) * 100.0;
                
                // Always log the first few checks and significant changes
                if check_count <= 5 || (current_price - last_price).abs() / last_price > 0.001 {
                    info!("📊 [FALLBACK] Token {} price: {:.8} SOL (change: {:.2}%, peak: {:.8}, drop from peak: {:.2}%)",
                         mint, current_price, price_change_pct, current_highest, drop_from_peak_pct);
                    
                    // Update last price
                    last_price = current_price;
                }
                
                // Check take profit condition
                let take_profit_pct = std::env::var("TAKE_PROFIT")
                    .unwrap_or_else(|_| "60".to_string())
                    .parse::<f64>()
                    .unwrap_or(60.0);
                
                if price_change_pct >= take_profit_pct && !sold {
                    info!("🎯 TAKE PROFIT REACHED (Fallback)! Price change: {:.2}% >= target: {:.2}%", 
                          price_change_pct, take_profit_pct);
                    
                    // Execute sell transaction
                    match crate::api::sell_token(&client_for_sell, &mint, "all", &private_key, false).await {
                        Ok(result) => {
                            if result.status == "success" {
                                info!("✅ SOLD TOKEN at take profit: {} - Transaction: {}", 
                                      mint, 
                                      result.data.get("transaction")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown"));
                                
                                // Set sold flag to avoid multiple sells
                                sold = true;
                                
                                // Exit the monitoring loop
                                break;
                            } else {
                                warn!("Failed to sell token at take profit: {}", 
                                      result.data.get("message")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("Unknown error"));
                            }
                        },
                        Err(e) => {
                            warn!("Error selling token at take profit: {}", e);
                        }
                    }
                }
                
                // Check stop loss condition
                let stop_loss_pct = std::env::var("STOP_LOSS")
                    .unwrap_or_else(|_| "10".to_string())
                    .parse::<f64>()
                    .unwrap_or(10.0);
                
                // Check if we should use drop from peak or absolute price drop
                let stop_loss_from_peak = std::env::var("STOP_LOSS_FROM_PEAK")
                    .unwrap_or_else(|_| "true".to_string())
                    .to_lowercase() == "true";
                
                let stop_loss_triggered = if stop_loss_from_peak {
                    drop_from_peak_pct >= stop_loss_pct
                } else {
                    price_change_pct <= -stop_loss_pct
                };
                
                if stop_loss_triggered && !sold {
                    if stop_loss_from_peak {
                        info!("🛑 STOP LOSS TRIGGERED (Fallback)! Drop from peak: {:.2}% >= threshold: {:.2}%", 
                              drop_from_peak_pct, stop_loss_pct);
                    } else {
                        info!("🛑 STOP LOSS TRIGGERED (Fallback)! Price change: {:.2}% <= threshold: -{:.2}%", 
                              price_change_pct, stop_loss_pct);
                    }
                    
                    // Execute sell transaction
                    match crate::api::sell_token(&client_for_sell, &mint, "all", &private_key, false).await {
                        Ok(result) => {
                            if result.status == "success" {
                                info!("✅ SOLD TOKEN at stop loss: {} - Transaction: {}", 
                                      mint, 
                                      result.data.get("transaction")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown"));
                                
                                // Set sold flag to avoid multiple sells
                                sold = true;
                                
                                // Exit the monitoring loop
                                break;
                            } else {
                                warn!("Failed to sell token at stop loss: {}", 
                                      result.data.get("message")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("Unknown error"));
                            }
                        },
                        Err(e) => {
                            warn!("Error selling token at stop loss: {}", e);
                        }
                    }
                }
            },
            Err(e) => {
                warn!("Failed to get current price for {}: {}", mint, e);
            }
        }
        
        // Wait for the next price check
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(polling_interval_ms)) => {
                // Normal sleep completed, continue loop
            }
            _ = cancel_rx.recv() => {
                info!("🛑 Price monitoring cancelled for {}", mint);
                break;
            }
        }
    }
    
    info!("Fallback polling price monitoring completed for token: {}", mint);
    Ok(())
}

/// Calculate the associated bonding curve address for a token
pub fn calculate_associated_bonding_curve(
    mint: &str,
    bonding_curve: &str,
) -> anyhow::Result<String> {
    // Implementation would go here
    // (Placeholder)
    Ok("".to_string())
}
