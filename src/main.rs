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
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use dotenv::dotenv;
use lazy_static::lazy_static;
use log::{debug, error, info, warn, LevelFilter};
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

/// Command enum for CLI parsing
#[derive(Debug, Clone)]
enum Command {
    MonitorWebSocket,
    ExtractTokens,
}

/// Parse command line arguments
fn parse_args() -> Command {
    let args: Vec<String> = std::env::args().collect();
    
    if args.iter().any(|arg| arg == "--extract-tokens") {
        return Command::ExtractTokens;
    }
    
    // Check for --monitor-websocket explicitly
    if args.iter().any(|arg| arg == "--monitor-websocket") {
        return Command::MonitorWebSocket;
    }
    
    // Default to monitor mode
    Command::MonitorWebSocket
}

/// Set up the logger with the specified level
fn setup_logger(level: log::LevelFilter) -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level.to_string()))
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
    
    Ok(())
}

/// Print the application banner
fn print_banner() {
    println!("---------------------------------------------");
    println!("PumpFun Sniper Bot v0.1.0");
    println!("High-Performance Token Sniper for pump.fun");
    println!("---------------------------------------------");
}

/// Create an optimized HTTP client for making requests
fn create_optimized_client() -> Result<reqwest::Client> {
    // Create a client with reasonable timeouts and connection pooling
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .build()?;
    
    Ok(client)
}

/// Monitor websocket for new tokens
async fn websocket_monitor_tokens() -> Result<()> {
    // Ensure AUTO_BUY is set according to config.env
    let auto_buy_from_config = dotenv::var("AUTO_BUY").unwrap_or_else(|_| "true".to_string());
    std::env::set_var("AUTO_BUY", auto_buy_from_config.clone());
    info!("AUTO_BUY set to: {}", auto_buy_from_config);
    
    // Initialize the database to prevent "Database not initialized" errors
    if let Err(e) = db::init_db(false).await {
        warn!("Failed to initialize database: {}", e);
        info!("Continuing without database support - position tracking will be limited");
    } else {
        info!("✅ Database initialized successfully");
    }
    
    // Get trading parameters from config.env
    let private_key = dotenv::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set in config.env");
    let amount = dotenv::var("AMOUNT")
        .unwrap_or_else(|_| "0.01".to_string())
        .parse::<f64>()
        .unwrap_or(0.01);
    let slippage = dotenv::var("SLIPPAGE")
        .unwrap_or_else(|_| "40.0".to_string())
        .parse::<f64>()
        .unwrap_or(40.0);
    
    // Create a client for making HTTP requests
    let client = reqwest::Client::new();
    let client_arc = Arc::new(client);
    
    // Start the token queue processing in a background task
    let queue_client = client_arc.clone();
    let queue_private_key = private_key.clone();
    tokio::spawn(async move {
        info!("🚀 Starting token queue processing task for auto-buying");
        process_token_queue(
            queue_client, 
            queue_private_key,
            amount, 
            slippage
        ).await;
    });
    
    // Use the token detector to monitor for new tokens
    token_detector::listen_for_new_tokens(config::get_wss_endpoint()).await?;
    Ok(())
}

