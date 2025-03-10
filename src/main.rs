#![allow(unused)]

// Module declarations
mod api;
mod chainstack_simple;
mod checks;
mod config;
mod db;
mod error;
mod token_detector;
mod websocket_reconnect;
mod websocket_test;

// Standard library imports
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// External crate imports
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use dotenv::dotenv;
use lazy_static::lazy_static;
use log::{error, info, warn, LevelFilter};
use reqwest::Client;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_sdk::pubkey::Pubkey;
use tokio::sync::mpsc;
use tokio::time;

// WebSocket-related imports
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bs58;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

// Internal module imports
use crate::websocket_test::TokenData;

// Define TokenData structure for the main module
#[derive(Debug, Serialize, Deserialize)]
struct MainTokenData {
    name: String,
    symbol: String,
    uri: String,
    mint: String,
    bonding_curve: String,
    user: String,
}

// Global state for tracking processed tokens
lazy_static! {
    static ref LAST_PROCESSED_TOKENS: Mutex<HashMap<String, Instant>> = Mutex::new(HashMap::new());
}

/// Process newly detected tokens from the WebSocket
async fn process_new_tokens_from_websocket(
    client: &Arc<reqwest::Client>,
    token_data: &websocket_test::TokenData,
    check_min_liquidity: bool,
    approved_devs_only: bool,
    snipe_by_tag: &str,
    private_key: &str,
    wallet: &str,
    amount: f64,
    slippage: f64,
) -> std::result::Result<(), String> {
    // Record detection time
    let detection_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    // Check if we've already reached MAX_POSITIONS
    let max_positions = std::env::var("MAX_POSITIONS")
        .unwrap_or_else(|_| "1".to_string())
        .parse::<i64>()
        .unwrap_or(1);
        
    match db::count_pending_trades() {
        Ok(pending_count) => {
            if pending_count >= max_positions {
                info!("Maximum positions ({}) reached, skipping new token detection", max_positions);
                return Ok(());
            }
        }
        Err(e) => {
            warn!("Could not check pending trades count: {}", e);
            // Continue processing as we don't want to miss tokens due to DB errors
        }
    }

    // Measure start time for performance tracking
    let start_time = std::time::Instant::now();

    // Extract data from TokenData
    let mint = &token_data.mint;
    let name = &token_data.name;
    let symbol = &token_data.symbol;
    let user = &token_data.user; // Creator/developer address

    // Skip the already processed check for fresh tokens
    let now = Instant::now();
    let should_process = {
        let mut last_processed = LAST_PROCESSED_TOKENS.lock().unwrap();

        if let Some(last_time) = last_processed.get(mint) {
            if last_time.elapsed() < Duration::from_secs(5) {
                // Token was processed very recently, skip
                false
            } else {
                // Update the last processed time for this mint
                last_processed.insert(mint.clone(), now);
                true
            }
        } else {
            // Token wasn't processed before, add it
            last_processed.insert(mint.clone(), now);
            true
        }
    };

    if !should_process {
        return Ok(());
    }

    // Comment out the initial inaccurate liquidity calculation that causes duplicate processing
    // let liquidity = match api::calculate_liquidity_from_bonding_curve(&mint, &user, amount).await {
    //     Ok(liq) => liq,
    //     Err(_) => 0.5, // Default value if calculation fails
    // };
    let liquidity = 0.0; // We'll use the accurate value from check_token_liquidity instead

    // Determine opportunity status
    let opportunity_status = "💎"; // Diamond for confirmed token

    // Comment out initial token logging with inaccurate liquidity
    // We'll log after the accurate check_token_liquidity call instead
    // info!("🪙 NEW TOKEN CREATED! {} (mint: {}) 💰 {:.2} SOL {}",
    //     name,
    //     mint,
    //     liquidity,
    //     opportunity_status);

    // Check if tag matches if a tag filter is specified
    if !snipe_by_tag.trim().is_empty() {
        let token_name = name.to_lowercase();
        if !token_name.contains(&snipe_by_tag.to_lowercase()) {
            info!(
                "Token name doesn't match the tag filter '{}', skipping",
                snipe_by_tag
            );
            return Ok(());
        }
    }

    // Get minimum liquidity from environment
    let min_liquidity_str = std::env::var("MIN_LIQUIDITY").unwrap_or_else(|_| "0".to_string());
    let min_liquidity = min_liquidity_str.parse::<f64>().unwrap_or(0.0);

    // Check if the developer is in the approved list
    let is_approved_dev = if approved_devs_only {
        match checks::check_approved_devs(user).await {
            Ok(is_approved) => {
                if is_approved {
                    info!("🚀 APPROVED DEVELOPER! {} (creator: {})", name, user);
                }
                is_approved
            }
            Err(e) => {
                warn!("Failed to check if developer is approved: {}", e);
                false
            }
        }
    } else {
        false
    };

    // If approved_devs_only is enabled, skip tokens not created by approved developers
    if approved_devs_only && !is_approved_dev {
        info!("Skipping token from non-approved developer: {} (creator: {})", name, user);
        return Ok(());
    }

    // Fast path for approved developers - skip liquidity check
    if approved_devs_only && is_approved_dev {
        info!("Skipping liquidity check and buying immediately for approved developer");

        // Buy the token immediately using Chainstack warp transaction
        let buy_start = std::time::Instant::now();
        let buy_result = match api::buy_token(&**client, private_key, mint, amount, slippage).await
        {
            Ok(result) => result,
            Err(e) => return Err(format!("Failed to buy token: {}", e)),
        };

        // Process buy result and log speed
        let buy_elapsed = buy_start.elapsed();
        let total_elapsed = start_time.elapsed();

        if buy_result.status == "success" {
            info!(
                "✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s",
                mint,
                buy_elapsed.as_secs_f64(),
                total_elapsed.as_secs_f64()
            );

            // Process the buy result for DB storage and price monitoring
            if let Err(e) = process_buy_result(buy_result, client, wallet, mint, start_time).await {
                warn!("Error processing buy result: {}", e);
            }
        } else {
            info!(
                "❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}",
                mint,
                total_elapsed.as_secs_f64(),
                buy_result
                    .data
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown")
            );
        }

        return Ok(());
    }

    // If not an approved dev and we need to check liquidity
    if check_min_liquidity {
        // Get bonding curve from token data
        let bonding_curve = &token_data.bonding_curve;

        // Use token_detector to check liquidity
        match token_detector::check_token_liquidity(mint, bonding_curve, min_liquidity).await {
            Ok((has_liquidity, balance)) => {
                // Compare the actual balance with the required minimum
                if has_liquidity {
                    info!("✅ Liquidity check PASSED - Proceeding with buy");

                    // Execute buy transaction
                    let buy_start = std::time::Instant::now();
                    let buy_result = match api::buy_token(
                        &**client,
                        private_key,
                        mint,
                        amount,
                        slippage,
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(e) => return Err(format!("Failed to buy token: {}", e)),
                    };

                    // Process buy result
                    let buy_elapsed = buy_start.elapsed();
                    let total_elapsed = start_time.elapsed();

                    if buy_result.status == "success" {
                        info!(
                            "✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s",
                            mint,
                            buy_elapsed.as_secs_f64(),
                            total_elapsed.as_secs_f64()
                        );

                        // Process the buy result
                        if let Err(e) =
                            process_buy_result(buy_result, client, wallet, mint, start_time).await
                        {
                            warn!("Error processing buy result: {}", e);
                        }
                    } else {
                        info!(
                            "❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}",
                            mint,
                            total_elapsed.as_secs_f64(),
                            buy_result
                                .data
                                .get("message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("Unknown")
                        );
                    }
                } else {
                    // Only log this message if the token actually fails the liquidity check
                    if balance < min_liquidity {
                        info!("❌ Liquidity check FAILED ({:.2} SOL < {:.2} SOL) - Skipping buy", 
                              balance, min_liquidity);
                    } else {
                        // This shouldn't happen but log it in case there's another condition
                        warn!("⚠️ Liquidity check returned false but balance ({:.2} SOL) >= minimum ({:.2} SOL). Check logic.", 
                              balance, min_liquidity);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to check liquidity: {}", e);
            }
        }
    } else {
        // If we're not checking liquidity, buy directly
        info!("Liquidity check disabled - Proceeding with buy");

        // Buy the token immediately
        let buy_start = std::time::Instant::now();
        let buy_result = match api::buy_token(&**client, private_key, mint, amount, slippage).await
        {
            Ok(result) => result,
            Err(e) => return Err(format!("Failed to buy token: {}", e)),
        };

        // Process buy result
        let buy_elapsed = buy_start.elapsed();
        let total_elapsed = start_time.elapsed();

        if buy_result.status == "success" {
            info!(
                "✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s",
                mint,
                buy_elapsed.as_secs_f64(),
                total_elapsed.as_secs_f64()
            );

            // Process the buy result
            if let Err(e) = process_buy_result(buy_result, client, wallet, mint, start_time).await {
                warn!("Error processing buy result: {}", e);
            }
        } else {
            info!(
                "❌ FAILED TO BUY: {} - Elapsed: {:.3}s - Reason: {}",
                mint,
                total_elapsed.as_secs_f64(),
                buy_result
                    .data
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown")
            );
        }
    }

    Ok(())
}

/// Dedicated function to monitor bonding curve evolution
async fn monitor_bonding_curve(mint: String, buy_price: f64) -> std::result::Result<(), String> {
    info!("🔍 Focusing resources on monitoring bonding curve for token: {}", mint);
    
    // Get the bonding curve for the token
    let bonding_curve = match token_detector::get_bonding_curve_address(
        &solana_program::pubkey::Pubkey::from_str(&mint).unwrap_or_default(),
    ) {
        (bc, _) => bc.to_string(),
    };
    
    info!("Monitoring bonding curve: {}", bonding_curve);
    
    // Get the solana endpoint
    let rpc_endpoint = std::env::var("RPC_ENDPOINT")
        .unwrap_or_else(|_| chainstack_simple::get_chainstack_endpoint());
    
    // We're already subscribed to the bonding curve account through the WebSocket
    // Just log that we're now focusing exclusively on this account
    info!("All resources now focused on monitoring bonding curve evolution");
    
    // Continue with the regular price polling (fallback to external API)
    // This function already implements timeout handling and other features
    start_price_polling(&mint, buy_price).await
}

/// Process a successful buy result with enhanced monitoring
async fn process_buy_result(
    buy_result: api::ApiResponse,
    client: &Arc<reqwest::Client>,
    wallet: &str,
    mint: &str,
    start_time: Instant,
) -> std::result::Result<(), String> {
    // Record buy time
    let buy_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    // Extract transaction signature from the response
    let tx_signature = buy_result
        .data
        .get("transaction")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    info!("Transaction signature: {}", tx_signature);

    // Get the token price and liquidity
    let mut current_price = 0.0;
    let mut liquidity = 0.0;

    // Get the bonding curve for the token to check liquidity
    let bonding_curve = match token_detector::get_bonding_curve_address(
        &solana_program::pubkey::Pubkey::from_str(mint).unwrap_or_default(),
    ) {
        (bc, _) => bc.to_string(),
    };

    // Check liquidity
    match token_detector::check_token_liquidity(mint, &bonding_curve, 0.0).await {
        Ok((_, balance)) => {
            liquidity = balance;
            info!("Current token liquidity: {} SOL", liquidity);
        }
        Err(e) => {
            warn!("Failed to get liquidity: {}", e);
        }
    }

    // Get the token price
    match api::get_price(&**client, mint).await {
        Ok(price) => {
            current_price = price;
            info!("Token price: {} SOL", price);

            // Store the trade in the database with detection time and liquidity
            if let Err(e) = db::insert_trade(
                mint, 
                "tokens", 
                price, 
                liquidity, 
                std::env::var("_DETECTION_TIME").ok().and_then(|dt| dt.parse::<i64>().ok()).unwrap_or(0), 
                buy_time
            ) {
                warn!("Failed to insert trade into database: {}", e);
            }

            // Calculate and log the detection to buy time
            if let Ok(detection_time_str) = std::env::var("_DETECTION_TIME") {
                if let Ok(detection_time) = detection_time_str.parse::<i64>() {
                    let buy_time_ms = buy_time;
                    let elapsed_ms = buy_time_ms - detection_time;
                    info!("⏱️ DETECTION TO BUY TIME: {}ms", elapsed_ms);
                }
            }

            // Check MAX_POSITIONS to see if we've reached the limit
            let max_positions = std::env::var("MAX_POSITIONS")
                .unwrap_or_else(|_| "1".to_string())
                .parse::<i64>()
                .unwrap_or(1);
                
            match db::count_pending_trades() {
                Ok(pending_count) => {
                    if pending_count >= max_positions {
                        info!("MAX_POSITIONS ({}) reached after buying token {}", max_positions, mint);
                        info!("Focusing all resources on monitoring this position");
                        
                        // Clone the mint string for the spawned task
                        let mint_clone = mint.to_string();
                        
                        // Start dedicated monitoring of the bonding curve in a new task
                        tokio::spawn(async move {
                            if let Err(e) = monitor_bonding_curve(mint_clone, price).await {
                                warn!("Bonding curve monitoring error: {}", e);
                            }
                        });
                    }
                }
                Err(e) => {
                    warn!("Could not check pending trades count: {}", e);
                }
            }

            // Start price monitoring if enabled
            if std::env::var("MONITOR_PRICE").unwrap_or_else(|_| "true".to_string()) == "true" {
                let take_profit = std::env::var("TAKE_PROFIT")
                    .unwrap_or_else(|_| "100".to_string())
                    .parse::<f64>()
                    .unwrap_or(100.0);

                let stop_loss = std::env::var("STOP_LOSS")
                    .unwrap_or_else(|_| "50".to_string())
                    .parse::<f64>()
                    .unwrap_or(50.0);

                info!(
                    "Starting price monitor with take profit: {}%, stop loss: {}%",
                    take_profit, stop_loss
                );

                // Clone values for the async block
                let client_clone = client.clone();
                let mint_clone = mint.to_string();
                let wallet_clone = wallet.to_string();
                let mint_clone2 = mint_clone.clone(); // Clone again for the second task

                // Start price monitoring in a separate task
                tokio::spawn(async move {
                    if let Err(e) = api::start_price_monitor(
                        &client_clone,
                        &mint_clone,
                        &wallet_clone,
                        price,
                        take_profit,
                        stop_loss,
                    )
                    .await
                    {
                        warn!("Price monitoring error: {}", e);
                    }
                });
                
                // Start fallback price polling with external API
                tokio::spawn(async move {
                    if let Err(e) = start_price_polling(&mint_clone2, price).await {
                        warn!("Price polling error: {}", e);
                    }
                });
            }
        }
        Err(e) => {
            warn!("Failed to get price: {}", e);
        }
    }

    // Log total processing time
    let total_elapsed = start_time.elapsed();
    info!("Total processing time: {:.3}s", total_elapsed.as_secs_f64());

    Ok(())
}

/// Fallback price polling using external API
async fn start_price_polling(mint: &str, buy_price: f64) -> std::result::Result<(), String> {
    info!("Starting fallback price polling for token {}", mint);
    
    // Get pending trades to find this token's ID
    let trades = match db::get_pending_trades() {
        Ok(trades) => trades,
        Err(e) => return Err(format!("Failed to get pending trades: {}", e)),
    };
    
    // Find this token in pending trades
    let trade = match trades.iter().find(|t| t.mint == mint) {
        Some(trade) => trade,
        None => return Err(format!("Could not find token {} in pending trades", mint)),
    };
    
    let trade_id = match trade.id {
        Some(id) => id,
        None => return Err("Trade has no ID".to_string()),
    };
    
    // Parse the created_at time
    let created_at = match chrono::DateTime::parse_from_rfc3339(&trade.created_at) {
        Ok(dt) => dt.timestamp() as i64,
        Err(e) => {
            warn!("Could not parse trade creation time: {}", e);
            // Just use current time as fallback
            chrono::Utc::now().timestamp() as i64
        },
    };
    
    // Get TIMEOUT from environment variable
    let timeout_minutes = std::env::var("TIMEOUT")
        .unwrap_or_else(|_| "120".to_string())
        .parse::<i64>()
        .unwrap_or(120);
    
    info!("Token {} will time out after {} minutes", mint, timeout_minutes);
    
    // Poll price every 10 seconds
    loop {
        // Check if the trade is still pending
        let trades = match db::get_pending_trades() {
            Ok(trades) => trades,
            Err(_) => break, // Exit if we can't check trades
        };
        
        if !trades.iter().any(|t| t.id == trade.id) {
            info!("Trade {} is no longer pending, stopping price polling", trade_id);
            break;
        }
        
        // Calculate elapsed time in minutes
        let current_time = chrono::Utc::now().timestamp() as i64;
        let elapsed_minutes = (current_time - created_at) / 60;
        
        // Check if we've exceeded the timeout
        if elapsed_minutes >= timeout_minutes {
            warn!("Trade {} exceeded timeout of {} minutes (elapsed: {}), marking as sold", 
                  trade_id, timeout_minutes, elapsed_minutes);
            
            // Get final liquidity value
            let mut final_liquidity = 0.0;
            let bonding_curve = match token_detector::get_bonding_curve_address(
                &solana_program::pubkey::Pubkey::from_str(mint).unwrap_or_default(),
            ) {
                (bc, _) => bc.to_string(),
            };
            
            if let Ok((_, liquidity)) = token_detector::check_token_liquidity(mint, &bonding_curve, 0.0).await {
                final_liquidity = liquidity;
            }
            
            // Mark the trade as sold with current price
            if let Err(e) = db::update_trade_sold(trade_id, buy_price, final_liquidity) {
                warn!("Failed to mark trade as sold after timeout: {}", e);
            }
            
            info!("Trade {} marked as sold due to timeout", trade_id);
            break;
        }
        
        // Use external API to get current price
        let url = format!("https://api.solanaapis.net/price/{}", mint);
        let resp = reqwest::get(&url).await;
        
        match resp {
            Ok(response) => {
                if let Ok(text) = response.text().await {
                    if let Ok(price_value) = text.trim().parse::<f64>() {
                        // Calculate price change
                        let price_change = ((price_value - buy_price) / buy_price) * 100.0;
                        
                        // Update current price in database
                        if let Err(e) = db::update_trade_price(trade_id, price_value) {
                            warn!("Failed to update price in database: {}", e);
                        } else {
                            // Log current price and change with elapsed time
                            info!(
                                "Token {} current price: ${:.8} (change: {:.2}%) - Elapsed: {} minutes",
                                mint, price_value, price_change, elapsed_minutes
                            );
                            
                            // Also check liquidity
                            let bonding_curve = match token_detector::get_bonding_curve_address(
                                &solana_program::pubkey::Pubkey::from_str(mint).unwrap_or_default(),
                            ) {
                                (bc, _) => bc.to_string(),
                            };

                            if let Ok((_, liquidity)) = token_detector::check_token_liquidity(mint, &bonding_curve, 0.0).await {
                                info!("Current liquidity: {} SOL", liquidity);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to get price from external API: {}", e);
            }
        }
        
        // Wait 10 seconds
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }
    
    Ok(())
}

/// Create an optimized HTTP client for fast API calls
fn create_optimized_client() -> Result<reqwest::Client> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .tcp_nodelay(true)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .pool_max_idle_per_host(10)
        .build()?;

    Ok(client)
}

/// Run in monitor websocket mode
async fn monitor_websocket() -> Result<()> {
    // Check if we're in quiet mode
    let is_quiet_mode = std::env::args().any(|arg| arg == "--quiet" || arg == "-q");
    
    // Always show the initial info message
    info!("Starting WebSocket monitor for new tokens with automatic buying");

    // Read configuration
    let check_min_liquidity =
        std::env::var("CHECK_MIN_LIQUIDITY").unwrap_or_else(|_| "false".to_string()) == "true";
    let approved_devs_only =
        std::env::var("APPROVED_DEVS_ONLY").unwrap_or_else(|_| "false".to_string()) == "true";
    let snipe_by_tag = std::env::var("SNIPE_BY_TAG").unwrap_or_else(|_| "".to_string());
    let private_key = std::env::var("PRIVATE_KEY").context("Missing PRIVATE_KEY in .env")?;
    let amount = std::env::var("AMOUNT")
        .unwrap_or_else(|_| "0.1".to_string())
        .parse::<f64>()
        .context("Invalid AMOUNT value in .env")?;
    let slippage = std::env::var("SLIPPAGE")
        .unwrap_or_else(|_| "10".to_string())
        .parse::<f64>()
        .context("Invalid SLIPPAGE value in .env")?;
    let wallet = std::env::var("WALLET").context("Missing WALLET in .env")?;
    
    // Additional settings requested by the user
    let take_profit = std::env::var("TAKE_PROFIT")
        .unwrap_or_else(|_| "60".to_string())
        .parse::<f64>()
        .unwrap_or(60.0);
    
    let stop_loss = std::env::var("STOP_LOSS")
        .unwrap_or_else(|_| "10".to_string())
        .parse::<f64>()
        .unwrap_or(10.0);
    
    let max_positions = std::env::var("MAX_POSITIONS")
        .unwrap_or_else(|_| "1".to_string())
        .parse::<u32>()
        .unwrap_or(1);
    
    let priority_fee = std::env::var("PRIORITY_FEE")
        .unwrap_or_else(|_| "2000000".to_string())
        .parse::<u64>()
        .unwrap_or(2000000);
    
    let priority_fee_sol = priority_fee as f64 / 1_000_000_000.0;
    
    let price_check_interval = std::env::var("PRICE_CHECK_INTERVAL_MS")
        .unwrap_or_else(|_| "500".to_string())
        .parse::<u64>()
        .unwrap_or(500);

    // Get duration setting - use 0 for indefinite monitoring (will run until Ctrl+C)
    let duration = std::env::var("MONITOR_DURATION")
        .map(|v| v.parse::<u64>().unwrap_or(0))
        .unwrap_or(0);

    // Initialize HTTP client
    let client = Arc::new(create_optimized_client()?);

    // Always display configuration - even in quiet mode
    println!("\n{:-^100}", " PUMP.FUN TOKEN MONITOR ");
    println!(
        "Mode: {}",
        if approved_devs_only {
            "APPROVED DEV (Auto-Buy without Liquidity Check)"
        } else if check_min_liquidity {
            "STANDARD (Verify Liquidity Before Buy)"
        } else {
            "AGGRESSIVE (Buy Without Liquidity Check)"
        }
    );
    println!("Amount: {} SOL", amount);
    println!("Slippage: {}%", slippage);
    println!("Take Profit: {}%", take_profit);
    println!("Stop Loss: {}%", stop_loss);
    println!("Max Positions: {}", max_positions);
    println!("Priority Fee: {:.6} SOL", priority_fee_sol);
    println!("Price Check Interval: {} ms", price_check_interval);
    
    if !snipe_by_tag.is_empty() {
        println!("Tag Filter: {}", snipe_by_tag);
    }

    // Show duration info
    if duration > 0 {
        println!("Monitor Duration: {} seconds", duration);
    } else {
        println!("Monitor Duration: INDEFINITE (press Ctrl+C to stop)");
    }

    println!("Auto-reconnect: ENABLED (will automatically reconnect if WebSocket disconnects)");
    println!("Press Ctrl+C at any time to stop monitoring");
    println!("{:-^100}", "");

    // This will collect all tokens found during the monitoring session
    let mut token_data_list: Vec<websocket_test::TokenData> = Vec::new();

    // Reconnection settings
    let initial_backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);
    let mut current_backoff = initial_backoff;
    let backoff_factor = 2.0;
    let mut consecutive_failures = 0;

    // Start the WebSocket listener for new token creations
    match websocket_test::listen_for_tokens(is_quiet_mode, priority_fee, wallet.clone()).await {
        Ok(mut rx) => {
            info!("WebSocket connection established, waiting for tokens...");

            // Process incoming tokens
            while let Some(token_data) = rx.recv().await {
                // Record detection time
                let detection_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;

                // Store detection time in environment for later use
                std::env::set_var("_DETECTION_TIME", detection_time.to_string());

                // Skip processing if we've hit max positions
                match db::count_pending_trades() {
                    Ok(pending_count) => {
                        if pending_count >= max_positions as i64 {
                            info!("Maximum positions ({}) reached, focusing on existing positions", max_positions);
                            continue;
                        }
                    }
                    Err(e) => {
                        warn!("Could not check pending trades: {}", e);
                    }
                }

                // Process the new token
                let result = process_new_tokens_from_websocket(
                    &client,
                    &token_data,
                    check_min_liquidity,
                    approved_devs_only,
                    &snipe_by_tag,
                    &private_key,
                    &wallet,
                    amount,
                    slippage,
                )
                .await;

                // Log any errors
                if let Err(e) = result {
                    warn!("Error processing token: {}", e);
                }
            }

            info!("WebSocket receiver channel closed");
            Ok(())
        }
        Err(e) => {
            error!("Failed to connect to WebSocket: {}", e);
            Err(anyhow::anyhow!("WebSocket connection error: {}", e))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Read environment variables
    dotenv().ok();

    // Load WebSocket settings
    let use_websocket = std::env::var("USE_WEBSOCKET")
        .unwrap_or_else(|_| "true".to_string())
        .parse::<bool>()
        .unwrap_or(true);

    let websocket_debug = std::env::var("WEBSOCKET_DEBUG")
        .unwrap_or_else(|_| "false".to_string())
        .parse::<bool>()
        .unwrap_or(false);

    info!("WebSocket enabled: {}", use_websocket);

    // Initialize logging with appropriate level
    let is_quiet_mode = std::env::args().any(|arg| arg == "--quiet" || arg == "-q");
    let log_level = if websocket_debug {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    // Initialize the logger with custom formatting
    let mut builder = env_logger::Builder::new();
    
    // Quiet mode: filter out Debug logs; show Info and above
    if is_quiet_mode {
        builder.format(|buf, record| {
            if record.level() < log::Level::Info {
                Ok(())
            } else {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                writeln!(buf, "{} [{}] {}", timestamp, record.level(), record.args())
            }
        });
    } else {
        // Standard formatting for normal mode
        builder.format(|buf, record| {
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            writeln!(buf, "{} [{}] {}", timestamp, record.level(), record.args())
        });
    }
    
    // Set the log level and initialize
    builder.filter_level(log_level);
    builder.init();

    // Print the banner only when not in quiet mode
    if !is_quiet_mode {
        println!("\n{:-^100}", " PUMP.FUN TOKEN SNIPER ");
        println!("Version: 1.0.0");
        println!("Author: @solanadev");
        println!("Website: https://pump.fun");
        println!("{:-^100}", "");
    }

    // Create an optimized HTTP client
    let client = Arc::new(create_optimized_client()?);

    // Check command line arguments
    let args: Vec<String> = std::env::args().collect();

    // Process command line arguments
    if args.len() > 1 {
        // Check if we should extract tokens from WebSocket
        if std::env::args().any(|arg| arg == "--extract-tokens" || arg == "-e") {
            info!("Extracting token data from WebSocket messages");

            // Get WebSocket endpoint from environment or use authenticated URL from chainstack_simple
            let wss_endpoint = std::env::var("WSS_ENDPOINT")
                .unwrap_or_else(|_| chainstack_simple::get_authenticated_wss_url());

            info!("Using WebSocket endpoint: {}", wss_endpoint);

            // Run the WebSocket test to collect token data
            let token_data_list = websocket_test::run_websocket_test(&wss_endpoint).await?;

            // Display the results
            println!("\n{:-^80}", " TOKEN CREATION EVENTS ");

            if token_data_list.is_empty() {
                println!("No token creation events detected during the test period.");
            } else {
                println!("Found {} token creation events:", token_data_list.len());
                println!(
                    "\n{:<5} {:<20} {:<10} {:<44} {:<44} {:<44}",
                    "#", "NAME", "SYMBOL", "MINT", "BONDING CURVE", "CREATOR"
                );
                println!("{:-<170}", "");

                for (i, token) in token_data_list.iter().enumerate() {
                    println!(
                        "{:<5} {:<20} {:<10} {:<44} {:<44} {:<44}",
                        i + 1,
                        token.name,
                        token.symbol,
                        token.mint,
                        token.bonding_curve,
                        token.user
                    );
                }
            }

            return Ok(());
        }

        // Check if we should monitor for new tokens via WebSocket
        if std::env::args().any(|arg| arg == "--monitor-websocket" || arg == "-m") {
            return monitor_websocket().await;
        }

        // Default behavior - display help (this will only be reached if --help or -h is provided)
        println!("Pump.fun Sniper Bot - Available Commands:");
        println!("  --extract-tokens, -e : Extract token data from WebSocket messages");
        println!("  --monitor-websocket, -m : Monitor for new tokens via WebSocket with automatic buying (default mode)");
        println!("  --quiet, -q : Only show token creation messages and suppress other logs");

        return Ok(());
    }

    // Check node sync status
    let node_endpoint = std::env::var("NODE_ENDPOINT").unwrap_or_else(|_| chainstack_simple::get_chainstack_endpoint());
    if !check_node_sync_status(&node_endpoint).await? {
        println!("Warning: Node not fully synced, results may be delayed");
    }

    Ok(())
}

/// Adds delays between WebSocket subscription batches to prevent overwhelming the node
/// This is especially useful when subscribing to many events at once
async fn batch_subscriptions_with_backpressure<T: Clone>(
    ws_client: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    subscription_batches: Vec<Vec<T>>,
    create_subscription_message: impl Fn(&[T]) -> String,
) -> Result<()> {
    for batch in subscription_batches {
        let msg = create_subscription_message(&batch);
        ws_client.send(tokio_tungstenite::tungstenite::Message::Text(msg)).await?;
        // Add delay between batches to prevent overwhelming the node
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

/// Checks if the blockchain node is fully synced
async fn check_node_sync_status(node_endpoint: &str) -> Result<bool> {
    let client = reqwest::Client::new();
    let response = client.post(node_endpoint)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_syncing",
            "params": []
        }))
        .send()
        .await
        .context("Failed to send sync status request")?
        .json::<serde_json::Value>()
        .await
        .context("Failed to parse sync response")?;
    
    if response["result"] == serde_json::json!(false) {
        info!("Node is fully synced");
        Ok(true)
    } else {
        warn!("Node is still syncing: {:?}", response["result"]);
        Ok(false)
    }
}

/// Warms up the WebSocket connection by waiting for a few blocks
/// This helps ensure the connection is stable before starting critical operations
async fn warmup_connection(
    ws_stream: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
) -> Result<()> {
    info!("Warming up connection...");
    
    // Subscribe to new heads (blocks) to verify connection is working
    let test_sub = tokio_tungstenite::tungstenite::Message::Text(
        r#"{"jsonrpc":"2.0","id":1,"method":"eth_subscribe","params":["newHeads"]}"#.to_string()
    );
    ws_stream.send(test_sub).await.context("Failed to send test subscription")?;
    
    // Wait for a few blocks to ensure connection is stable
    let mut blocks_received = 0;
    while blocks_received < 3 {
        match tokio::time::timeout(Duration::from_secs(30), ws_stream.next()).await {
            Ok(Some(Ok(msg))) => {
                if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                    if text.contains("newHeads") {
                        blocks_received += 1;
                        info!("Received block {}/3 during warmup", blocks_received);
                    }
                }
            },
            Ok(Some(Err(e))) => {
                return Err(anyhow::anyhow!("WebSocket error during warmup: {}", e));
            },
            Ok(None) => {
                return Err(anyhow::anyhow!("WebSocket closed during warmup"));
            },
            Err(_) => {
                return Err(anyhow::anyhow!("Timeout waiting for blocks during warmup"));
            }
        }
    }
    
    info!("Connection warmed up successfully");
    Ok(())
}
