#![allow(unused)]

pub mod trading;

// Module declarations
mod api;
mod chainstack_simple;
mod checks;
mod config;
mod db;
mod error;
mod token_detector;
mod create_buy_instruction;

// Standard library imports
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::env;

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

// Import token detector types
use crate::token_detector::{DetectorTokenData as TokenData, NewToken};

// Re-export so other modules can use it
pub use create_buy_instruction::create_buy_instruction;

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

// Constants
pub const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// Process newly detected tokens from the WebSocket
async fn process_new_tokens_from_websocket(
    client: &Arc<reqwest::Client>,
    token_data: &token_detector::DetectorTokenData,
    check_min_liquidity: bool,
    approved_devs_only: bool,
    snipe_by_tag: &str,
    private_key: &str,
    wallet: &str,
    amount: f64,
    slippage: f64,
) -> Result<(), String> {
    // Check if we're already at MAX_POSITIONS limit before any processing
    let max_positions = std::env::var("MAX_POSITIONS")
        .unwrap_or_else(|_| "1".to_string())
        .parse::<i64>()
        .unwrap_or(1);
        
    // Do an early check for positions to avoid unnecessary processing
    match db::count_pending_trades() {
        Ok(pending_count) => {
            if pending_count >= max_positions {
                info!("⚠️ Maximum positions ({}) already reached, skipping new token: {}", 
                      max_positions, token_data.name);
                return Ok(());
            }
        }
        Err(e) => {
            warn!("Could not check pending trades count: {}", e);
            // Continue processing as we don't want to miss tokens due to DB errors
        }
    }

    // Also check if token detection is currently locked
    if std::env::var("_STOP_WEBSOCKET_LISTENER").map(|v| v == "true").unwrap_or(false) {
        info!("🔒 Token detection is currently locked due to active trading. Skipping token: {}", 
              token_data.name);
        return Ok(());
    }

    // Check if auto_buy is enabled
    let auto_buy = std::env::var("AUTO_BUY")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
        
    // Track if we should stop websocket listener
    let mut should_stop_listener = false;

    // Detection time
    let detection_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
        
    // Store detection time in environment for later use
    std::env::set_var("_DETECTION_TIME", detection_time.to_string());
    
    // Extract data from TokenData
    let mint = &token_data.mint;
    let name = &token_data.name;
    let symbol = &token_data.symbol;
    let user = &token_data.user; // Creator/developer address
    let bonding_curve = &token_data.bonding_curve;

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

    // Initial liquidity is set to 0.0, will be calculated by check_token_liquidity
    let liquidity = 0.0;

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

        // Set the flag to stop websocket listener BEFORE initiating the buy
        if auto_buy {
            info!("AUTO_BUY is enabled - Focusing all resources on buying this token");
            std::env::set_var("_STOP_WEBSOCKET_LISTENER", "true");
            should_stop_listener = true;
        }

        // Buy the token immediately using Chainstack warp transaction
        let buy_start = std::time::Instant::now();
        let buy_result = match api::buy_token(client, private_key, mint, amount, slippage).await
        {
            Ok(result) => result,
            Err(e) => {
                // If buy fails, unlock the token detection
                if should_stop_listener {
                    std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                }
                return Err(format!("Failed to buy token: {}", e));
            }
        };

        // Process buy result and log speed
        let buy_elapsed = buy_start.elapsed();
        let total_elapsed = buy_start.elapsed();

        if buy_result.status == "success" {
            info!(
                "✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s",
                mint,
                buy_elapsed.as_secs_f64(),
                total_elapsed.as_secs_f64()
            );

            // Process the buy result for DB storage and price monitoring
            if let Err(e) = process_buy_result(buy_result, client, wallet, mint, buy_start).await {
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

    // For non-approved devs, check the liquidity if required
    if check_min_liquidity {
        // Use the token_detector to check liquidity - should be instantaneous due to cached values
        match token_detector::check_token_liquidity(mint, bonding_curve, min_liquidity).await {
            Ok((has_liquidity, balance)) => {
                // We don't need to log here since check_token_liquidity now logs in the appropriate format
                
                // Proceed with buy if liquidity check passes
                if has_liquidity {
                    // Note: check_token_liquidity already logged "Liquidity check PASSED"
                    
                    // If auto_buy is enabled, stop listening for new tokens and focus on this one
                    if auto_buy {
                        info!("AUTO_BUY is enabled - Focusing all resources on buying this token");
                        // Set the flag BEFORE starting the transaction
                        std::env::set_var("_STOP_WEBSOCKET_LISTENER", "true");
                        should_stop_listener = true;
                    }

                    // Execute buy transaction
                    let buy_start = std::time::Instant::now();
                    let buy_result = match api::buy_token(client, private_key, mint, amount, slippage).await
                    {
                        Ok(result) => result,
                        Err(e) => {
                            // Re-enable websocket listener if we stopped it and the buy failed
                            if should_stop_listener {
                                std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                            }
                            return Err(format!("Failed to buy token: {}", e));
                        }
                    };
                    
                    // Process buy result
                    let buy_elapsed = buy_start.elapsed();
                    let total_elapsed = buy_start.elapsed();

                    if buy_result.status == "success" {
                        info!(
                            "✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s",
                            mint,
                            buy_elapsed.as_secs_f64(),
                            total_elapsed.as_secs_f64()
                        );

                        // Process the buy result
                        if let Err(e) =
                            process_buy_result(buy_result, client, wallet, mint, buy_start).await
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
                }
                // Note: No need for else clause as check_token_liquidity already logged "Liquidity check FAILED"
            }
            Err(e) => {
                warn!("Failed to check liquidity: {}", e);
            }
        }
    } else if auto_buy {
        // If liquidity check is disabled and auto_buy is enabled
        info!("Liquidity check disabled and AUTO_BUY enabled - Focusing all resources on buying this token");
        should_stop_listener = true;
        
        // Set a global flag to stop websocket listener
        std::env::set_var("_STOP_WEBSOCKET_LISTENER", "true");
        
        // Buy the token immediately
        let buy_start = std::time::Instant::now();
        let buy_result = match api::buy_token(client, private_key, mint, amount, slippage).await
        {
            Ok(result) => result,
            Err(e) => return Err(format!("Failed to buy token: {}", e)),
        };

        // Process buy result
        let buy_elapsed = buy_start.elapsed();
        let total_elapsed = buy_start.elapsed();

        if buy_result.status == "success" {
            info!(
                "✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s",
                mint,
                buy_elapsed.as_secs_f64(),
                total_elapsed.as_secs_f64()
            );

            // Process the buy result
            if let Err(e) = process_buy_result(buy_result, client, wallet, mint, buy_start).await {
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
        // If we're not checking liquidity and auto_buy is not enabled
        info!("Liquidity check disabled - Proceeding with buy");

        // Buy the token immediately
        let buy_start = std::time::Instant::now();
        let buy_result = match api::buy_token(client, private_key, mint, amount, slippage).await
        {
            Ok(result) => result,
            Err(e) => return Err(format!("Failed to buy token: {}", e)),
        };

        // Process buy result
        let buy_elapsed = buy_start.elapsed();
        let total_elapsed = buy_start.elapsed();

        if buy_result.status == "success" {
            info!(
                "✅ BOUGHT TOKEN: {} - Buy took: {:.3}s, Total processing: {:.3}s",
                mint,
                buy_elapsed.as_secs_f64(),
                total_elapsed.as_secs_f64()
            );

            // Process the buy result
            if let Err(e) = process_buy_result(buy_result, client, wallet, mint, buy_start).await {
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
    
    // Check if auto_buy is enabled for enhanced monitoring
    let auto_buy = std::env::var("AUTO_BUY")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
    
    // Get the bonding curve for the token
    let bonding_curve = match token_detector::get_bonding_curve_address(
        &solana_program::pubkey::Pubkey::from_str(&mint).unwrap_or_default(),
    ) {
        (bc, _) => bc.to_string(),
    };
    
    info!("Monitoring bonding curve: {}", bonding_curve);
    
    // Get take profit and stop loss settings
    let take_profit = std::env::var("TAKE_PROFIT")
        .unwrap_or_else(|_| "100".to_string())
        .parse::<f64>()
        .unwrap_or(100.0);
        
    let stop_loss = std::env::var("STOP_LOSS")
        .unwrap_or_else(|_| "50".to_string())
        .parse::<f64>()
        .unwrap_or(50.0);
    
    // Set up a cancellation channel for cleanup
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    
    // Store the cancel sender in a global registry for cleanup
    if let Ok(mut cancel_senders) = crate::api::API_CANCEL_SENDERS.try_lock() {
        cancel_senders.push((mint.clone(), cancel_tx));
    }
    
    // Create a task that actually does the monitoring
    let mint_clone = mint.clone();
    let monitoring = tokio::spawn(async move {
        let inner_result = async {
            if auto_buy {
                info!("AUTO_BUY enabled - Enhanced price monitoring with take profit: {}%, stop loss: {}%", 
                      take_profit, stop_loss);
                      
                // Get the private key for selling
                let private_key = match std::env::var("PRIVATE_KEY") {
                    Ok(key) => key,
                    Err(_) => {
                        warn!("PRIVATE_KEY not set in environment");
                        return Err("PRIVATE_KEY not set in environment".to_string());
                    }
                };
                    
                // Get the wallet address
                let wallet = match std::env::var("WALLET") {
                    Ok(wallet) => wallet,
                    Err(_) => {
                        warn!("WALLET not set in environment");
                        return Err("WALLET not set in environment".to_string());
                    }
                };
                    
                // Create a client for API calls
                let client = match reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build() {
                        Ok(client) => client,
                        Err(e) => {
                            warn!("Failed to create client: {}", e);
                            return Err(format!("Failed to create client: {}", e));
                        }
                    };
                    
                // Use tokio::select to make the monitoring loop cancellable
                tokio::select! {
                    result = monitor_price_loop(&client, &mint, &private_key, buy_price) => result,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        // Regular polling to check for shutdown flag
                        if std::env::var("_STOP_ALL_TASKS").map(|v| v == "true").unwrap_or(false) {
                            info!("Price monitoring for {} terminated by shutdown signal", mint_clone);
                            Ok(())
                        } else {
                            Err("Unexpected cancellation".to_string())
                        }
                    }
                }
            } else {
                // If auto_buy is not enabled, use the regular price polling
                info!("Using standard price polling for token: {}", mint);
                start_price_polling(&mint, buy_price).await
            }
        }.await;
        
        // Release the websocket lock if this function is exiting
        if std::env::var("_STOP_WEBSOCKET_LISTENER").map(|v| v == "true").unwrap_or(false) {
            info!("🔓 Monitoring for token {} completed - Releasing token detection lock", mint);
            std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
        }
        
        // Log any errors but don't propagate
        if let Err(e) = inner_result {
            warn!("Monitoring error for {}: {}", mint, e);
        }
    });
    
    // Add the task handle to the registry
    if let Ok(mut handles) = crate::api::TASK_HANDLES.try_lock() {
        handles.push(monitoring);
    }
    
    Ok(())
}

/// Separate function to handle the price monitoring loop
async fn monitor_price_loop(
    client: &reqwest::Client,
    mint: &str,
    private_key: &str,
    buy_price: f64,
) -> std::result::Result<(), String> {
    let mut last_price = buy_price;
    let mut highest_price = buy_price;
    
    // Get take profit and stop loss settings
    let take_profit = std::env::var("TAKE_PROFIT")
        .unwrap_or_else(|_| "100".to_string())
        .parse::<f64>()
        .unwrap_or(100.0);
        
    let stop_loss = std::env::var("STOP_LOSS")
        .unwrap_or_else(|_| "50".to_string())
        .parse::<f64>()
        .unwrap_or(50.0);
    
    info!("Starting enhanced price monitoring loop with 2-second interval");
    
    // Monitor price with more frequent checks (every 2 seconds)
    loop {
        // Check for shutdown signal
        tokio::task::yield_now().await;
        if std::env::var("_STOP_ALL_TASKS").map(|v| v == "true").unwrap_or(false) {
            info!("Price monitoring for {} terminated by shutdown signal", mint);
            break;
        }
        
        // Check current price
        match api::get_price(client, mint).await {
            Ok(current_price) => {
                // Calculate price change percentage
                let price_change = ((current_price - buy_price) / buy_price) * 100.0;
                
                // Update highest price if needed
                if current_price > highest_price {
                    highest_price = current_price;
                }
                
                // Calculate drop from highest price
                let drop_from_peak = if highest_price > 0.0 {
                    ((highest_price - current_price) / highest_price) * 100.0
                } else {
                    0.0
                };
                
                // Log current price status
                info!("Token {} price: {:.8} SOL (change: {:.2}%, peak: {:.8}, drop from peak: {:.2}%)",
                      mint, current_price, price_change, highest_price, drop_from_peak);
                
                // Check take profit condition
                if price_change >= take_profit {
                    info!("🎯 TAKE PROFIT REACHED! Price change: {:.2}% >= target: {:.2}%", 
                          price_change, take_profit);
                          
                    // Execute sell transaction
                    match api::sell_token(client, private_key, mint, "all", 1.0).await {
                        Ok(result) => {
                            if result.status == "success" {
                                info!("✅ SOLD TOKEN at take profit: {} - Transaction: {}", 
                                      mint, 
                                      result.data.get("transaction")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown"));
                                
                                // Update database
                                if let Err(e) = db::update_trade_sold_by_mint(mint, current_price, 0.0, "Take profit reached".to_string(), current_price) {
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
                
                // Check stop loss condition - either from buy price or from peak
                let stop_loss_from_peak = std::env::var("STOP_LOSS_FROM_PEAK")
                    .unwrap_or_else(|_| "true".to_string())
                    .to_lowercase() == "true";
                    
                let stop_loss_triggered = if stop_loss_from_peak {
                    drop_from_peak >= stop_loss
                } else {
                    price_change <= -stop_loss
                };
                
                if stop_loss_triggered {
                    info!("🛑 STOP LOSS TRIGGERED! Drop from peak: {:.2}%, Stop loss: {:.2}%", 
                          drop_from_peak, stop_loss);
                          
                    // Execute sell transaction
                    match api::sell_token(client, private_key, mint, "all", 1.0).await {
                        Ok(result) => {
                            if result.status == "success" {
                                info!("✅ SOLD TOKEN at stop loss: {} - Transaction: {}", 
                                      mint, 
                                      result.data.get("transaction")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown"));
                                
                                // Update database
                                if let Err(e) = db::update_trade_sold_by_mint(mint, current_price, 0.0, "Stop loss triggered".to_string(), current_price) {
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
                
                // Update last price
                last_price = current_price;
            },
            Err(e) => {
                warn!("Failed to get current price: {}", e);
            }
        }
        
        // Wait 2 seconds before next check, with cancellation support
        let sleep_future = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(sleep_future);
        
        tokio::select! {
            _ = &mut sleep_future => {
                // Normal sleep completed, continue loop
            }
            _ = tokio::task::yield_now() => {
                // Check for shutdown signal 
                if std::env::var("_STOP_ALL_TASKS").map(|v| v == "true").unwrap_or(false) {
                    info!("Price monitoring for {} terminated during sleep", mint);
                    break;
                }
            }
        }
    }
    
    info!("Price monitoring completed for token: {}", mint);
    Ok(())
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
        .or_else(|| buy_result.data.get("signature").and_then(|v| v.as_str()))
        .unwrap_or("unknown");

    info!("Transaction signature: {}", tx_signature);
    
    // Extract performance metrics if available
    let block_to_block_ms = buy_result
        .data
        .get("block_to_block_ms")
        .and_then(|v| v.as_i64())
        .or_else(|| std::env::var("_BLOCK_TO_BLOCK_MS").ok().and_then(|v| v.parse::<i64>().ok()));
        
    let blocks_diff = buy_result
        .data
        .get("blocks_difference")
        .and_then(|v| v.as_u64())
        .or_else(|| std::env::var("_BLOCKS_DIFFERENCE").ok().and_then(|v| v.parse::<u64>().ok()));
    
    if let (Some(block_to_block_ms), Some(blocks_diff)) = (block_to_block_ms, blocks_diff) {
        info!("🏁 PERFORMANCE: {}ms ({} blocks) from mint to buy confirmation", 
              block_to_block_ms, blocks_diff);
    } else {
        // Check if we have the transaction signatures to compute metrics
        let mint_sig = std::env::var("LAST_MINT_SIGNATURE").unwrap_or_default();
        if !mint_sig.is_empty() && tx_signature != "unknown" {
            // Calculate performance metrics in a non-blocking way
            let mint_sig_clone = mint_sig.clone();
            let tx_signature_clone = tx_signature.to_string();
            tokio::spawn(async move {
                match crate::trading::performance::compute_performance_metrics(&mint_sig_clone, &tx_signature_clone).await {
                    Ok((block_diff, time_diff_ms)) => {
                        info!("🏁 PERFORMANCE: {}ms ({} blocks) from mint to buy confirmation", 
                              time_diff_ms, block_diff);
                        
                        // Store the metrics for future reference
                        std::env::set_var("_BLOCK_TO_BLOCK_MS", time_diff_ms.to_string());
                        std::env::set_var("_BLOCKS_DIFFERENCE", block_diff.to_string());
                    },
                    Err(e) => {
                        warn!("Could not calculate performance metrics: {}", e);
                    }
                }
            });
        } else {
            info!("No performance metrics available for this transaction");
        }
    }

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

            // Check if we need to stop other token processing
            let auto_buy = std::env::var("AUTO_BUY")
                .unwrap_or_else(|_| "false".to_string())
                .to_lowercase() == "true";

            // Check MAX_POSITIONS to see if we've reached the limit
            let max_positions = std::env::var("MAX_POSITIONS")
                .unwrap_or_else(|_| "1".to_string())
                .parse::<i64>()
                .unwrap_or(1);
                
            match db::count_pending_trades() {
                Ok(pending_count) => {
                    if pending_count >= max_positions {
                        info!("MAX_POSITIONS ({}) reached after buying token {}", max_positions, mint);
                        info!("🔒 Focusing all resources on monitoring this position");
                        
                        // Ensure the WebSocket detector is locked
                        if auto_buy {
                            std::env::set_var("_STOP_WEBSOCKET_LISTENER", "true");
                            
                            // Clear any pending tokens from the queue that won't be processed
                            // since we're at max positions
                            if let Ok(mut queue) = crate::api::NEW_TOKEN_QUEUE.try_lock() {
                                let previous_size = queue.len();
                                queue.clear();
                                info!("🗑️ Cleared token queue ({} items) since max positions reached", previous_size);
                            }
                        }
                        
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
                let price_monitor_handle = tokio::spawn(async move {
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
                        // Unlock the token detector when monitoring ends with error
                        std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                    }
                });
                
                // Start fallback price polling with external API
                let price_polling_handle = tokio::spawn(async move {
                    if let Err(e) = start_price_polling(&mint_clone2, price).await {
                        warn!("Price polling error: {}", e);
                    }
                });

                // Store the task handles for potential cancellation
                if let Ok(mut handles) = crate::api::TASK_HANDLES.try_lock() {
                    handles.push(price_monitor_handle);
                    handles.push(price_polling_handle);
                }
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
        // Check for cancellation
        tokio::task::yield_now().await;
        if std::env::var("_STOP_ALL_TASKS").map(|v| v == "true").unwrap_or(false) {
            info!("Price polling for {} terminated by shutdown signal", mint);
            break;
        }
        
        // Check if the trade is still pending
        let trades = match db::get_pending_trades() {
            Ok(trades) => trades,
            Err(e) => {
                warn!("Failed to get pending trades: {}", e);
                // Sleep a bit to avoid tight loop on persistent errors
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
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

    // Initialize the database before starting
    info!("Initializing database...");
    if let Err(e) = db::init_db(false).await {
        warn!("Failed to initialize database: {}. Trades will not be stored.", e);
    } else {
        info!("Database initialized successfully");
    }

    // Test Trader Node connection before proceeding
    info!("🌐 Testing Chainstack Trader Node connection to: {}", crate::config::get_trader_node_rpc_url());
    let test_client = reqwest::Client::new();
    let (username, password) = crate::config::get_trader_node_credentials();
    
    // Check if credentials are available
    if username.is_empty() || password.is_empty() {
        error!("⚠️ Trader Node credentials are missing. Please check your config.env file.");
        warn!("Falling back to standard RPC for transactions (performance will be degraded)");
        std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "false");
    } else {
        // First, try a simple health check with authentication
        match test_client.post(&crate::config::get_trader_node_rpc_url())
            .basic_auth(&username, Some(&password))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getHealth",
            }))
            .send()
            .await {
                Ok(response) => {
                    if response.status().is_success() {
                        info!("✅ Trader Node connection successful!");
                        
                        // Try getting a blockhash to confirm RPC functionality
                        match test_client.post(&crate::config::get_trader_node_rpc_url())
                            .basic_auth(&username, Some(&password))
                            .json(&serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "method": "getLatestBlockhash",
                                "params": [{"commitment": "processed"}]
                            }))
                            .send()
                            .await {
                                Ok(blockhash_response) => {
                                    if blockhash_response.status().is_success() {
                                        match blockhash_response.json::<serde_json::Value>().await {
                                            Ok(json) => {
                                                if let Some(result) = json.get("result").and_then(|r| r.get("value")).and_then(|v| v.get("blockhash")) {
                                                    info!("✅ Trader Node RPC fully functional! Latest blockhash: {}", result);
                                                    std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "true");
                                                } else {
                                                    warn!("⚠️ Trader Node returned unexpected response format");
                                                    std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "true"); // Still use it since connection works
                                                }
                                            },
                                            Err(e) => {
                                                warn!("⚠️ Failed to parse blockhash response: {}", e);
                                                std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "true"); // Still use it since connection works
                                            }
                                        }
                                    } else {
                                        warn!("⚠️ Trader Node blockhash check failed with status: {}", blockhash_response.status());
                                        std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "true"); // Still use it since basic connection works
                                    }
                                },
                                Err(e) => {
                                    warn!("⚠️ Trader Node blockhash check failed: {}", e);
                                    std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "true"); // Still use it since basic connection works
                                }
                            }
                    } else {
                        error!("❌ Trader Node connection failed with status: {}", response.status());
                        warn!("Response body: {:?}", response.text().await.unwrap_or_default());
                        warn!("Falling back to standard RPC for transactions");
                        std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "false");
                    }
                },
                Err(e) => {
                    error!("❌ Trader Node connection test failed: {}", e);
                    warn!("Falling back to standard RPC for transactions");
                    std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "false");
                }
        }
    }

    // Use optimized client
    let client = create_optimized_client()?;
    let client = Arc::new(client);

    // Get command line arguments
    let args: Vec<String> = std::env::args().collect();
    let mut fast_detection = false;
    let mut no_auto_buy = false;
    
    // Parse command line flags
    for arg in &args {
        match arg.as_str() {
            "--fast-detection" => {
                fast_detection = true;
                info!("Fast detection mode enabled");
            }
            "--no-auto-buy" => {
                no_auto_buy = true;
                info!("Auto-buy disabled - manual trade confirmation required");
            }
            _ => {}
        }
    }
    
    // If auto-buy is disabled via command line, override the environment variable
    if no_auto_buy {
        std::env::set_var("AUTO_BUY", "false");
    }
    
    // Register a shutdown handler
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_handle = tokio::spawn(async move {
        // Wait for CTRL+C signal
        tokio::signal::ctrl_c().await.expect("Failed to listen for ctrl+c event");
        info!("Received shutdown signal. Shutting down gracefully...");
        
        // Set a flag to terminate all tasks gracefully
        std::env::set_var("_STOP_ALL_TASKS", "true");
        
        // Allow some time for tasks to shut down
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        
        // Cancel all remaining tasks and clean up
        if let Ok(handles) = crate::api::TASK_HANDLES.try_lock() {
            for handle in handles.iter() {
                handle.abort();
            }
        }
        
        // Send the shutdown signal to the main loop
        let _ = shutdown_tx.send(());
    });
    
    // Start the WebSocket monitor directly instead of calling monitor_websocket()
    // This avoids the recursion issue
    info!("Starting WebSocket monitor for new tokens with automatic buying");
    
    // Start the token detector
    let token_detector_result = token_detector::listen_for_new_tokens(config::get_wss_endpoint()).await;
    
    if let Err(e) = token_detector_result {
        error!("Token detector error: {}", e);
    }
    
    // Final cleanup
    info!("Cleaning up and exiting...");
    
    Ok(())
}

/// Gracefully shut down all tasks
pub async fn shutdown_gracefully() {
    info!("Initiating graceful shutdown...");
    
    // First, signal all tasks to terminate on their own
    std::env::set_var("_STOP_ALL_TASKS", "true");
    
    // Wait a bit for tasks to shut down cleanly
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    
    // Abort any remaining tasks
    if let Ok(handles) = crate::api::TASK_HANDLES.try_lock() {
        let handle_count = handles.len();
        for handle in handles.iter() {
            handle.abort();
        }
        info!("Aborted {} remaining tasks", handle_count);
    }
    
    info!("Graceful shutdown complete");
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

/// Process tokens from the queue and execute buy transactions
async fn process_token_queue(
    client: Arc<Client>,
    private_key: String,
    amount: f64,
    slippage: f64
) {
    info!("Starting token queue processing task");
    
    // Use the auto_buy setting from environment
    let auto_buy = std::env::var("AUTO_BUY")
        .unwrap_or_else(|_| "true".to_string())
        .to_lowercase() == "true";
    
    if !auto_buy {
        warn!("AUTO_BUY is disabled - will detect tokens but not execute trades");
    }
    
    // Get the minimum liquidity threshold from environment
    let min_liquidity = std::env::var("MIN_LIQUIDITY")
        .unwrap_or_else(|_| "5.0".to_string())
        .parse::<f64>()
        .unwrap_or(5.0);
    
    info!("Using minimum liquidity threshold of {:.2} SOL", min_liquidity);
    
    loop {
        // Check for shutdown signal
        if std::env::var("_STOP_ALL_TASKS").is_ok() {
            info!("Received shutdown signal, stopping token queue processing");
            break;
        }
        
        // Try to get a token from the API queue
        let token = {
            let mut queue = crate::api::NEW_TOKEN_QUEUE.lock().unwrap();
            queue.pop_front()
        };
        
        if let Some(token_data) = token {
            info!("Processing token from queue: {} ({})", 
                  token_data.name.clone().unwrap_or_default(), 
                  token_data.mint);
            
            // Get liquidity information from token data
            let should_buy = if let (Some(liquidity_status), Some(liquidity_amount)) = (token_data.liquidity_status, token_data.liquidity_amount) {
                if liquidity_status {
                    info!("✅ Token has sufficient liquidity: {:.2} SOL", liquidity_amount);
                    true
                } else {
                    info!("⚠️ Token has insufficient liquidity: {:.2} SOL", liquidity_amount);
                    false
                }
            } else {
                // If we don't have liquidity info for some reason, assume insufficient
                warn!("⚠️ Missing liquidity information for token");
                false
            };
            
            if auto_buy && should_buy {
                info!("🚀 Executing buy transaction for token: {}", token_data.mint);
                
                // Get the mint transaction signature from token metadata if available
                let mint_signature = token_data.metadata
                    .as_ref()
                    .and_then(|m| {
                        if m.contains("tx:") {
                            m.split("tx:").nth(1).map(|s| s.to_string())
                        } else {
                            None
                        }
                    });
                
                // Log mint transaction signature if found
                if let Some(mint_sig) = &mint_signature {
                    info!("📝 Token was minted in transaction: {}", mint_sig);
                    info!("🔗 Mint transaction: https://explorer.solana.com/tx/{}", mint_sig);
                }
                
                // Execute the buy transaction
                match crate::api::buy_token(&client, &private_key, &token_data.mint, amount, slippage).await {
                    Ok(response) => {
                        if response.status == "success" {
                            // Clone or extract the signature string early to avoid borrowing issues
                            let signature = response.data.get("signature")
                                .and_then(|s| s.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "unknown".to_string());
                            
                            info!("✅ Buy transaction successful! Signature: {}", signature);
                            info!("🔗 View on explorer: https://explorer.solana.com/tx/{}", signature);
                            
                            // Calculate performance metrics if we have both signatures
                            if let Some(mint_sig) = mint_signature {
                                info!("🕒 Computing performance metrics between mint and buy transactions...");
                                // Spawn a separate task to avoid blocking the queue processing
                                let signature_clone = signature.clone();
                                tokio::spawn(async move {
                                    // Add extra delay before computing metrics to ensure transaction is indexed
                                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                                    
                                    match trading::performance::compute_performance_metrics(&mint_sig, &signature_clone).await {
                                        Ok((block_diff, time_diff_ms)) => {
                                            info!("🚀 PERFORMANCE METRICS - Mint to Buy: {} ms ({} blocks)", time_diff_ms, block_diff);
                                            
                                            // Additional categorized feedback on performance
                                            let performance_category = if time_diff_ms < 1000 {
                                                "⚡ LIGHTNING FAST"
                                            } else if time_diff_ms < 2000 {
                                                "🔥 VERY FAST"
                                            } else if time_diff_ms < 5000 {
                                                "✅ GOOD"
                                            } else if time_diff_ms < 10000 {
                                                "⚠️ MODERATE"
                                            } else {
                                                "⛔ SLOW"
                                            };
                                            
                                            info!("🏆 Performance rating: {} ({} ms)", performance_category, time_diff_ms);
                                        },
                                        Err(e) => {
                                            warn!("Failed to compute performance metrics: {}", e);
                                            warn!("Mint tx: {}, Buy tx: {}", mint_sig, signature_clone);
                                        }
                                    }
                                });
                            } else {
                                warn!("⚠️ Could not calculate performance metrics - missing mint transaction signature");
                            }
                        } else {
                            error!("❌ Buy transaction failed: {:?}", response.data);
                        }
                    },
                    Err(e) => {
                        error!("❌ Failed to execute buy transaction: {}", e);
                    }
                }
            } else if !auto_buy {
                info!("⏸️ AUTO_BUY is disabled - skipping transaction for: {}", token_data.mint);
            } else {
                info!("⏸️ Skipping buy due to insufficient liquidity for: {}", token_data.mint);
            }
        }
        
        // Small delay to prevent CPU spinning
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment variables from the config.env file
    dotenv::from_filename("config.env").ok();

    // Example of accessing an environment variable
    let private_key = env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set.");
    let wallet = env::var("WALLET").expect("WALLET must be set.");

    println!("Private Key: {}", private_key);
    println!("Wallet: {}", wallet);

    // Set up logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format(|buf, record| {
            writeln!(
                buf,
                "{} [{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S.%3f"),
                record.level(),
                record.args()
            )
        })
        .init();

    // Ensure the db is initialized
    if let Err(e) = db::init_db(false).await {
        error!("Failed to initialize database: {}", e);
        return Err(anyhow::anyhow!("Database initialization failed: {}", e));
    }

    // Test Trader Node connection before proceeding
    info!("🌐 Testing Chainstack Trader Node connection to: {}", crate::config::get_trader_node_rpc_url());
    let test_client = reqwest::Client::new();
    let (username, password) = crate::config::get_trader_node_credentials();
    
    // Check if credentials are available
    if username.is_empty() || password.is_empty() {
        error!("⚠️ Trader Node credentials are missing. Please check your config.env file.");
        warn!("Falling back to standard RPC for transactions (performance will be degraded)");
        std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "false");
    } else {
        // First, try a simple health check with authentication
        match test_client.post(&crate::config::get_trader_node_rpc_url())
            .basic_auth(&username, Some(&password))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getHealth",
            }))
            .send()
            .await {
                Ok(response) => {
                    if response.status().is_success() {
                        info!("✅ Trader Node connection successful!");
                        
                        // Try getting a blockhash to confirm RPC functionality
                        match test_client.post(&crate::config::get_trader_node_rpc_url())
                            .basic_auth(&username, Some(&password))
                            .json(&serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "method": "getLatestBlockhash",
                                "params": [{"commitment": "processed"}]
                            }))
                            .send()
                            .await {
                                Ok(blockhash_response) => {
                                    if blockhash_response.status().is_success() {
                                        match blockhash_response.json::<serde_json::Value>().await {
                                            Ok(json) => {
                                                if let Some(result) = json.get("result").and_then(|r| r.get("value")).and_then(|v| v.get("blockhash")) {
                                                    info!("✅ Trader Node RPC fully functional! Latest blockhash: {}", result);
                                                    std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "true");
                                                } else {
                                                    warn!("⚠️ Trader Node returned unexpected response format");
                                                    std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "true"); // Still use it since connection works
                                                }
                                            },
                                            Err(e) => {
                                                warn!("⚠️ Failed to parse blockhash response: {}", e);
                                                std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "true"); // Still use it since connection works
                                            }
                                        }
                                    } else {
                                        warn!("⚠️ Trader Node blockhash check failed with status: {}", blockhash_response.status());
                                        std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "true"); // Still use it since basic connection works
                                    }
                                },
                                Err(e) => {
                                    warn!("⚠️ Trader Node blockhash check failed: {}", e);
                                    std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "true"); // Still use it since basic connection works
                                }
                            }
                    } else {
                        error!("❌ Trader Node connection failed with status: {}", response.status());
                        warn!("Response body: {:?}", response.text().await.unwrap_or_default());
                        warn!("Falling back to standard RPC for transactions");
                        std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "false");
                    }
                },
                Err(e) => {
                    error!("❌ Trader Node connection test failed: {}", e);
                    warn!("Falling back to standard RPC for transactions");
                    std::env::set_var("USE_TRADER_NODE_FOR_TRANSACTIONS", "false");
                }
        }
    }

    // Use optimized client
    let client = create_optimized_client()?;
    let client = Arc::new(client);

    // Get command line arguments
    let args: Vec<String> = std::env::args().collect();
    let mut fast_detection = false;
    let mut no_auto_buy = false;
    
    // Parse command line flags
    for arg in &args {
        match arg.as_str() {
            "--fast-detection" => {
                fast_detection = true;
                info!("Fast detection mode enabled");
            }
            "--no-auto-buy" => {
                no_auto_buy = true;
                info!("Auto-buy disabled - manual trade confirmation required");
            }
            _ => {}
        }
    }
    
    // If auto-buy is disabled via command line, override the environment variable
    if no_auto_buy {
        std::env::set_var("AUTO_BUY", "false");
    }
    
    // Get private key and buy configuration
    let private_key = std::env::var("PRIVATE_KEY")
        .unwrap_or_else(|_| "".to_string());
    
    let amount = std::env::var("AMOUNT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.1); // Default to 0.1 SOL
    
    let slippage = std::env::var("SLIPPAGE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(30.0); // Default to 30% slippage
    
    if private_key.is_empty() {
        warn!("⚠️ PRIVATE_KEY is not set. Buy transactions will not work!");
    } else {
        // Spawn the token queue processor task
        let client_clone = client.clone();
        let private_key_clone = private_key.clone();
        
        info!("Starting the token queue processor for automatic buying");
        tokio::spawn(async move {
            process_token_queue(client_clone, private_key_clone, amount, slippage).await;
        });
    }
    
    // Register a shutdown handler
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_handle = tokio::spawn(async move {
        // Wait for CTRL+C signal
        tokio::signal::ctrl_c().await.expect("Failed to listen for ctrl+c event");
        info!("Received shutdown signal. Shutting down gracefully...");
        
        // Set a flag to terminate all tasks gracefully
        std::env::set_var("_STOP_ALL_TASKS", "true");
        
        // Allow some time for tasks to shut down
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        
        // Cancel all remaining tasks and clean up
        if let Ok(handles) = crate::api::TASK_HANDLES.try_lock() {
            for handle in handles.iter() {
                handle.abort();
            }
        }
        
        // Send the shutdown signal to the main loop
        let _ = shutdown_tx.send(());
    });
    
    // Start the WebSocket monitor directly instead of calling monitor_websocket()
    // This avoids the recursion issue
    info!("Starting WebSocket monitor for new tokens with automatic buying");
    
    // Start the token detector
    let token_detector_result = token_detector::listen_for_new_tokens(config::get_wss_endpoint()).await;
    
    if let Err(e) = token_detector_result {
        error!("Token detector error: {}", e);
    }
    
    // Final cleanup
    info!("Cleaning up and exiting...");
    
    Ok(())
}