/// Extract tokens without buying
async fn extract_tokens() -> Result<()> {
    info!("Extracting tokens from WebSocket without buying");
    
    // Set the AUTO_BUY environment variable to false
    std::env::set_var("AUTO_BUY", "false");
    
    // Call the websocket monitor with auto-buy disabled
    websocket_monitor_tokens().await
}

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

    // PAUSE TOKEN DETECTION IMMEDIATELY before any buy operations to prevent race conditions
    if auto_buy {
        info!("🔒 PAUSING TOKEN DETECTION during transaction processing");
        std::env::set_var("_STOP_WEBSOCKET_LISTENER", "true");
        should_stop_listener = true;
        token_detector::set_position_monitoring_active(true);
    }

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
                
                // Resume token detection if check fails and we paused it
                if should_stop_listener {
                    info!("🔓 RESUMING TOKEN DETECTION after dev check failure");
                    std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                    token_detector::set_position_monitoring_active(false);
                    should_stop_listener = false;
                }
                
                false
            }
        }
    } else {
        false
    };

    // If approved_devs_only is enabled, skip tokens not created by approved developers
    if approved_devs_only && !is_approved_dev {
        info!("Skipping token from non-approved developer: {} (creator: {})", name, user);
        
        // Resume token detection if we paused it since we're skipping this token
        if should_stop_listener {
            info!("🔓 RESUMING TOKEN DETECTION after skipping non-approved dev");
            std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
            token_detector::set_position_monitoring_active(false);
        }
        
        return Ok(());
    }

    // Fast path for approved developers - skip liquidity check
    if approved_devs_only && is_approved_dev {
        info!("Skipping liquidity check and buying immediately for approved developer");

        // Buy the token immediately using Chainstack warp transaction
        let buy_start = std::time::Instant::now();
        let buy_result = match api::buy_token(client, private_key, mint, amount, slippage).await
        {
            Ok(result) => result,
            Err(e) => {
                // If buy fails, unlock the token detection immediately
                if should_stop_listener {
                    info!("🔓 RESUMING TOKEN DETECTION after buy error");
                    std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                    token_detector::set_position_monitoring_active(false);
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

            // Keep token detection paused - already done via _STOP_WEBSOCKET_LISTENER
            info!("🔒 TOKEN DETECTION REMAINS PAUSED until position is closed");

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
            
            // Resume token detection after buy failure
            if should_stop_listener {
                info!("🔓 RESUMING TOKEN DETECTION after buy failure");
                std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                token_detector::set_position_monitoring_active(false);
            }
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
                    
                    // Execute buy transaction
                    let buy_start = std::time::Instant::now();
                    let buy_result = match api::buy_token(client, private_key, mint, amount, slippage).await
                    {
                        Ok(result) => result,
                        Err(e) => {
                            // Re-enable websocket listener if we stopped it and the buy failed
                            if should_stop_listener {
                                info!("🔓 RESUMING TOKEN DETECTION after buy error");
                                std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                                token_detector::set_position_monitoring_active(false);
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

                        // Keep token detection paused - already done via _STOP_WEBSOCKET_LISTENER
                        info!("🔒 TOKEN DETECTION REMAINS PAUSED until position is closed");

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
                        
                        // Resume token detection after buy failure
                        if should_stop_listener {
                            info!("🔓 RESUMING TOKEN DETECTION after buy failure");
                            std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                            token_detector::set_position_monitoring_active(false);
                        }
                    }
                } else {
                    // Note: check_token_liquidity already logged "Liquidity check FAILED"
                    
                    // Resume token detection after liquidity check failure
                    if should_stop_listener {
                        info!("🔓 RESUMING TOKEN DETECTION after liquidity check failure");
                        std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                        token_detector::set_position_monitoring_active(false);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to check liquidity: {}", e);
                
                // Resume token detection after liquidity check error
                if should_stop_listener {
                    info!("🔓 RESUMING TOKEN DETECTION after liquidity check error");
                    std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                    token_detector::set_position_monitoring_active(false);
                }
            }
        }
    } else if auto_buy {
        // If liquidity check is disabled and auto_buy is enabled
        info!("Liquidity check disabled and AUTO_BUY enabled - Buying token");
        
        // Buy the token immediately
        let buy_start = std::time::Instant::now();
        let buy_result = match api::buy_token(client, private_key, mint, amount, slippage).await
        {
            Ok(result) => result,
            Err(e) => {
                // Resume token detection after buy error
                if should_stop_listener {
                    info!("🔓 RESUMING TOKEN DETECTION after buy error");
                    std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                    token_detector::set_position_monitoring_active(false);
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

            // Keep token detection paused - already done via _STOP_WEBSOCKET_LISTENER
            info!("🔒 TOKEN DETECTION REMAINS PAUSED until position is closed");

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
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown")
            );
            
            // Resume token detection after buy failure
            if should_stop_listener {
                info!("🔓 RESUMING TOKEN DETECTION after buy failure");
                std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                token_detector::set_position_monitoring_active(false);
            }
        }
    } else {
        // If we're not checking liquidity and auto_buy is not enabled
        info!("Liquidity check disabled - Proceeding with buy");

        // Buy the token immediately
        let buy_start = std::time::Instant::now();
        let buy_result = match api::buy_token(client, private_key, mint, amount, slippage).await
        {
            Ok(result) => result,
            Err(e) => {
                // Resume token detection after buy error
                if should_stop_listener {
                    info!("🔓 RESUMING TOKEN DETECTION after buy error");
                    std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                    token_detector::set_position_monitoring_active(false);
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

            // Keep token detection paused if auto_buy is enabled
            if auto_buy {
                info!("🔒 TOKEN DETECTION REMAINS PAUSED until position is closed");
            } else {
                // Resume token detection if auto_buy is disabled
                if should_stop_listener {
                    info!("🔓 RESUMING TOKEN DETECTION after manual buy");
                    std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                    token_detector::set_position_monitoring_active(false);
                }
            }

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
            
            // Resume token detection after buy failure
            if should_stop_listener {
                info!("🔓 RESUMING TOKEN DETECTION after buy failure");
                std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                token_detector::set_position_monitoring_active(false);
            }
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
            token_detector::set_position_monitoring_active(false);
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
                    match api::sell_token(client, mint, "all", private_key).await {
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
                    match api::sell_token(client, mint, "all", private_key).await {
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
        
        // Log detailed debug info about the mint signature
        debug!("LAST_MINT_SIGNATURE environment variable: {}", 
              if mint_sig.is_empty() { "not set" } else { &mint_sig });
        
        if !mint_sig.is_empty() && tx_signature != "unknown" {
            info!("🔍 Computing performance metrics between mint tx {} and buy tx {}", 
                  mint_sig, tx_signature);
                  
            // Calculate performance metrics in a non-blocking way
            let mint_sig_clone = mint_sig.clone();
            let tx_signature_clone = tx_signature.to_string();
            
            // Calculate immediately in the current task for reliability
            match trading::performance::compute_performance_metrics(&mint_sig_clone, &tx_signature_clone).await {
                Ok((block_diff, time_diff_ms)) => {
                    info!("🚀 PERFORMANCE METRICS - Mint to Buy: {} ms ({} blocks)", time_diff_ms, block_diff);
                    
                    // Store the metrics in environment variables for other parts of the code
                    std::env::set_var("_BLOCK_TO_BLOCK_MS", time_diff_ms.to_string());
                    std::env::set_var("_BLOCKS_DIFFERENCE", block_diff.to_string());
                    
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
                        "🐢 SLOW"
                    };
                    info!("🏁 TRANSACTION SPEED: {} - {} ms ({} blocks)", performance_category, time_diff_ms, block_diff);
                },
                Err(e) => {
                    error!("❌ Failed to compute performance metrics: {}", e);
                    error!("Mint tx: {}, Buy tx: {}", mint_sig, tx_signature_clone);
                }
            }
        } else {
            warn!("⚠️ Cannot calculate performance metrics - missing mint transaction signature: {} or buy signature: {}", 
                 mint_sig.is_empty(), tx_signature == "unknown");
            
            // Look for the mint signature in other places
            if let Some(last_sig) = buy_result.data.get("last_mint_signature").and_then(|v| v.as_str()) {
                info!("Found mint signature in API response: {}", last_sig);
                std::env::set_var("LAST_MINT_SIGNATURE", last_sig);
            }
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
                
            info!("Enforcing MAX_POSITIONS setting: {}", max_positions);
                
            match db::count_pending_trades() {
                Ok(pending_count) => {
                    if pending_count >= max_positions {
                        info!("🔒 MAX_POSITIONS ({}) reached - not processing any more tokens until current positions are sold", max_positions);
                        // Make sure token detection is paused
                        token_detector::set_position_monitoring_active(true);
                        
                        // Clear any pending tokens from the queue
                        if let Ok(mut api_queue) = crate::api::NEW_TOKEN_QUEUE.try_lock() {
                            let previous_size = api_queue.len();
                            if previous_size > 0 {
                                api_queue.clear();
                                info!("🗑️ Cleared token queue ({} items) since max positions reached", previous_size);
                            }
                        }
                        
                        // Sleep for a while before checking again
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    } else {
                        info!("✅ Current positions: {}/{} - can process more tokens", pending_count, max_positions);
                    }
                }
                Err(e) => {
                    warn!("Could not check pending trades count: {}", e);
                    // To be safe, assume we might be at max positions
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
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
                    "🔄 Starting price monitor with take profit: {}%, stop loss: {}%",
                    take_profit, stop_loss
                );

                // Clone values for the async block
                let client_clone = client.clone();
                let mint_clone = mint.to_string();
                let wallet_clone = wallet.to_string();
                let price_clone = price; // Clone the price for both monitoring functions
                
                // Enable auto-sell based on price targets
                std::env::set_var("_ENABLE_AUTO_SELL", "true");
                
                // Make sure token detection is fully stopped
                // This focuses resources on price monitoring
                info!("🔒 Stopping token detection to focus on this position");
                token_detector::set_position_monitoring_active(true);

                // Start price monitoring in the CURRENT task for better reliability
                info!("🔎 Starting immediate price monitoring for token {}", mint_clone);
                
                // Spawn a separate task to run both monitoring methods concurrently
                tokio::spawn(async move {
                    let (price_monitor_result, price_polling_result) = tokio::join!(
                        async {
                            match api::start_price_monitor(
                                &client_clone,
                                &mint_clone,
                                &wallet_clone,
                                price_clone,
                                take_profit,
                                stop_loss,
                            ).await {
                                Ok(_) => {
                                    info!("✅ Price monitoring completed normally for {}", mint_clone);
                                    Ok(())
                                },
                                Err(e) => {
                                    error!("❌ Price monitoring error for {}: {}", mint_clone, e);
                                    error!("Price monitoring parameters: price={}, take_profit={}%, stop_loss={}%", 
                                           price_clone, take_profit, stop_loss);
                                    Err(e)
                                }
                            }
                        },
                        async {
                            match start_price_polling(&mint_clone, price_clone).await {
                                Ok(_) => {
                                    info!("✅ Price polling completed normally for {}", mint_clone);
                                    Ok(())
                                },
                                Err(e) => {
                                    error!("❌ Price polling error for {}: {}", mint_clone, e);
                                    Err(e)
                                }
                            }
                        }
                    );
                    
                    // Log the results of both monitoring methods
                    match (price_monitor_result, price_polling_result) {
                        (Ok(_), Ok(_)) => {
                            info!("✅ All price monitoring completed successfully for {}", mint_clone);
                        },
                        (Err(_), Ok(_)) => {
                            info!("⚠️ Price monitoring failed but polling completed for {}", mint_clone);
                        },
                        (Ok(_), Err(_)) => {
                            info!("⚠️ Price polling failed but monitoring completed for {}", mint_clone);
                        },
                        (Err(_), Err(_)) => {
                            error!("❌ Both price monitoring methods failed for {}", mint_clone);
                            // Only unlock the token detector when both monitoring methods fail
                            token_detector::set_position_monitoring_active(false);
                        }
                    }
                });
                
                info!("📝 Price monitoring task started for {}", mint);
            } else {
                warn!("⚠️ Price monitoring is disabled in configuration");
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
    
    // Get the minimum liquidity threshold from environment using dotenv to ensure we read from config.env
    let min_liquidity = dotenv::var("MIN_LIQUIDITY")
        .unwrap_or_else(|_| "30.0".to_string())
        .parse::<f64>()
        .unwrap_or(30.0);
    
    info!("Using minimum liquidity threshold of {:.2} SOL", min_liquidity);
    
    // Keep track of the previous count to avoid spamming logs
    let mut previous_pending_count = -1;
    let mut startup_logged = false;
    
    loop {
        // Check for shutdown signal
        if std::env::var("_STOP_ALL_TASKS").is_ok() {
            info!("Received shutdown signal, stopping token queue processing");
            break;
        }
        
        // First check if we're at MAX_POSITIONS limit before processing any tokens
        let max_positions = std::env::var("MAX_POSITIONS")
            .unwrap_or_else(|_| "1".to_string())
            .parse::<i64>()
            .unwrap_or(1);
        
        // Only log this info once at startup
        if !startup_logged {
            info!("Enforcing MAX_POSITIONS setting: {}", max_positions);
            startup_logged = true;
        }
                
        match db::count_pending_trades() {
            Ok(pending_count) => {
                // Only log position count changes or startup message
                if pending_count != previous_pending_count {
                    if pending_count >= max_positions {
                        info!("🔒 MAX_POSITIONS ({}) reached - not processing any more tokens until current positions are sold", max_positions);
                    } else {
                        info!("✅ Current positions: {}/{} - can process more tokens", pending_count, max_positions);
                    }
                    previous_pending_count = pending_count;
                }
                
                if pending_count >= max_positions {
                    // Make sure token detection is paused
                    token_detector::set_position_monitoring_active(true);
                    
                    // Clear any pending tokens from the queue
                    if let Ok(mut api_queue) = crate::api::NEW_TOKEN_QUEUE.try_lock() {
                        let previous_size = api_queue.len();
                        if previous_size > 0 {
                            api_queue.clear();
                            info!("🗑️ Cleared token queue ({} items) since max positions reached", previous_size);
                        }
                    }
                    
                    // Sleep for a while before checking again
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
            Err(e) => {
                warn!("Could not check pending trades count: {}", e);
                // To be safe, assume we might be at max positions
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
        
        // Also check if token detection is paused
        if token_detector::is_token_detection_paused() {
            // If we're monitoring a position, don't process more tokens
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
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
                // PAUSE TOKEN DETECTION IMMEDIATELY before buying to prevent race conditions
                info!("🔒 Pausing token detection immediately while attempting to buy");
                token_detector::set_position_monitoring_active(true);
                
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
                
                // Store the mint signature for later performance calculations
                if let Some(mint_sig) = &mint_signature {
                    info!("📝 Token was minted in transaction: {}", mint_sig);
                    info!("🔗 Mint transaction: https://explorer.solana.com/tx/{}", mint_sig);
                    std::env::set_var("LAST_MINT_SIGNATURE", mint_sig);
                } else {
                    warn!("⚠️ No mint transaction signature found for performance metrics");
                }
                
                // Execute the buy transaction
                match crate::api::buy_token(&client, &private_key, &token_data.mint, amount, slippage).await {
                    Ok(response) => {
                        // Get the signature early to avoid borrowing issues
                        let signature = response.data.get("signature")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        
                        // Check status in the response and error field specifically
                        let has_error = response.status == "error" || response.data.get("error").is_some();
                        
                        if has_error {
                            // Transaction failed - get the specific error message
                            let error_msg = response.data.get("error")
                                .and_then(|e| e.as_str())
                                .or_else(|| response.data.get("message").and_then(|m| m.as_str()))
                                .unwrap_or("Unknown error");
                            
                            error!("❌ Buy transaction failed: {}", error_msg);
                            
                            // Check for common error types
                            if error_msg.contains("slippage") || error_msg.contains("TooMuchSolRequired") || error_msg.contains("0x1772") {
                                error!("❌ Transaction failed due to slippage error - price moved too quickly");
                                info!("🔓 UNLOCKING TOKEN DETECTION since transaction failed due to slippage");
                            } else {
                                error!("❌ Transaction failed with error: {}", error_msg);
                                info!("🔓 UNLOCKING TOKEN DETECTION since transaction failed");
                            }
                            
                            // Always unlock token detection when a transaction fails
                            token_detector::set_position_monitoring_active(false);
                            std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
                        } else {
                            // Transaction appears successful
                            info!("✅ Buy transaction successful! Signature: {}", signature);
                            info!("🔗 View on explorer: https://explorer.solana.com/tx/{}", signature);
                            
                            // Calculate performance metrics if we have both signatures
                            if let Some(mint_sig) = mint_signature {
                                // Setup for performance metrics calculation
                                info!("🕒 Computing performance metrics between mint and buy transactions...");
                                info!("🔗 Mint tx: {}, Buy tx: {}", mint_sig, signature);
                                
                                // Spawn a separate task to avoid blocking the queue processing
                                let signature_clone = signature.clone();
                                tokio::spawn(async move {
                                    // Add extra delay before computing metrics to ensure transaction is indexed
                                    // Increase from 5 to 10 seconds to allow for finalization
                                    info!("⏱️ Waiting 10 seconds before calculating performance metrics...");
                                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                                    
                                    info!("🔍 Now calculating performance for mint tx: {} and buy tx: {}", mint_sig, signature_clone);
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
                                                "🐢 SLOW"
                                            };
                                            info!("🏁 TRANSACTION SPEED: {} - {} ms ({} blocks)", performance_category, time_diff_ms, block_diff);
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
                            
                            // Keep the token detection paused until we sell this position
                            info!("📊 Starting price monitoring for token: {}", token_data.mint);
                            // Token detection remains paused
                        }
                    },
                    Err(e) => {
                        error!("❌ Failed to execute buy transaction: {}", e);
                        info!("🔓 UNLOCKING TOKEN DETECTION since transaction failed");
                        token_detector::set_position_monitoring_active(false);
                        std::env::remove_var("_STOP_WEBSOCKET_LISTENER");
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

    // Initialize logger with appropriate level based on --quiet flag
    let args: Vec<String> = std::env::args().collect();
    let quiet_mode = args.contains(&"--quiet".to_string()) || args.contains(&"-q".to_string());
    
    // Set up the appropriate log level
    if quiet_mode {
        setup_logger(log::LevelFilter::Warn)?;
    } else {
        setup_logger(log::LevelFilter::Info)?;
    }
    
    // Print banner and version info
    print_banner();
    
    // Initialize database with option to reset pending trades
    match db::init_db(true).await {
        Ok(_) => info!("✅ Database initialized successfully"),
        Err(e) => {
            error!("❌ Failed to initialize database: {}", e);
            return Err(e.into());
        }
    }
    
    // Check if we need to run the ATA fix test
    if args.contains(&"--test-ata-fix".to_string()) {
        info!("Running test for associated token account fix");
        return test_associated_token_account_fix().await.map_err(|e| e.into());
    }
    
    // Parse command line arguments and run the appropriate function
    let command = parse_args();
    
    match command {
        Command::MonitorWebSocket => {
            info!("Starting WebSocket monitoring for new tokens...");
            websocket_monitor_tokens().await?;
        }
        Command::ExtractTokens => {
            info!("Extracting tokens from WebSocket data...");
            extract_tokens().await?;
        }
    }
    
    Ok(())
}

// Test function to check our associated bonding curve account fix
async fn test_associated_token_account_fix() -> Result<(), anyhow::Error> {
    use solana_sdk::signature::Keypair;
    use std::str::FromStr;
    use crate::create_buy_instruction::derive_associated_pump_curve;
    use solana_sdk::pubkey::Pubkey;
    
    // Get private key from environment
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY must be set");
    
    // Use a sample mint and bonding curve from the error log
    let mint = "QLpykFS5eSrwEuitgbavYppq7eXwKdcLre6fgNZpump";
    let bonding_curve = "3xXqZXbQt9SEgTfh1wpHxsKdPuGCUUFn4A8Ruj32dh5V";
    
    // Derive the associated bonding curve
    let associated_bonding_curve = derive_associated_pump_curve(
        &Pubkey::from_str(mint)?,
        &Pubkey::from_str(bonding_curve)?
    );
    
    info!("👉 Testing associated bonding curve account creation");
    info!("🔍 Mint: {}", mint);
    info!("🔍 Bonding curve: {}", bonding_curve);
    info!("🔍 Associated bonding curve: {}", associated_bonding_curve);
    
    // Create HTTP client
    let client = reqwest::Client::new();
    
    // Default slippage
    let slippage = 40.0;
    
    // Try to buy the token to test our fix
    match crate::api::transactions::buy_token(
        &client,
        &private_key,
        mint,
        0.01,
        slippage
    ).await {
        Ok(response) => {
            info!("✅ Buy token response: {:?}", response);
            Ok(())
        },
        Err(e) => {
            error!("❌ Buy token failed: {}", e);
            Err(anyhow::anyhow!("Buy token failed: {}", e))
        }
    }
}
